//! CR 730 (Merging with Permanents) + CR 702.140 (Mutate).
//!
//! Phase 1 of the Mutate keyword. A mutating creature spell that resolves with a
//! legal target does NOT enter the battlefield (CR 702.140c); instead it merges
//! with the target creature, becoming one object represented by more than one
//! card or token (CR 730.2). This module owns the merge primitive
//! ([`merge_object_onto`]), the leave-the-battlefield split (CR 730.3,
//! [`split_merged_permanent_on_leave`]), and the top/bottom choice handler
//! ([`handle_mutate_merge_choice`]).
//!
//! Merge identity (BINDING review resolution S4):
//!   * The surviving battlefield object is ALWAYS the target creature's
//!     `ObjectId` (CR 730.2c continuity). The merged permanent "is the same
//!     object that it was before."
//!   * Over/under only selects which component supplies copiable characteristics
//!     (CR 730.2a) — recorded as the topmost element of
//!     `GameObject::merged_components` (convention: index `[0]` is topmost).
//!   * The merged permanent always has the UNION of every component's abilities
//!     (CR 702.140e); its other characteristics come from the topmost component
//!     (CR 730.2a).
//!   * Each component retains its ORIGINAL owner so the CR 730.3 leave-split
//!     routes each card/token to the correct player's zone.
//!
//! Multi-instance stacking (CR 730.2) IS supported: mutating onto an
//! already-merged permanent extends its component stack, and the merged
//! permanent's layer-1 copy effect is re-derived from the full stack each time.
//!
//! Face-down components (partial — CR 730.2e): handled now is the merged
//! permanent's status following its topmost component (a face-down topmost keeps
//! the survivor face down; a face-up topmost over a face-down component does NOT
//! count as turning it face up, so no `TurnedFaceUp` event), and the suppression
//! of a non-topmost face-down component's listed characteristics from the
//! copiable-values union (a cloaked/disguised component's `base_keywords = [Ward]`
//! is not inherited by a face-up merged permanent — CR 708.2a + CR 730.2e). Still
//! deferred: CR 730.2f turn-up of a face-down merged permanent; full CR 702.140e
//! reveal of a formerly-face-down bottom component's real abilities on a
//! mutate-over; the split (CR 730.3) of a face-down merge survivor (the survivor's
//! `base_*` holds the stale 2/2 face-down profile rather than the component's real
//! card characteristics); and the extreme corner where a sticker placed on a
//! face-down permanent is wiped by the Layer-1b re-seed (rules-correct per CR
//! 708.2a — a face-down object has only its listed characteristics). Note:
//! `apply_face_down` does not normalize `base_loyalty`/`base_defense`; that is
//! pre-existing Step-1 reset behavior, faithfully preserved by the shared
//! `seed_live_characteristics_from_base` helper.
//!
//! Deferred: copy effects targeting a merged permanent, DFC components, full CR
//! 702.140d downstream reflexive effects, and the CR 730.3a graveyard/library
//! arrange-order UI (a deterministic order is used).

use std::collections::HashSet;
use std::sync::Arc;

use crate::game::game_object::{DisplayIdentity, GameObject, ObjectIncarnationRefExt};
use crate::game::printed_cards::intrinsic_copiable_values;
use crate::game::targeting;
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{
    ContinuousModification, CopiableValues, Duration, Effect, LibraryPosition, LibraryShuffleMode,
    ResolvedAbility, TargetFilter, ThisWayCause,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef, TrackedSetId};
use crate::types::proposed_event::AppliedReplacementKey;
use crate::types::zones::Zone;

/// CR 702.140c + CR 730.2a: Which side of the target creature the mutating
/// spell is placed on. The choice selects the topmost component (copiable
/// characteristics, CR 730.2a); it never changes the merged permanent's
/// `ObjectId` (CR 730.2c). A typed enum rather than a `bool` so call sites are
/// self-documenting and exhaustively matched.
///
/// Serializes as the plain variant string ("Top" / "Bottom") so the frontend
/// `GameAction::ChooseMutateMergeSide` payload is `{ side: "Top" | "Bottom" }`,
/// parallel to the sibling `ChooseTopOrBottom { top: bool }`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum MergeSide {
    /// The mutating spell is placed on TOP of the target creature — the spell's
    /// card/token supplies the copiable characteristics.
    Top,
    /// The mutating spell is placed UNDER the target creature — the target keeps
    /// its own copiable characteristics.
    Bottom,
}

/// CR 702.140c + CR 730.2: Merge `merging_id` (the resolving mutate spell's
/// card/token) onto `target_id` (the surviving battlefield creature) on the
/// chosen `side`.
///
/// The target keeps its `ObjectId` (CR 730.2c); `side` only sets the topmost
/// component. The merged permanent gains the union of all components' abilities
/// (CR 702.140e) and the topmost component's other characteristics (CR 730.2a)
/// through the existing layer-1 `CopyValues` machinery. The permanent is NOT
/// considered to have entered the battlefield (CR 730.2b/c), so no ETB triggers
/// fire. Emits `GameEvent::Mutated`.
///
/// CR 730.2 multi-instance stacking: if `target_id` is already a merged
/// permanent, `merging_id` extends its component stack (over or under the whole
/// stack per `side`); the identity is re-derived from the full stack. The
/// derived copy effect is rebuilt from the component list on each merge.
/// `merging_id`'s `GameObject` is retained in `state.objects` as a component (it
/// has left the stack in `stack::resolve_top`) so
/// [`split_merged_permanent_on_leave`] can restore it.
pub fn merge_object_onto(
    state: &mut GameState,
    merging_id: ObjectId,
    target_id: ObjectId,
    side: MergeSide,
    events: &mut Vec<GameEvent>,
) {
    // Resolve the merging spell's controller for the event payload before any
    // mutation (the component object survives, so this stays valid).
    let controller = state
        .objects
        .get(&merging_id)
        .map(|o| o.controller)
        .or_else(|| state.objects.get(&target_id).map(|o| o.controller))
        .expect("merge components exist");

    // CR 730.2b/c: the merging card leaves the stack and becomes part of the
    // battlefield object identified by `target_id`. It is not itself in any zone
    // list; mark its zone as Battlefield so component queries see a consistent
    // location. The stack entry was already popped in `stack::resolve_top`. The
    // stack-only `mutate_form` marker is cleared — it is now a component.
    crate::game::zones::absorb_component(state, merging_id, None);
    if let Some(merging) = state.objects.get_mut(&merging_id) {
        merging.mutate_form = None;
        // CR 730.2b/c + CR 614.1d: a merged component never enters the
        // battlefield, so a cast-this-way enters-tapped rider has no entry to
        // modify; it ends with the spell (it is stack-scoped).
        merging.cast_permission_enter_tapped = crate::types::zones::EtbTapState::Unspecified;
    }

    // CR 730.2 multi-instance stacking: extend the existing stack when
    // `target_id` is already merged; otherwise start from the survivor itself.
    // Convention: element [0] is the topmost component (CR 730.2a).
    let existing: Vec<ObjectId> = state
        .objects
        .get(&target_id)
        .map(|o| o.merged_components.clone())
        .unwrap_or_default();
    let base_order = if existing.is_empty() {
        vec![target_id]
    } else {
        existing
    };
    let ordered: Vec<ObjectId> = match side {
        MergeSide::Top => {
            let mut v = Vec::with_capacity(base_order.len() + 1);
            v.push(merging_id);
            v.extend(base_order);
            v
        }
        MergeSide::Bottom => {
            let mut v = base_order;
            v.push(merging_id);
            v
        }
    };
    let topmost_id = ordered[0];
    // CR 702.140c + CR 730.2a + CR 730.2e + CR 730.2f: every component of a mutate pile can hide
    // its name from now on (a face-down component under a face-up top, the whole pile once turned
    // face down), so each forgets its printing as it merges, the survivor included.
    for component_id in &ordered {
        if let Some(component) = state.objects.get_mut(component_id) {
            component.conceal_printing();
        }
    }
    // CR 730.2e: a merged permanent's face-down status follows its topmost
    // component. Read it before anything below changes any object's face-down status or merge
    // structure (the loop above touches printings only), so the survivor adopts the topmost
    // component's face-down status while merged.
    let topmost_is_face_down = state.objects.get(&topmost_id).is_some_and(|o| o.face_down);

    // Remove any previous mutate copy effect before deriving the new one, so a
    // re-merge where the survivor remains topmost reads the survivor's intrinsic
    // base values rather than the prior merged form.
    remove_merge_layer_effect(state, target_id);

    let Some((values, display)) = merged_copiable_values(state, &ordered, topmost_id) else {
        return;
    };

    if let Some(survivor) = state.objects.get_mut(&target_id) {
        survivor.merged_components = ordered;
        // CR 730.2: mark this pile as Mutate-built so the CR 712.4c transform
        // guard can distinguish it from a Meld survivor (a two-creature mutate
        // also has `merged_components.len() == 2`).
        survivor.merge_kind = Some(crate::game::game_object::MergeKind::Mutate);
        // CR 730.2e: the merged permanent's status follows its topmost component.
        // When a face-down permanent becomes face up as a result of merging, other
        // effects don't count it as turned face up, so no `GameEvent::TurnedFaceUp`
        // is pushed here.
        // CR 730.2e: the survivor keeps its own status as a component.
        if survivor.component_face_down.is_none() && survivor.face_down != topmost_is_face_down {
            survivor.component_face_down = Some(survivor.face_down);
        }
        survivor.face_down = topmost_is_face_down;
    }

    // CR 730.2d: a merged permanent is a token only if its TOPMOST component is a
    // token. The survivor keeps its `ObjectId` (CR 730.2c) but adopts the topmost
    // component's token-ness while merged. Capture the survivor's intrinsic value
    // once (on the first merge that actually overrides it) so the on-leave split
    // can restore it (CR 111.7 cease-to-exist must fire for a token host that had
    // a card mutated on top of it). The all-card case is a no-op (topmost matches).
    let topmost_is_token = state.objects.get(&topmost_id).is_some_and(|o| o.is_token);
    if let Some(survivor) = state.objects.get_mut(&target_id) {
        if survivor.pre_merge_is_token.is_none() && survivor.is_token != topmost_is_token {
            survivor.pre_merge_is_token = Some(survivor.is_token);
        }
        survivor.is_token = topmost_is_token;
    }

    install_merge_layer_effect(state, target_id, controller, values, display);

    // CR 702.140c-d: the mutation is observable. NO ETB (CR 730.2b/c).
    events.push(GameEvent::Mutated {
        merged_id: target_id,
        merging_id,
        controller,
    });
}

