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

use std::sync::Arc;

use crate::game::printed_cards::intrinsic_copiable_values;
use crate::types::ability::{ContinuousModification, CopiableValues, Duration, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
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
    // CR 730.2e: a merged permanent's face-down status follows its topmost
    // component. Read it BEFORE any mutation so the survivor adopts the topmost
    // component's face-down status while merged.
    let topmost_is_face_down = state.objects.get(&topmost_id).is_some_and(|o| o.face_down);

    // Remove any previous mutate copy effect before deriving the new one, so a
    // re-merge where the survivor remains topmost reads the survivor's intrinsic
    // base values rather than the prior merged form.
    remove_merge_layer_effect(state, target_id);

    let Some((values, display_source, printed_ref, token_image_ref)) =
        merged_copiable_values(state, &ordered, topmost_id)
    else {
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

    install_merge_layer_effect(
        state,
        target_id,
        controller,
        values,
        display_source,
        printed_ref,
        token_image_ref,
    );

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
) -> Option<(
    CopiableValues,
    crate::game::game_object::DisplaySource,
    Option<crate::types::card::PrintedCardRef>,
    Option<crate::types::card::TokenImageRef>,
)> {
    let topmost = state.objects.get(&topmost_id)?;
    let printed_ref = topmost
        .base_printed_ref
        .clone()
        .or_else(|| topmost.printed_ref.clone());
    // CR 730.2a: the merged permanent presents the topmost component's identity,
    // so its art routing follows the topmost too (a token-topmost mutate carries
    // the token's `display_source`/`token_image_ref`, not just `printed_ref`).
    let display_source = topmost.display_source;
    let token_image_ref = topmost.token_image_ref.clone();
    let mut values = crate::game::layers::compute_current_copiable_values(state, topmost_id)
        .unwrap_or_else(|| intrinsic_copiable_values(topmost));
    let mut abilities = Vec::new();
    let mut triggers = Vec::new();
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
        statics.extend(stat.iter().cloned());
        // CR 707.2 / CR 611.2b: merged copiable values are printed/defining
        // characteristics, not the runtime "for as long as you control ~" locks
        // another permanent installed on a component. Those gated defs live in
        // base only for layer-reset survival; exclude them from this
        // copiable-values surface (mirrors `intrinsic_copiable_values`) so a
        // merged permanent does not inherit a component host's runtime lock.
        replacements.extend(
            repl.iter()
                .filter(|def| {
                    !crate::game::printed_cards::is_runtime_control_gated_replacement(def)
                })
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
    values.static_definitions = Arc::new(statics);
    values.replacement_definitions = Arc::new(replacements);
    values.keywords = keywords;
    Some((values, display_source, printed_ref, token_image_ref))
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
    display_source: crate::game::game_object::DisplaySource,
    printed_ref: Option<crate::types::card::PrintedCardRef>,
    token_image_ref: Option<crate::types::card::TokenImageRef>,
) {
    let effect_id = state.add_transient_continuous_effect(
        target_id,
        controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: target_id },
        vec![ContinuousModification::CopyValues {
            values: Box::new(values),
            display_source,
            printed_ref,
            token_image_ref,
        }],
        None,
    );
    if let Some(obj) = state.objects.get_mut(&target_id) {
        obj.merge_layer_effect_id = Some(effect_id);
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
/// CR 730.3d (replacement propagation): `dest` is the merged permanent's
/// *resolved* destination — the merged-permanent leave is a single ZoneChange
/// event consulted ONCE through `replace_event` (on the survivor) before
/// `zones::move_to_zone` reaches this seam, so `dest` already reflects any
/// applied `Moved` redirect (Rest in Peace / Leyline of the Void:
/// graveyard → exile). Routing every component to that same resolved `dest`
/// "applies one replacement effect to the object [and thereby] to all components
/// of the object" — exactly CR 730.3d. Components are explicitly NOT re-consulted
/// per component here (`put_component_into_zone` routes raw); re-consulting would
/// double-apply ordering and is rules-wrong per 730.3d.
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
/// CR 903.9c (merged commander zone redirect): when a commander component is
/// part of a merged permanent and the whole pile moves to hand or library (CR
/// 903.9b destination), `put_component_into_zone` places the commander
/// component in the destination zone with its `is_commander` flag intact. The
/// next SBA pass calls `commander::commander_eligible_for_zone_return`, which
/// covers `Zone::Hand | Zone::Library` (CR 903.9b), and presents
/// `WaitingFor::CommanderZoneChoice` to the owner. On accept the commander
/// component is moved to `Zone::Command` via `zones::move_to_zone` — the same
/// path used for standalone commanders. Non-commander components remain in the
/// destination zone. This function does not need special-case commander logic;
/// the SBA path handles it identically to the standalone case.
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

    // CR 730.3 + CR 400.7: before the surviving object changes zone, drop the
    // merge's layer-1 copy effect and flush layers so it leaves as its own card.
    remove_merge_layer_effect(state, merged_id);
    crate::game::layers::flush_layers(state);

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
        put_component_into_zone(state, component_id, component_dest, events);
        // CR 730.3c: record which surviving object this component split from, so an
        // effect that later finds "the object the merged permanent became" (a
        // flicker/blink return) brings this component back too, not just the
        // survivor. See `expand_returned_merge_components`.
        if let Some(obj) = state.objects.get_mut(&component_id) {
            obj.split_from_merge_survivor = Some(merged_id);
        }
    }

    // The surviving object's merge identity is cleared by its own
    // `reset_for_battlefield_exit` during the subsequent `move_to_zone`.
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
/// Given the objects a *continuity* reference resolved to, append each survivor's
/// co-departed sibling components that are still co-located in the same zone, in
/// deterministic id order. The caller (the `ChangeZone` return loop) then applies
/// its move to the whole pile; the components return as separate, non-merged
/// objects (CR 730.3 — merging is not re-established) and their back-links clear
/// on battlefield entry.
///
/// This is a no-op unless `target_filter` is a continuity reference AND a
/// resolved object is a former merged survivor with co-located components. In
/// particular it does NOT fire for a freshly chosen target (e.g. reanimating one
/// specific card from a graveyard), which must not over-return.
pub(crate) fn expand_returned_merge_components(
    state: &GameState,
    resolved: Vec<ObjectId>,
    target_filter: &TargetFilter,
) -> Vec<ObjectId> {
    if !references_object_that_left(target_filter) {
        return resolved;
    }
    let mut expanded = resolved.clone();
    for &survivor_id in &resolved {
        let components = co_split_components(state, survivor_id, &expanded);
        expanded.extend(components);
    }
    expanded
}

/// CR 730.3c: The component cards that the merged permanent identified by
/// `survivor_id` split into when it left the battlefield (CR 730.3), and that are
/// still co-located with the survivor in its current (off-battlefield) zone —
/// returned in deterministic id order, omitting any id already in `exclude`.
///
/// Empty unless `survivor_id` is a former merged survivor with components still
/// in its zone. Shared by both return paths that "find the object the merged
/// permanent became": the `ChangeZone` continuity-reference return
/// ([`expand_returned_merge_components`], flicker/blink) and the
/// `UntilSourceLeaves` implicit return in `engine::check_exile_returns`
/// (exile-until-this-leaves / "O-Ring" effects).
pub(crate) fn co_split_components(
    state: &GameState,
    survivor_id: ObjectId,
    exclude: &[ObjectId],
) -> Vec<ObjectId> {
    let Some(zone) = state.objects.get(&survivor_id).map(|o| o.zone) else {
        return Vec::new();
    };
    // Split-out components are never independent members of the battlefield
    // (CR 730.2) — only an off-battlefield survivor can have co-located ones.
    if zone == Zone::Battlefield {
        return Vec::new();
    }
    let mut components: Vec<ObjectId> = state
        .objects
        .iter()
        .filter(|(id, obj)| {
            obj.split_from_merge_survivor == Some(survivor_id)
                && obj.zone == zone
                && !exclude.contains(*id)
        })
        .map(|(id, _)| *id)
        .collect();
    components.sort_by_key(|id| id.0);
    components
}

/// CR 730.3c: Target references that denote "the object that just left the
/// battlefield" — i.e. continuity references that find the object a merged
/// permanent became — as opposed to a freshly chosen target. Only these expand
/// to a former merged permanent's component cards.
fn references_object_that_left(target_filter: &TargetFilter) -> bool {
    matches!(
        target_filter,
        TargetFilter::ParentTarget
            | TargetFilter::ParentTargetSlot { .. }
            | TargetFilter::TrackedSet { .. }
            | TargetFilter::TrackedSetFiltered { .. }
            | TargetFilter::TriggeringSource
    )
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
fn put_component_into_zone(
    state: &mut GameState,
    component_id: ObjectId,
    dest: Zone,
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

    let turn_zone_change_index =
        crate::game::restrictions::record_zone_change(state, record.clone());
    record.turn_zone_change_index = turn_zone_change_index;
    events.push(GameEvent::ZoneChanged {
        object_id: component_id,
        from: None,
        to: dest,
        record: Box::new(record),
    });
}

/// CR 702.140c + CR 730.2a: Resolve the controller's top/bottom choice for a
/// paused mutating creature spell. Consumes `state.pending_mutate_merge`, performs
/// the merge, and returns the engine to priority. Errors if no merge is pending or
/// the acting player is not the spell's controller.
pub fn handle_mutate_merge_choice(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    side: MergeSide,
    events: &mut Vec<GameEvent>,
) -> Result<crate::types::game_state::WaitingFor, crate::game::engine::EngineError> {
    use crate::game::engine::EngineError;

    let pending = state
        .pending_mutate_merge
        .take()
        .ok_or_else(|| EngineError::ActionNotAllowed("No mutate merge is pending".to_string()))?;
    if pending.controller != player {
        // Restore the pending state so the correct player can still act.
        state.pending_mutate_merge = Some(pending);
        return Err(EngineError::ActionNotAllowed(
            "Only the mutate spell's controller may choose the merge side".to_string(),
        ));
    }

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
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, FaceDownProfile, QuantityExpr, TargetFilter,
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

        let (values, _, _, _) = merged_copiable_values(&state, &[top, buried], top).unwrap();
        assert!(values.keywords.contains(&Keyword::Flying)); // allow-raw-authority: merged_copiable_values snapshot struct, not a GameObject
        assert!(
            !values.keywords.contains(&ward_2()), // allow-raw-authority: merged_copiable_values snapshot struct, not a GameObject
            "non-topmost face-down component's ward is suppressed (CR 708.2a + CR 730.2e)"
        );

        // NEGATIVE: a non-topmost FACE-UP component IS unioned.
        let buried_up = make_creature(&mut state, 2, player, "BuriedUp", 1, 1);
        state.objects.get_mut(&buried_up).unwrap().base_keywords = vec![Keyword::Trample];
        let (values2, _, _, _) = merged_copiable_values(&state, &[top, buried_up], top).unwrap();
        assert!(
            values2.keywords.contains(&Keyword::Trample), // allow-raw-authority: merged_copiable_values snapshot struct, not a GameObject
            "a face-up non-topmost component's keywords are unioned (CR 702.140e)"
        );
    }
}