/// CR 730.2a + CR 702.140e: Build the copiable values for a merged permanent:
/// the topmost component's copiable characteristics, with the ability sets
/// replaced by the union of every component's intrinsic abilities.
fn merged_copiable_values(
    state: &GameState,
    ordered: &[ObjectId],
    topmost_id: ObjectId,
) -> Option<(CopiableValues, DisplayIdentity)> {
    let topmost = state.objects.get(&topmost_id)?;
    let values = crate::game::layers::compute_current_copiable_values(state, topmost_id)
        .unwrap_or_else(|| intrinsic_copiable_values(topmost));
    merged_copiable_values_from(state, ordered, topmost_id, values)
}

fn merged_copiable_values_from(
    state: &GameState,
    ordered: &[ObjectId],
    topmost_id: ObjectId,
    mut values: CopiableValues,
) -> Option<(CopiableValues, DisplayIdentity)> {
    let topmost = state.objects.get(&topmost_id)?;
    let printed_ref = topmost
        .base_printed_ref
        .clone()
        .or_else(|| topmost.printed_ref.clone());
    // CR 730.2a: the merged permanent presents the topmost component's identity,
    // so its art routing follows the topmost too (a token-topmost mutate carries
    // the token's `display_source`/`token_image_ref`, not just `printed_ref`).
    let display = DisplayIdentity {
        printed_ref,
        ..topmost.display_identity()
    };
    let mut abilities = Vec::new();
    let mut triggers = Vec::new();
    let mut trigger_printed_origins = Vec::new();
    let mut statics = Vec::new();
    let mut replacements = Vec::new();
    let mut keywords: Vec<crate::types::keywords::Keyword> = Vec::new();

    type BaseSets = (
        Arc<Vec<crate::types::ability::AbilityDefinition>>,
        Arc<Vec<crate::types::ability::TriggerDefinition>>,
        Arc<Vec<crate::types::ability::StaticDefinition>>,
        Arc<Vec<crate::types::ability::ReplacementDefinition>>,
        Vec<crate::types::keywords::Keyword>,
    );

    for &component_id in ordered {
        let Some(obj) = state.objects.get(&component_id) else {
            continue;
        };
        // CR 708.2a + CR 730.2e: a non-topmost face-down component carries no card
        // abilities. Morph zeroes its base abilities/triggers/statics/replacements,
        // but a cloaked/disguised component still holds `base_keywords = [Ward {2}]`;
        // a face-up merged permanent must not inherit a buried face-down component's
        // ward (or any other listed face-down characteristic). Skip non-topmost
        // face-down components entirely from the copiable-values union.
        if component_id != topmost_id && obj.face_down {
            continue;
        }
        let (abil, trig, stat, repl, kws): BaseSets = (
            obj.base_abilities.clone(),
            obj.base_trigger_definitions.clone(),
            obj.base_static_definitions.clone(),
            obj.base_replacement_definitions.clone(),
            obj.base_keywords.clone(),
        );
        abilities.extend(abil.iter().cloned());
        triggers.extend(trig.iter().cloned());
        trigger_printed_origins.extend(
            crate::game::printed_cards::base_trigger_printed_origins(obj)
                .iter()
                .cloned(),
        );
        statics.extend(stat.iter().cloned());
        // CR 707.2 / CR 611.2b: merged copiable values are printed/defining
        // characteristics, not runtime locks or target-bound die-exile riders
        // another effect installed on a component. Those defs live in base only
        // for layer-reset survival; exclude them from this copiable-values
        // surface (mirrors `intrinsic_copiable_values`) so a merged permanent
        // does not inherit a component host's runtime replacement.
        replacements.extend(
            repl.iter()
                .filter(|def| !crate::game::printed_cards::is_runtime_non_copiable_replacement(def))
                .cloned(),
        );
        for kw in kws {
            if !keywords.contains(&kw) {
                keywords.push(kw);
            }
        }
    }

    values.abilities = Arc::new(abilities);
    values.trigger_definitions = Arc::new(triggers);
    values.trigger_printed_origins = Arc::new(trigger_printed_origins);
    values.static_definitions = Arc::new(statics);
    values.replacement_definitions = Arc::new(replacements);
    values.keywords = keywords;
    Some((values, display))
}

/// CR 730.2i + CR 730.2a + CR 702.140e: a turned component changes the pile's
/// characteristics and ability union. CR 613.2a + CR 613.7: derive at the merge
/// effect's own layer position, retaining its merge-time timestamp.
pub(crate) fn rederive_merge_layer_effect(state: &mut GameState, survivor_id: ObjectId) {
    use crate::game::game_object::MergeKind;

    let Some(survivor) = state.objects.get(&survivor_id) else {
        return;
    };
    match survivor.merge_kind {
        Some(MergeKind::Mutate) => {}
        Some(MergeKind::Meld | MergeKind::Augment) | None => return,
    }
    let Some(effect_id) = survivor.merge_layer_effect_id else {
        return;
    };
    let ordered = survivor.merged_components.clone();
    let Some(&topmost_id) = ordered.first() else {
        return;
    };
    let Some(effect) = state
        .transient_continuous_effects
        .iter()
        .find(|e| e.id == effect_id)
    else {
        return;
    };
    let Some(values) = crate::game::layers::compute_copiable_values_preceding(
        state,
        topmost_id,
        effect_id,
        effect.timestamp,
    ) else {
        return;
    };
    let Some((values, display)) = merged_copiable_values_from(state, &ordered, topmost_id, values)
    else {
        return;
    };
    if let Some(effect) = state
        .transient_continuous_effects
        .iter_mut()
        .find(|e| e.id == effect_id)
    {
        effect.modifications = vec![ContinuousModification::CopyValues {
            values: Box::new(values),
            display,
        }];
    }
    crate::game::layers::mark_layers_full(state);
}

pub(crate) fn remove_merge_layer_effect(state: &mut GameState, target_id: ObjectId) {
    let effect_id = state
        .objects
        .get(&target_id)
        .and_then(|obj| obj.merge_layer_effect_id);
    let Some(effect_id) = effect_id else {
        return;
    };
    state
        .transient_continuous_effects
        .retain(|effect| effect.id != effect_id);
    if let Some(obj) = state.objects.get_mut(&target_id) {
        obj.merge_layer_effect_id = None;
    }
    crate::game::layers::mark_layers_full(state);
}

pub(crate) fn install_merge_layer_effect(
    state: &mut GameState,
    target_id: ObjectId,
    controller: crate::types::player::PlayerId,
    values: CopiableValues,
    display: DisplayIdentity,
) {
    let effect_id = state.add_transient_continuous_effect(
        target_id,
        controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: target_id },
        vec![ContinuousModification::CopyValues {
            values: Box::new(values),
            display,
        }],
        None,
    );
    if let Some(obj) = state.objects.get_mut(&target_id) {
        obj.merge_layer_effect_id = effect_id;
    }
    crate::game::layers::flush_layers(state);
}

/// CR 730.3: When a merged permanent leaves the battlefield, one permanent
/// leaves and EACH component is put into the appropriate zone. Each component
/// goes to its OWN owner's `dest` zone (S4: components retain their original
/// owner). The surviving object (`merged_id`) is moved by the normal
/// `move_to_zone` flow; this routes the OTHER components.
///
/// Called from the battlefield-exit seam in `zones::move_to_zone` BEFORE the
/// surviving object is moved. Returns immediately for non-merged objects.
///
/// CR 730.3d (replacement propagation): this raw primitive is retained only
/// for callers that intentionally bypass the zone pipeline. Normal gameplay
/// instead enters [`move_merged_permanent_on_leave`], which seeds each component
/// with the merged event's applied set and lets the shared batch pipeline consult
/// only still-applicable component replacements. That preserves the rule's
/// direction that applying a replacement to the merged object applies it to all
/// components, without suppressing the CR 903.9b-c commander exception.
///
/// CR 730.3e (card-vs-token scope): a "card"-scoped redirect (one that applies to
/// a card being put into a zone without also including tokens) follows the
/// survivor's resolved destination through `dest`. When the merged permanent is
/// NOT a token (its survivor is a card, so a card-scoped redirect matches it and
/// redirects the leave event), ALL components — token components included — take
/// `dest` (730.3e first clause: "applies to all components of the merged
/// permanent if it's not a token, including components that are tokens").
///
/// CR 730.3e SECOND clause (the merged permanent's survivor is itself a TOKEN, so
/// a card-scoped redirect does NOT match the survivor): the token survivor + its
/// token components take the pre-replacement default zone (`dest`), while the
/// CARD components are "moved by the replacement effect". This split is driven by
/// `state.merged_card_component_route`, which the pipeline
/// (`zone_pipeline::deliver_replaced_zone_change`) stashes from a SINGLE
/// component-aware consult (one `replace_event` for the card partition — NOT a
/// per-component re-consult, which CR 730.3d forbids, and which would re-burn
/// CR 616.1 ordering). When that override is present, a card component routes to
/// its `card_dest`; a token component routes to its `default_dest` (== `dest`).
/// The override is absent (and every component follows `dest`) for non-token
/// survivors and whenever no card-scoped redirect diverges from the survivor's
/// destination.
///
/// CR 903.9c (merged commander zone redirect): a Hand/Library component is
/// never delivered through this raw fallback in ordinary play. The pipeline
/// batch asks its owner the CR 903.9b replacement question before delivery;
/// accepting places that commander component in Command while the other
/// components receive their appropriate destinations.
///
/// CR 730.3a deferred: the owner's arrange-order choice for graveyard/library
/// destinations is not modeled — components are placed in their stored
/// (topmost-first) order.
pub fn split_merged_permanent_on_leave(
    state: &mut GameState,
    merged_id: ObjectId,
    dest: Zone,
    events: &mut Vec<GameEvent>,
) {
    let Some(survivor) = state.objects.get(&merged_id) else {
        return;
    };
    if survivor.merged_components.is_empty() {
        return;
    }
    let components = survivor.merged_components.clone();
    let survivor_successor = leave_successor(survivor);

    // CR 730.3e (second clause): a TOKEN merged permanent leaving under a
    // card-scoped (`NonToken`) `Moved` redirect routes its CARD components to
    // the redirect destination (`card_dest`) while the token survivor + token
    // components take the pre-replacement default zone (`default_dest`). The
    // pipeline (`deliver_replaced_zone_change`) stashes this from the single
    // component-aware consult; absent it (non-token survivor / no card-scoped
    // divergence — clause 1), every component follows the survivor's `dest`
    // (CR 730.3d). The override only fires when the survivor itself is a token
    // (it lands in `default_dest == dest` via `move_to_zone`).
    let card_route = state.merged_card_component_route;

    for component_id in components {
        // The surviving object itself rides the normal `move_to_zone` flow; only
        // the absorbed (non-survivor) components need explicit routing here.
        if component_id == merged_id {
            continue;
        }
        // CR 730.3 + S4 / CR 730.3e: route each component to ITS OWN owner's
        // destination zone as a NEW object that did not independently leave the
        // battlefield. Under the clause-2 override, a CARD component follows the
        // card-scoped redirect (`card_dest`); a token component (and the default
        // case) follows the survivor's `dest`.
        let component_dest = match card_route {
            Some(route)
                if state
                    .objects
                    .get(&component_id)
                    .is_some_and(|o| !o.is_token) =>
            {
                route.card_dest
            }
            Some(route) => route.default_dest,
            None => dest,
        };
        put_component_into_zone(
            state,
            component_id,
            component_dest,
            Some(survivor_successor),
            events,
        );
    }

    // The surviving object's merge identity is cleared by its own
    // `reset_for_battlefield_exit` during the subsequent `move_to_zone`.
}

/// CR 730.3d: "If multiple replacement effects could be applied to the event
/// of a merged permanent leaving the battlefield or being put into the new
/// zone, applying one of those replacement effects to the object applies it to
/// all components of the object. If the merged permanent is a commander, it
/// may be exempt from this rule; see rules 903.9b-c."
///
/// This is the normal gameplay route for a merged permanent whose approved
/// zone-change event is ready to deliver. Every component, including the
/// survivor, is queued in `move_objects_simultaneously_then`: its inherited
/// `applied` set prevents a replacement already applied to the merged event
/// from applying again, while still letting a component-specific replacement
/// (notably CR 903.9b) consult and pause. The batch owns every pause/restart,
/// so replacement choices cannot clobber one another or expose partial event
/// output. The survivor moves through the ordinary mover; absorbed components
/// retain their `from: None` component-delivery event shape.
///
/// CR 903.9c: when an individual commander component accepts the CR 903.9b
/// Hand/Library replacement, only that component's request changes to Command;
/// noncommander components remain routed to their appropriate zones.
///
/// CR 701.24a + CR 730.3: every component inherits the approved library
/// position together with the move's shuffle mode. Under a terminal shuffle
/// the position is transient and the chained `Shuffle` randomizes the library
/// once. Under an ordinary placement the components arrive at that position
/// as one group, which their owner arranges (CR 730.3a + CR 401.4) through
/// the staged batch.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn move_merged_permanent_on_leave(
    state: &mut GameState,
    merged_id: ObjectId,
    dest: Zone,
    library_placement: Option<&LibraryPosition>,
    library_shuffle: LibraryShuffleMode,
    applied: &HashSet<AppliedReplacementKey>,
    performed_by: Option<crate::types::player::PlayerId>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    move_merged_permanent_for_parent(
        state,
        merged_id,
        dest,
        library_placement,
        library_shuffle,
        applied,
        performed_by,
        None,
        events,
    )
}

#[allow(clippy::too_many_arguments)] // main's placement pair plus the announcing owner link.
pub(crate) fn move_merged_permanent_for_parent(
    state: &mut GameState,
    merged_id: ObjectId,
    dest: Zone,
    library_placement: Option<&LibraryPosition>,
    library_shuffle: LibraryShuffleMode,
    applied: &HashSet<AppliedReplacementKey>,
    performed_by: Option<crate::types::player::PlayerId>,
    parent: Option<crate::types::game_state::PendingComponentDeliveryParent>,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let requests = plan_merged_component_moves(
        state,
        merged_id,
        dest,
        library_placement,
        library_shuffle,
        applied,
        performed_by,
    );
    if requests.is_empty() {
        return BatchMoveResult::Done;
    }
    zone_pipeline::move_component_batch_then(
        state,
        requests,
        ObjectIncarnationRef::from_object(&state.objects[&merged_id]),
        parent,
        events,
    )
}

/// CR 730.3d: plan the physical moves without changing the live permanent.
pub(crate) fn plan_merged_component_moves(
    state: &GameState,
    merged_id: ObjectId,
    dest: Zone,
    library_placement: Option<&LibraryPosition>,
    library_shuffle: LibraryShuffleMode,
    applied: &HashSet<AppliedReplacementKey>,
    performed_by: Option<crate::types::player::PlayerId>,
) -> Vec<ZoneMoveRequest> {
    let Some(survivor) = state.objects.get(&merged_id) else {
        return Vec::new();
    };
    survivor
        .merged_components
        .iter()
        .copied()
        .map(|component_id| {
            // CR 730.3: every component inherits the final approved destination
            // and position; its own replacement consult may redirect it again.
            let placement = match (dest, library_placement) {
                (Zone::Library, Some(position)) => Some(position.clone()),
                _ => None,
            };
            ZoneMoveRequest::merged_component(component_id, dest, placement, library_shuffle)
                .with_replacement_applied(applied.clone())
                // CR 608.2c + CR 730.3: every component is moved by the
                // player who performed the merged permanent's move, so each card
                // that reaches exile records that player as its exiler. No
                // ownership input is carried, as before: a component goes to its
                // own owner's zone.
                .actors(zone_pipeline::MoveActors {
                    performed_by,
                    ownership_actor: None,
                })
        })
        .collect()
}

/// Non-arranged component delivery keeps the existing serial split path.
pub(super) fn detach_merge_for_serial_delivery(state: &mut GameState, merged_id: ObjectId) {
    let survivor = &state.objects[&merged_id];
    let successor = leave_successor(survivor);
    remove_merge_layer_effect(state, merged_id);
    crate::game::layers::flush_layers(state);
    let survivor = state
        .objects
        .get_mut(&merged_id)
        .expect("merged survivor exists");
    let components = std::mem::take(&mut survivor.merged_components);
    survivor.merge_kind = None;
    for component in components.into_iter().filter(|id| *id != merged_id) {
        state
            .objects
            .get_mut(&component)
            .expect("merged component exists")
            .split_from_merge_survivor = Some(successor);
    }
}

/// CR 730.2d + CR 400.7 + CR 111.7: restore the survivor's intrinsic token-ness
/// after the leave-event snapshot captures the merged permanent's topmost-derived
/// token-ness, but before the object lands outside the battlefield. This lets
/// "creature token dies" filters see the object as it existed immediately before
/// leaving while still letting the restored token host cease to exist.
pub(crate) fn restore_pre_merge_tokenness_for_leave(state: &mut GameState, merged_id: ObjectId) {
    if let Some(survivor) = state.objects.get_mut(&merged_id) {
        if let Some(intrinsic) = survivor.pre_merge_is_token.take() {
            survivor.is_token = intrinsic;
        }
    }
}

/// CR 730.3c: "If an effect can find the new object that a merged permanent
/// becomes as it leaves the battlefield, it finds ALL of those objects. ... the
/// same actions are taken upon each of them." When an effect references the
/// object that just left the battlefield (a flicker/blink's "return it") and
/// that object was a merged permanent's survivor, the absorbed component cards it
/// split into (CR 730.3) must receive the same action too — otherwise a flicker
/// returns only the survivor and strands the other components in exile.
///
/// Given the objects a *continuity* reference (`ParentTarget`,
/// `TriggeringSource`, `ExiledBySource`) resolved to, append each survivor's
/// co-departed sibling components that are still co-located in the same zone, in
/// deterministic id order. The caller (the `ChangeZone` return loop, or
/// `ChangeZoneAll`'s linked-exile return) then applies its move to the whole
/// pile; the components return as separate, non-merged objects (CR 730.3 —
/// merging is not re-established) and their back-links clear on battlefield
/// entry.
///
/// This is a no-op unless the effect references a RECORDED DEPARTURE — the
/// object a merged permanent became when it left the battlefield — and a
/// resolved object is a former survivor with co-located components. A freshly
/// chosen target (Riftsweeper choosing one exiled card; reanimating one specific
/// card) carries no departure evidence for that object and must not over-return.
/// A tracked-set filter expands per resolved referent from the set's recorded
/// departures ([`expand_tracked_set_referents`]); co-location is not evidence
/// for a tracked set (CR 730.3c).
pub(crate) fn expand_returned_merge_components(
    state: &GameState,
    ability: &ResolvedAbility,
    resolved: Vec<ObjectId>,
    target_filter: &TargetFilter,
) -> Vec<ObjectId> {
    if matches!(target_filter, TargetFilter::SelfRef) {
        // CR 603.7c + CR 730.3c: a delayed return whose source binding followed
        // the source to the object it became when it left ("exile ~. Return it
        // … at the beginning of the next end step") finds every object the
        // merged permanent became.
        if let Some(components) = delayed_binding_departure_components(state, ability) {
            let mut expanded = resolved;
            for id in components {
                if !expanded.contains(&id) {
                    expanded.push(id);
                }
            }
            return expanded;
        }
        // CR 701.24a + CR 730.3c: a source's own recorded departure is
        // followed only by the move component of a terminal-shuffle operation
        // ("shuffle it into its owner's library"), whose order is randomized.
        if !is_terminal_shuffle_move(&ability.effect) {
            return resolved;
        }
        let mut expanded = resolved;
        for id in recorded_departure_components(state, ability) {
            if !expanded.contains(&id) {
                expanded.push(id);
            }
        }
        return expanded;
    }
    // CR 730.3c + CR 400.7: a tracked-set referent's components come from the
    // set's recorded battlefield departures (D2), per resolved referent; never
    // from co-location.
    if matches!(
        target_filter,
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. }
    ) {
        return expand_tracked_set_referents(state, ability, resolved, target_filter);
    }
    let mut expanded = resolved.clone();
    for &survivor_id in &resolved {
        if !referent_records_departure(state, ability, target_filter, survivor_id) {
            continue;
        }
        let components = co_split_components(state, survivor_id, &expanded);
        expanded.extend(components);
    }
    expanded
}

/// CR 730.3c + CR 400.7 + CR 608.2c: the bound move path over a tracked set.
/// Each resolved referent that is a member of the bound set, and whose
/// battlefield departure the set recorded, expands to the objects split at that
/// departure: a split object that is itself a member is found as a member; any
/// other only through the reader's D7 gate and its member's producer action
/// ([`tracked_set_finds_component`]). Expansion is per referent, never
/// set-wide: a referent the set did not move (a chosen card) and a set that
/// recorded no battlefield departure (a membership-only set) expand to nothing.
/// The referent object need not still exist (CR 704.5d): the recorded
/// departure, not the survivor, is the evidence.
fn expand_tracked_set_referents(
    state: &GameState,
    ability: &ResolvedAbility,
    resolved: Vec<ObjectId>,
    target_filter: &TargetFilter,
) -> Vec<ObjectId> {
    let (set_id, caused_by) =
        match targeting::resolve_tracked_set_sentinel(state, target_filter.clone()) {
            TargetFilter::TrackedSet { id } => (id, None),
            TargetFilter::TrackedSetFiltered { id, caused_by, .. } => (id, caused_by),
            // The combat-damage source rung yields a source filter, not a set:
            // no recorded departure to read (E3), no expansion.
            _ => return resolved,
        };
    let Some(members) = state.tracked_object_sets.get(&set_id) else {
        return resolved;
    };
    let mut expanded = resolved.clone();
    for referent in resolved {
        if !members.contains(&referent) {
            continue;
        }
        let Some(successor) = member_departure_successor(state, set_id, referent) else {
            continue;
        };
        let mut components: Vec<ObjectId> = state
            .objects
            .iter()
            .filter(|(id, obj)| {
                obj.split_from_merge_survivor == Some(successor) && !expanded.contains(id)
            })
            .map(|(id, _)| *id)
            .filter(|id| {
                members.contains(id)
                    || tracked_set_finds_component(
                        state,
                        set_id,
                        *id,
                        Some(ability),
                        caused_by.as_ref(),
                    )
            })
            .collect();
        components.sort_by_key(|id| id.0);
        expanded.extend(components);
    }
    expanded
}

/// CR 603.7c + CR 400.7 + CR 730.3c: the component cards of the merged
/// permanent this delayed ability's (CR 603.7a) source binding found when it
/// left the battlefield. The binding followed the source to the new object it
/// became (`delayed_source_context`), so it records that object's
/// off-battlefield zone and incarnation, which is exactly the survivor pin each
/// split component carries. Reading the recorded value, not the survivor's
/// current location, keeps a survivor that moved again (shuffled out of exile)
/// from hiding its unmoved components; the effect does as much as possible
/// (CR 609.3).
///
/// `None` when the binding did not follow a departed source, so other `SelfRef`
/// rules apply. A library destination is excluded: ordering several returned
/// components there is not implemented (#10759).
fn delayed_binding_departure_components(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<Vec<ObjectId>> {
    if ability.delayed_origin.is_none()
        || matches!(
            ability.effect,
            Effect::ChangeZone {
                destination: Zone::Library,
                ..
            }
        )
    {
        return None;
    }
    let identity = &ability.trigger_source.as_ref()?.identity;
    if identity.reference.object_id != ability.source_id
        || identity.expected_zone == Zone::Battlefield
    {
        return None;
    }
    let mut components: Vec<ObjectId> = state
        .objects
        .iter()
        .filter(|(_, obj)| {
            obj.zone == identity.expected_zone
                && obj.split_from_merge_survivor == Some(identity.reference)
        })
        .map(|(id, _)| *id)
        .collect();
    components.sort_by_key(|id| id.0);
    Some(components)
}

/// CR 701.24a: the effect is the move component of a parser-emitted "shuffle
/// [set] into [library]" operation, marked by its explicit
/// `LibraryShuffleMode::TerminalShuffle`.
fn is_terminal_shuffle_move(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            destination: Zone::Library,
            library_shuffle: LibraryShuffleMode::TerminalShuffle,
            ..
        } | Effect::ChangeZoneAll {
            destination: Zone::Library,
            library_shuffle: LibraryShuffleMode::TerminalShuffle,
            ..
        }
    )
}

/// CR 730.3c + CR 400.7e + CR 603.7c: Whether this effect found `referent` as
/// the object a merged permanent became when it LEFT the battlefield, rather
/// than an object it merely chose. The filter kind alone cannot answer this: a
/// fresh `ParentTarget` selection and a flicker continuation carry the same
/// anaphor. The evidence must name this exact object incarnation:
///
/// * `ParentTarget`/`ParentTargetSlot`: the referent's current incarnation is
///   pinned by an event/delayed binding (`target_incarnations`), is a result
///   of the immediate producer that moved it (`forwarded_result_context`), or
///   is exactly one past the incarnation announced for it (`target_pins`: an
///   earlier instruction of this resolution moved it). A pin or result for any
///   other object is not evidence for this one.
/// * `TriggeringSource`: the trigger event is this referent's own departure
///   from the battlefield.
/// * `ExiledBySource` is formed by an earlier move in the chain and records a
///   departure.
///
/// Tracked-set filters never reach this function:
/// [`expand_tracked_set_referents`] answers them from the set's recorded
/// departures.
///
/// `SelfRef` is answered separately by [`recorded_departure_components`],
/// which requires the trigger's own departure event.
fn referent_records_departure(
    state: &GameState,
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
    referent: ObjectId,
) -> bool {
    let Some(current) = state
        .objects
        .get(&referent)
        .map(ObjectIncarnationRef::from_object)
    else {
        return false;
    };
    match target_filter {
        TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. } => {
            ability.target_incarnations.contains(&current)
                || ability
                    .context
                    .forwarded_result_context
                    .as_ref()
                    .is_some_and(|result| result.object_incarnations.contains(&current))
                // CR 400.7 + CR 601.2c: the incarnation announced for this
                // referent is exactly one behind its current one: an earlier
                // instruction of this same resolution moved it ("Exile target
                // creature you control, then return that card"). A fresh
                // selection of an already-departed object announced the
                // incarnation it still has.
                || ability.target_pins.iter().flatten().any(|pin| {
                    pin.object_id == current.object_id
                        && pin.incarnation.checked_add(1) == Some(current.incarnation)
                })
        }
        TargetFilter::ExiledBySource => true,
        TargetFilter::TriggeringSource => matches!(
            state.current_trigger_event.as_ref(),
            Some(GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                ..
            }) if *object_id == referent
        ),
        _ => false,
    }
}

/// CR 730.3c: The component cards that the merged permanent identified by
/// `survivor_id` split into when it left the battlefield (CR 730.3), and that are
/// still co-located with the survivor in its current (off-battlefield) zone —
/// returned in deterministic id order, omitting any id already in `exclude`.
///
/// Empty unless `survivor_id` is a former merged survivor with components still
/// in its zone. Shared by both return paths that "find the object the merged
/// permanent became": the `ChangeZone` continuity-reference return
/// ([`expand_returned_merge_components`], flicker/blink and the linked-exile
/// return) and the `UntilSourceLeaves` implicit return in
/// `engine::check_exile_returns` (exile-until-this-leaves effects).
///
/// A component whose pinned survivor incarnation is no longer current is not
/// returned (CR 400.7): the survivor changed zones since the split.
pub(crate) fn co_split_components(
    state: &GameState,
    survivor_id: ObjectId,
    exclude: &[ObjectId],
) -> Vec<ObjectId> {
    let Some(survivor) = state.objects.get(&survivor_id) else {
        return Vec::new();
    };
    let zone = survivor.zone;
    let survivor_ref = ObjectIncarnationRef::from_object(survivor);
    // Split-out components are never independent members of the battlefield
    // (CR 730.2) — only an off-battlefield survivor can have co-located ones.
    if zone == Zone::Battlefield {
        return Vec::new();
    }
    let mut components: Vec<ObjectId> = state
        .objects
        .iter()
        .filter(|(id, obj)| {
            obj.split_from_merge_survivor == Some(survivor_ref)
                && obj.zone == zone
                && !exclude.contains(*id)
        })
        .map(|(id, _)| *id)
        .collect();
    components.sort_by_key(|id| id.0);
    components
}

/// CR 400.7: the incarnation the survivor holds once its leave move lands; that
/// move bumps it exactly once, so a later zone change no longer matches.
fn leave_successor(survivor: &GameObject) -> ObjectIncarnationRef {
    departure_successor(survivor.id, survivor.incarnation)
}

/// CR 400.7 + CR 730.3: the incarnation a leave move lands on, which is also the
/// split pin every component of the departed merged permanent carries. The move
/// bumps the departing object's incarnation exactly once, so the successor of
/// departure incarnation `k` is `k + 1`.
pub(crate) fn departure_successor(
    object_id: ObjectId,
    departure_incarnation: u64,
) -> ObjectIncarnationRef {
    ObjectIncarnationRef::of(object_id, departure_incarnation + 1)
}

/// CR 400.7e + CR 730.3c: the component cards of the merged permanent whose
/// departure from the battlefield triggered this ability, read once from the
/// trigger's recorded event: the public zone the source moved to and the
/// successor incarnation its split components were pinned to. The recorded pin
/// is a value, so a survivor that moved again (Cremate responding to the dies
/// trigger) does not hide its sibling; the effect still does as much as
/// possible (CR 609.3). Empty when the ability was not triggered by its own
/// source's departure.
fn recorded_departure_components(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    let Some(source) = ability.trigger_source.as_ref() else {
        return Vec::new();
    };
    let Some(event @ GameEvent::ZoneChanged { object_id, .. }) =
        state.current_trigger_event.as_ref()
    else {
        return Vec::new();
    };
    if *object_id != ability.source_id {
        return Vec::new();
    }
    // The ability's own capture is the departing identity; the turn ledger
    // confirms it, so an event copy without source context still answers.
    departed_identity_components(state, event, source.identity.reference)
}

/// CR 730.3 + CR 730.3c + CR 400.7e: the component cards a merged permanent split
/// into when the battlefield departure `event` moved its survivor, read from the
/// recorded event: the public zone the survivor moved to and the successor
/// incarnation its split components were pinned to. The pin is a value, so a
/// survivor that moved again does not hide its sibling (CR 609.3). A component
/// that left that zone since has lost its back-link (`zones.rs` exit cleanup) and
/// is not listed. The departing identity is the one the event itself records, so
/// an event without it, any other event, a non-public destination, or a record
/// the turn ledger does not confirm gives none. Returned in id order.
pub(crate) fn departure_event_components(state: &GameState, event: &GameEvent) -> Vec<ObjectId> {
    let GameEvent::ZoneChanged { record, .. } = event else {
        return Vec::new();
    };
    let Some(departing) = record
        .trigger_source_context()
        .map(|context| context.identity.reference)
    else {
        return Vec::new();
    };
    departed_identity_components(state, event, departing)
}

/// CR 730.3 + CR 730.3c + CR 400.7e: the single scan behind both callers. The
/// caller supplies the departing identity it holds (`expected`): the ability's
/// captured source for a source's own departure, or the identity the event
/// records. The turn ledger entry at the event's index must carry that identity,
/// and the components are the objects in the event's public destination whose
/// back-link names the survivor's successor incarnation.
fn departed_identity_components(
    state: &GameState,
    event: &GameEvent,
    expected: ObjectIncarnationRef,
) -> Vec<ObjectId> {
    let GameEvent::ZoneChanged {
        from: Some(Zone::Battlefield),
        to,
        record,
        ..
    } = event
    else {
        return Vec::new();
    };
    if !to.is_public() {
        return Vec::new();
    }
    let recorded = state
        .zone_changes_this_turn
        .get(record.turn_zone_change_index)
        .and_then(|recorded| recorded.trigger_source_context())
        .is_some_and(|context| context.identity.reference == expected);
    if !recorded {
        return Vec::new();
    }
    let successor = departure_successor(expected.object_id, expected.incarnation);
    let mut components: Vec<ObjectId> = state
        .objects
        .iter()
        .filter(|(_, obj)| obj.zone == *to && obj.split_from_merge_survivor == Some(successor))
        .map(|(id, _)| *id)
        .collect();
    components.sort_by_key(|id| id.0);
    components
}

/// CR 730.3c + CR 400.7: the successor incarnation a tracked-set member's
/// battlefield departure landed on, read from the producer's recorded origin.
/// `Some` only for a member that departed the battlefield: a component can be
/// found only through a merged permanent's battlefield departure.
fn member_departure_successor(
    state: &GameState,
    set_id: TrackedSetId,
    member: ObjectId,
) -> Option<ObjectIncarnationRef> {
    let origin = state
        .tracked_set_member_origins
        .get(&set_id)?
        .get(&member)
        .filter(|origin| origin.departed_from == Zone::Battlefield)?;
    Some(departure_successor(member, origin.departure_incarnation))
}

/// CR 730.3c: every non-member object split from a member of `set_id` at that
/// member's recorded battlefield departure, as `(component, member)` pairs
/// sorted by component id. Provenance is NOT applied here (see
/// [`finding_admitted`]); this is the structural relation alone.
///
/// Equality is on the full [`ObjectIncarnationRef`], so a component split at an
/// earlier departure of the same member id is not found (CR 400.7: the later
/// object is a new object). Zone-agnostic by design: a component that changed
/// zones has a cleared pin, so it is a new object (CR 400.7). A membership-only
/// set (no battlefield-departure origin) returns empty without scanning objects.
pub(crate) fn tracked_set_split_components(
    state: &GameState,
    set_id: TrackedSetId,
) -> Vec<(ObjectId, ObjectId)> {
    let Some(members) = state.tracked_object_sets.get(&set_id) else {
        return Vec::new();
    };
    let successors: Vec<(ObjectIncarnationRef, ObjectId)> = members
        .iter()
        .filter_map(|member| {
            member_departure_successor(state, set_id, *member).map(|pin| (pin, *member))
        })
        .collect();
    if successors.is_empty() {
        return Vec::new();
    }
    let mut components: Vec<(ObjectId, ObjectId)> = state
        .objects
        .iter()
        .filter(|(id, _)| !members.contains(id))
        .filter_map(|(id, obj)| {
            let pin = obj.split_from_merge_survivor?;
            successors
                .iter()
                .find(|(successor, _)| *successor == pin)
                .map(|(_, member)| (*id, *member))
        })
        .collect();
    components.sort_by_key(|(component, _)| component.0);
    components
}

/// Whether `effect` is a tracked-set publisher in the census sense, the effects the
/// finding gate counts on a root's chain line (D7). The list is every `Effect` whose
/// resolution can stamp a zone-transition cause on a member (CR 608.2c; the per-member
/// stamp authority is `this_way_cause_for_resolved`, which defers to
/// `this_way_cause_for_effect` / `this_way_cause_for_action` except for `Counter`'s exile
/// rider, `SetTapState` and `GiveControl`), plus the dedicated-seam producers the census
/// of tracked-set readers names. The census is the source of truth for the list; its
/// `CopyToken` token names no `Effect` variant (`Effect::CopyTokenOf` is the nearest, and
/// the corpus has no effect under the census name), so the 24 `Effect` variants remain.
/// Not keyed on `EffectKind`, which is lossy (`RevealTop` collapses into `Reveal`,
/// `SetTapState` into `Tap`/`Untap`).
///
/// An unlisted effect may still publish a set through the generic event seam
/// (`publish_tracked_set_for_resolution`). It stamps no zone-transition cause, and
/// `stamp_member_causes` writes nothing for a member without a cause. So a member that only
/// an unlisted effect contributed carries no stamp, and `finding_admitted`'s veto rejects
/// the set (`u7_an_unstamped_member_vetoes_the_finding`). A member it re-publishes keeps the
/// stamp the listed publisher wrote, which still names the action that produced that
/// member, so the finding the veto admits is unchanged. The match is exhaustive, so a new
/// `Effect` variant must be classified here.
fn is_census_publisher(effect: &Effect) -> bool {
    match effect {
        Effect::Destroy { .. }
        | Effect::Counter { .. }
        | Effect::Token { .. }
        | Effect::SetTapState { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Discard { .. }
        | Effect::SearchLibrary { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::GiveControl { .. } => true,
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::ResumeDamageBatch { .. }
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::CounterAll { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::RemoveCounter { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::Populate
        | Effect::Clash
        | Effect::ExileTopCardContest { .. }
        | Effect::Behold { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Vote { .. }
        | Effect::SeparateIntoPiles { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Meld { .. }
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        | Effect::ChoosePermanent { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::ReproduceEventCounters { .. }
        | Effect::Animate { .. }
        | Effect::ReturnAsAura { .. }
        | Effect::RegisterBending { .. }
        | Effect::GenericEffect { .. }
        | Effect::Cleanup { .. }
        | Effect::Mana { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        | Effect::FlipPermanent { .. }
        | Effect::SearchOutsideGame { .. }
        | Effect::OpenBoosterPack { .. }
        | Effect::RevealHand { .. }
        | Effect::RevealFromHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealChosenNumbers { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::SetClassLevel { .. }
        | Effect::CreateDelayedTrigger { .. }
        | Effect::AddTargetReplacement { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::CreateEmblem { .. }
        | Effect::CreateBoon { .. }
        | Effect::NoteNumber { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CreateDrawReplacement { .. }
        | Effect::CreatePlaneswalkReplacement { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RollDie { .. }
        | Effect::FlipCoins { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        | Effect::AbandonScheme
        | Effect::ChaosEnsues
        | Effect::ReverseTurnOrder
        | Effect::RedistributeLifeTotals
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::RememberCard { .. }
        | Effect::NoteManaSpent
        | Effect::ForEachCategory { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::Discover { .. }
        | Effect::PhysicalContest { .. }
        | Effect::Heist { .. }
        | Effect::HeistExile
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::TurnFaceUp { .. }
        | Effect::TurnFaceDown { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::ModifyAnnouncedX { .. }
        | Effect::Double { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::EmpowerJace { .. }
        | Effect::Monstrosity { .. }
        | Effect::HordePlayCard { .. }
        | Effect::Specialize
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::CompletePlayerAction { .. }
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        | Effect::SetDayNight { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::Conjure { .. }
        | Effect::CreateCardCopyByName { .. }
        | Effect::ApplyPerpetual { .. }
        | Effect::Intensify { .. }
        | Effect::DraftFromSpellbook { .. }
        | Effect::ChooseOneOf { .. }
        | Effect::Unimplemented { .. } => false,
    }
}

/// CR 608.2c + CR 730.3c: whether a reader may find the components of `set_id`
/// (the D7 gate). A merged permanent's components are found only by a reader
/// that reads the set as the product of the action that moved the survivor.
///
/// - Provenance: the reader must be reading `set_id` through its own
///   resolution's chain rung ([`targeting::chain_rung_root`]). A concrete id
///   carried into the resolution (a delayed bound id, a cost-exile id) is
///   allocated before any chain id of this resolution, so it never equals the
///   chain id and is never admitted. A call with no reader (the validator,
///   legal-action enumeration) is never admitted.
/// - Cause-bound (`caused_by` set): admitted; CR 608.2c + CR 614.6 the producer
///   action is then checked per member by the caller (D4).
/// - Cause-free: the reader must follow exactly one census publisher on the
///   root's chain line, that publisher's declared action must be a zone
///   transition, and every member of the set must carry that action's stamp
///   (the veto: a set holding an unstamped or differently-caused member was not
///   produced solely by that action). For a modal root the line is walked from
///   the root, so earlier chosen modes are on it too: a later-mode reader is
///   admitted only when no earlier mode publishes and its effect occurs once on
///   the line (the conservative direction).
///
/// No cost-exile branch: no marker persists that the resolution could recognize,
/// so every carried concrete id is rejected (the safe direction).
fn finding_admitted(
    state: &GameState,
    set_id: TrackedSetId,
    reader: Option<&ResolvedAbility>,
    caused_by: Option<&ThisWayCause>,
) -> bool {
    let Some(reader) = reader else {
        return false;
    };
    let Some(root) = targeting::chain_rung_root(state, set_id, reader) else {
        return false;
    };
    if caused_by.is_some() {
        return true;
    }
    let line = || std::iter::successors(Some(root), |node| node.sub_ability.as_deref());
    if line().filter(|node| node.effect == reader.effect).count() != 1 {
        return false;
    }
    let mut publishers = line()
        .take_while(|node| node.effect != reader.effect)
        .filter(|node| is_census_publisher(&node.effect));
    let (Some(publisher), None) = (publishers.next(), publishers.next()) else {
        return false;
    };
    let Some(cause) = crate::game::effects::this_way_cause_for_effect(&publisher.effect)
        .filter(|cause| cause.is_zone_transition())
    else {
        return false;
    };
    let (Some(members), Some(stamps)) = (
        state.tracked_object_sets.get(&set_id),
        state.tracked_set_member_causes.get(&set_id),
    ) else {
        return false;
    };
    members
        .iter()
        .all(|member| stamps.get(member) == Some(&cause))
}

/// CR 730.3c + CR 608.2c + CR 614.6: D4 — a component inherits its member's
/// producer-action stamp. With no bound cause every component qualifies.
fn member_cause_matches(
    state: &GameState,
    set_id: TrackedSetId,
    member: ObjectId,
    caused_by: Option<&ThisWayCause>,
) -> bool {
    caused_by.is_none_or(|bound| {
        state
            .tracked_set_member_causes
            .get(&set_id)
            .and_then(|stamps| stamps.get(&member))
            == Some(bound)
    })
}

/// CR 730.3c: membership form of "an effect that can find the new object a
/// merged permanent becomes finds all of its components". Whether the
/// non-member `object_id` is a component split from a member of `set_id` at that
/// member's recorded battlefield departure, that the reader may find (the D7
/// gate) and whose member's producer action matches `caused_by` (D4).
///
/// Cheap rejects first: an object with no split pin (every ordinary object) is
/// rejected before any set work. Callers test direct membership first, so a
/// member never reaches this.
pub(crate) fn tracked_set_finds_component(
    state: &GameState,
    set_id: TrackedSetId,
    object_id: ObjectId,
    reader: Option<&ResolvedAbility>,
    caused_by: Option<&ThisWayCause>,
) -> bool {
    let Some(pin) = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.split_from_merge_survivor)
    else {
        return false;
    };
    let member = pin.object_id;
    state
        .tracked_object_sets
        .get(&set_id)
        .is_some_and(|members| members.contains(&member))
        && member_departure_successor(state, set_id, member) == Some(pin)
        && member_cause_matches(state, set_id, member, caused_by)
        && finding_admitted(state, set_id, reader, caused_by)
}

/// CR 730.3c: enumeration form of [`tracked_set_finds_component`]. The raw
/// members of `set_id` in set order (`caused_by` is not applied to them: that
/// is the zone projection's existing semantics), followed by the found
/// components sorted by id. The gate is evaluated once, and only when the
/// structural relation is non-empty. Components are disjoint from the members.
pub(crate) fn tracked_set_found_objects(
    state: &GameState,
    set_id: TrackedSetId,
    reader: Option<&ResolvedAbility>,
    caused_by: Option<&ThisWayCause>,
) -> Vec<ObjectId> {
    let mut found = state
        .tracked_object_sets
        .get(&set_id)
        .cloned()
        .unwrap_or_default();
    let components = tracked_set_split_components(state, set_id);
    if components.is_empty() || !finding_admitted(state, set_id, reader, caused_by) {
        return found;
    }
    found.extend(
        components
            .into_iter()
            .filter(|(_, member)| member_cause_matches(state, set_id, *member, caused_by))
            .map(|(component, _)| component),
    );
    found
}

/// CR 730.3 + CR 712.21: Put a non-surviving merge component into `dest` as a
/// NEW object that did NOT independently leave the battlefield.
///
/// A merged permanent is a single permanent (CR 730.2c); when it leaves, only
/// the surviving object's move is a battlefield exit. Each absorbed component is
/// "put into the appropriate zone" (CR 730.3) as a new object, emitting
/// `ZoneChanged { from: None, .. }` — mirroring token creation (CR 111.1), where
/// an object that appears directly in a zone has no origin zone.
///
/// This makes every battlefield-exit observer — "leaves the battlefield" / "dies"
/// triggers (`from == Battlefield`) and the `CreatureDiedThisTurn` look-back
/// (`from_zone == Some(Battlefield)`) — fire ONLY for the survivor, i.e. once for
/// the whole pile, while origin-agnostic observers ("whenever a card is put into
/// a graveyard from anywhere") still fire once per component card. This matches
/// the CR 712.21 meld worked example: a melded creature dying triggers "a
/// creature dies" once but "a card is put into a graveyard" once per card.
///
/// Composes `zones::apply_zone_exit_cleanup` (CR 400.7 new-object reset) and
/// `zones::add_to_zone` rather than `zones::move_to_zone`, because the component
/// is absorbed into the survivor (not present in any zone list) and its move must
/// not be a battlefield exit.
pub(crate) fn put_component_into_zone(
    state: &mut GameState,
    component_id: ObjectId,
    dest: Zone,
    survivor_successor: Option<ObjectIncarnationRef>,
    events: &mut Vec<GameEvent>,
) {
    // CR 603.10a: snapshot the component's characteristics BEFORE the CR 400.7
    // cleanup, so a transformed/animated component records its event-time face
    // (mirrors `move_to_zone`, which snapshots before exit cleanup). Origin is
    // `None`: the component enters `dest` as a new object, not as a departure
    // from the battlefield.
    let Some(mut record) = state
        .objects
        .get(&component_id)
        .map(|obj| obj.snapshot_for_zone_change(component_id, None, dest))
    else {
        return;
    };
    crate::game::zones::route_component(state, component_id, dest);

    // CR 730.3c: the cleanup clears previous incarnation links. Stamp the
    // new link only on arrival so a parked choice cannot expose a split card.
    if let Some(obj) = state.objects.get_mut(&component_id) {
        obj.split_from_merge_survivor = survivor_successor;
    }

    crate::game::restrictions::record_zone_change(state, &mut record);
    events.push(GameEvent::ZoneChanged {
        object_id: component_id,
        from: None,
        to: dest,
        record: Box::new(record),
    });
}

/// CR 702.140c + CR 730.2a: Resolve the controller's top/bottom choice for a
/// paused mutating creature spell. Consumes the active `MutateMerge` frame,
/// performs the merge, and returns the engine to priority. Errors if no merge is
/// pending or the acting player is not the spell's controller.
pub fn handle_mutate_merge_choice(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    side: MergeSide,
    events: &mut Vec<GameEvent>,
) -> Result<crate::types::game_state::WaitingFor, crate::game::engine::EngineError> {
    use crate::game::engine::EngineError;

    let pending = state
        .active_mutate_merge_frame()
        .ok_or_else(|| EngineError::ActionNotAllowed("No mutate merge is pending".to_string()))?;
    if pending.controller != player {
        return Err(EngineError::ActionNotAllowed(
            "Only the mutate spell's controller may choose the merge side".to_string(),
        ));
    }

    let pending = state
        .take_active_mutate_merge_frame()
        .map_err(|error| EngineError::ActionNotAllowed(error.to_string()))?
        .expect("active mutate-merge frame was checked before consuming it");

    merge_object_onto(state, pending.merging_id, pending.target_id, side, events);

    // CR 702.140c: resolution is complete; hand priority back to the active
    // player so SBAs/triggers from the `Mutated` event can be processed.
    Ok(crate::types::game_state::WaitingFor::Priority {
        player: state.active_player,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::morph::apply_face_down_creature_characteristics;
    use crate::game::printed_cards::is_runtime_target_die_exile_replacement;
    use crate::game::zones::create_object;
    use crate::parser::oracle::parse_oracle_text;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, FaceDownProfile, QuantityExpr,
        ReplacementDefinition, TargetFilter,
    };
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::identifiers::CardId;
    use crate::types::keywords::{Keyword, WardCost};
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;

    fn make_creature(
        state: &mut GameState,
        card: u64,
        player: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_name = name.to_string();
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.base_card_types = CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        };
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.card_types = obj.base_card_types.clone();
        id
    }

    fn draw_n_ability(n: i32) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: n },
                target: TargetFilter::Controller,
            },
        )
    }

    fn set_base_abilities(state: &mut GameState, id: ObjectId, abilities: Vec<AbilityDefinition>) {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_abilities = Arc::new(abilities.clone());
        obj.abilities = Arc::new(abilities);
    }

    fn make_face_down(
        state: &mut GameState,
        player: PlayerId,
        profile: FaceDownProfile,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(50),
            player,
            "Secret".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        apply_face_down_creature_characteristics(obj, &profile);
        id
    }

    fn ward_2() -> Keyword {
        Keyword::Ward(WardCost::Mana(ManaCost::generic(2)))
    }

    /// V16 — CR 730.3 + CR 730.3c + CR 400.7e: the components a battlefield
    /// departure made are read from the recorded event. A co-located component
    /// of a different split group is not one of them, a component that left the
    /// zone since is not listed, and an event that is not a departure lists none.
    #[test]
    fn departure_event_components_reads_only_the_recorded_split_group() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let mut events = Vec::new();

        // An earlier merged permanent's split leaves its component in the graveyard.
        let other_host = make_creature(&mut state, 1, player, "Other Host", 2, 2);
        let other_rider = make_creature(&mut state, 2, player, "Other Rider", 1, 1);
        merge_object_onto(
            &mut state,
            other_rider,
            other_host,
            MergeSide::Bottom,
            &mut events,
        );
        crate::game::zones::move_to_zone(&mut state, other_host, Zone::Graveyard, &mut events);
        assert!(
            state.objects[&other_rider]
                .split_from_merge_survivor
                .is_some(),
            "reach-guard: the other group's component carries its own back-link"
        );

        let host = make_creature(&mut state, 3, player, "Host", 7, 2);
        let rider = make_creature(&mut state, 4, player, "Rider", 4, 4);
        merge_object_onto(&mut state, rider, host, MergeSide::Bottom, &mut events);
        let mut departure_events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut departure_events);
        let departure = departure_events
            .iter()
            .find(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        object_id,
                        from: Some(Zone::Battlefield),
                        ..
                    } if *object_id == host
                )
            })
            .cloned()
            .expect("the merged permanent's departure is recorded");
        assert_eq!(state.objects[&other_rider].zone, Zone::Graveyard);

        assert_eq!(
            departure_event_components(&state, &departure),
            vec![rider],
            "only the recorded survivor's split group"
        );

        let not_a_departure = GameEvent::EffectResolved {
            kind: crate::types::ability::EffectKind::ChangeZone,
            source_id: host,
            subject: None,
        };
        assert!(departure_event_components(&state, &not_a_departure).is_empty());

        crate::game::zones::move_to_zone(&mut state, rider, Zone::Exile, &mut events);
        assert!(
            departure_event_components(&state, &departure).is_empty(),
            "a component that left the zone lost its back-link"
        );
    }

    /// CR 701.24a + CR 730.3c + CR 400.7e: a source's own terminal-shuffle
    /// return answers from the ability's captured identity plus the matching
    /// turn-ledger entry, even when the event copy it reads carries no source
    /// context (the wire format allows it to be absent). The event-only reader
    /// used by delayed triggers has no identity to confirm and lists none.
    #[test]
    fn terminal_shuffle_return_keeps_components_when_the_event_copy_lacks_context() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let host = make_creature(&mut state, 1, player, "Host", 7, 2);
        let rider = make_creature(&mut state, 2, player, "Rider", 4, 4);
        let mut events = Vec::new();
        merge_object_onto(&mut state, rider, host, MergeSide::Bottom, &mut events);
        let mut departure_events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, host, Zone::Graveyard, &mut departure_events);
        let mut departure = departure_events
            .into_iter()
            .find(|event| {
                matches!(
                    event,
                    GameEvent::ZoneChanged {
                        object_id,
                        from: Some(Zone::Battlefield),
                        ..
                    } if *object_id == host
                )
            })
            .expect("the merged permanent's departure is recorded");
        let GameEvent::ZoneChanged { record, .. } = &mut departure else {
            unreachable!("matched a zone change above");
        };
        let captured = record
            .trigger_source_context
            .take()
            .expect("reach-guard: the live departure records its identity");
        state.current_trigger_event = Some(departure.clone());

        let mut ability = ResolvedAbility::new(
            Effect::ChangeZone {
                actor: crate::types::ability::LibraryInstructionActor::Controller,
                library_shuffle: LibraryShuffleMode::TerminalShuffle,
                origin: None,
                destination: Zone::Library,
                target: TargetFilter::SelfRef,
                owner_library: true,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
            vec![],
            host,
            player,
        );
        ability.trigger_source = Some(captured);

        let expanded =
            expand_returned_merge_components(&state, &ability, vec![host], &TargetFilter::SelfRef);
        assert_eq!(
            expanded,
            vec![host, rider],
            "the captured identity and the ledger still find the component"
        );
        assert!(
            departure_event_components(&state, &departure).is_empty(),
            "the event-only reader needs the event's own identity"
        );
    }

    // Plan test 4: a creature mutated UNDER a face-down survivor — the survivor
    // stays a face-down 2/2 with no abilities (topmost is the face-down survivor;
    // CR 730.2e + CR 708.2a, Layer 1b re-seed).
    #[test]
    fn mutate_under_face_down_stays_face_down_2_2() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let survivor = make_face_down(&mut state, player, FaceDownProfile::vanilla_2_2());
        let mutant = make_creature(&mut state, 1, player, "Mutant", 5, 5);
        set_base_abilities(&mut state, mutant, vec![draw_n_ability(1)]);

        let mut events = Vec::new();
        merge_object_onto(&mut state, mutant, survivor, MergeSide::Bottom, &mut events);
        evaluate_layers(&mut state);

        let s = &state.objects[&survivor];
        assert!(s.face_down, "topmost is the face-down survivor (CR 730.2e)");
        assert_eq!(s.power, Some(2));
        assert_eq!(s.toughness, Some(2));
        assert!(
            s.abilities.is_empty(),
            "a face-down permanent has no abilities (CR 708.2a); Layer 1b drops the unioned mutant ability"
        );

        // SIBLING: mutate-under a FACE-UP host unions abilities normally.
        let host = make_creature(&mut state, 2, player, "Host", 3, 3);
        set_base_abilities(&mut state, host, vec![draw_n_ability(1)]);
        let mutant2 = make_creature(&mut state, 3, player, "Mutant2", 5, 5);
        set_base_abilities(&mut state, mutant2, vec![draw_n_ability(2)]);
        merge_object_onto(&mut state, mutant2, host, MergeSide::Bottom, &mut events);
        evaluate_layers(&mut state);
        let h = &state.objects[&host];
        assert!(!h.face_down);
        assert_eq!(
            h.abilities.len(),
            2,
            "a face-up merged permanent unions every component's abilities (CR 702.140e)"
        );
    }

    // Plan test 5: a creature mutated OVER a cloaked face-down survivor clears the
    // survivor's face-down status (topmost is face up — CR 730.2e), drops the
    // buried ward (CR 708.2a + CR 730.2e), and emits NO TurnedFaceUp event.
    #[test]
    fn mutate_over_face_down_clears_face_down_drops_ward() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let survivor = make_face_down(&mut state, player, FaceDownProfile::cloaked_2_2());
        assert!(state.objects[&survivor].base_keywords.contains(&ward_2()));
        let mutant = make_creature(&mut state, 1, player, "Mutant", 5, 5);

        let mut events = Vec::new();
        merge_object_onto(&mut state, mutant, survivor, MergeSide::Top, &mut events);
        evaluate_layers(&mut state);

        let s = &state.objects[&survivor];
        assert!(
            !s.face_down,
            "status follows the face-up topmost component (CR 730.2e)"
        );
        assert!(
            !s.keywords.contains(&ward_2()), // allow-raw-authority: asserts the exact post-merge keyword snapshot, not an off-zone-aware query
            "a face-up merged permanent does not inherit a buried face-down ward (CR 708.2a)"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::TurnedFaceUp { .. })),
            "merging is not 'turned face up' (CR 730.2e) — no TurnedFaceUp event"
        );
    }

    // CR 730.2b/c + CR 614.1d: a spell cast through Edgar, Master Machinist's
    // permission carries "that artifact enters tapped" while on the stack. If it
    // merges instead of entering, there is no entry to modify: the absorbed
    // component's rider is cleared, the survivor isn't tapped, and no entry
    // event is created. (No production route casts Mutate from the graveyard
    // today, so this pins the seam directly.)
    #[test]
    fn merge_clears_absorbed_cast_permission_enter_tapped() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let survivor = make_creature(&mut state, 1, player, "Ornithopter", 0, 2);
        let mutant = make_creature(&mut state, 2, player, "Gemrazer", 4, 4);
        {
            let obj = state.objects.get_mut(&mutant).unwrap();
            obj.zone = Zone::Stack;
            obj.cast_permission_enter_tapped = crate::types::zones::EtbTapState::Tapped;
        }
        state.battlefield.retain(|id| *id != mutant);

        let mut events = Vec::new();
        merge_object_onto(&mut state, mutant, survivor, MergeSide::Top, &mut events);

        assert_eq!(
            state.objects[&mutant].cast_permission_enter_tapped,
            crate::types::zones::EtbTapState::Unspecified,
            "the absorbed component carries no rider"
        );
        assert!(
            !state.objects[&survivor].tapped,
            "the survivor isn't tapped"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, GameEvent::Mutated { .. })),
            "reach guard: the merge happened"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::ZoneChanged { .. })),
            "CR 730.2c: the merge isn't an entry"
        );
    }

    // Plan test 6: merged_copiable_values skips a non-topmost FACE-DOWN component's
    // listed characteristics, but unions a non-topmost FACE-UP component's.
    #[test]
    fn merged_copiable_values_skips_non_topmost_face_down_component() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Topmost face-up component carries Flying.
        let top = make_creature(&mut state, 1, player, "Top", 3, 3);
        state.objects.get_mut(&top).unwrap().base_keywords = vec![Keyword::Flying];
        // Non-topmost cloaked face-down component carries ward {2}.
        let buried = make_face_down(&mut state, player, FaceDownProfile::cloaked_2_2());

        let (values, _) = merged_copiable_values(&state, &[top, buried], top).unwrap();
        assert!(values.keywords.contains(&Keyword::Flying)); // allow-raw-authority: merged_copiable_values snapshot struct, not a GameObject
        assert!(
            !values.keywords.contains(&ward_2()), // allow-raw-authority: merged_copiable_values snapshot struct, not a GameObject
            "non-topmost face-down component's ward is suppressed (CR 708.2a + CR 730.2e)"
        );

        // NEGATIVE: a non-topmost FACE-UP component IS unioned.
        let buried_up = make_creature(&mut state, 2, player, "BuriedUp", 1, 1);
        state.objects.get_mut(&buried_up).unwrap().base_keywords = vec![Keyword::Trample];
        let (values2, _) = merged_copiable_values(&state, &[top, buried_up], top).unwrap();
        assert!(
            values2.keywords.contains(&Keyword::Trample), // allow-raw-authority: merged_copiable_values snapshot struct, not a GameObject
            "a face-up non-topmost component's keywords are unioned (CR 702.140e)"
        );
    }

    #[test]
    fn merged_copiable_values_exclude_a_target_bound_die_exile_rider() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let top = make_creature(&mut state, 1, player, "Top", 3, 3);
        let marked = make_creature(&mut state, 2, player, "Marked", 2, 4);
        let parsed = parse_oracle_text(
            "Touch of the Void deals 3 damage to any target. If a creature dealt damage this way would die this turn, exile it instead.",
            "Touch of the Void",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        let mut cursor = Some(&parsed.abilities[0]);
        let die_exile = loop {
            let def = cursor.expect("the parsed spell must contain a die-exile rider");
            if let Effect::AddTargetReplacement { replacement, .. } = def.effect.as_ref() {
                break replacement.as_ref().clone();
            }
            cursor = def.sub_ability.as_deref();
        };
        assert!(is_runtime_target_die_exile_replacement(&die_exile));
        let printed =
            ReplacementDefinition::new(crate::types::replacements::ReplacementEvent::Moved)
                .valid_card(TargetFilter::SelfRef)
                .destination_zone(Zone::Battlefield);
        let obj = state.objects.get_mut(&marked).unwrap();
        Arc::make_mut(&mut obj.base_replacement_definitions)
            .extend([die_exile.clone(), printed.clone()]);

        let (values, _) = merged_copiable_values(&state, &[top, marked], top).unwrap();
        assert!(
            !values.replacement_definitions.contains(&die_exile),
            "CR 707.2: merge/mutate must not copy a target-bound turn-long die-exile rider"
        );
        assert!(
            values.replacement_definitions.contains(&printed),
            "CR 707.2: genuine printed replacements remain copiable"
        );
    }
}
