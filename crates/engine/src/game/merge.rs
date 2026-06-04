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
//! Deferred: copy effects targeting a merged permanent, face-down/DFC
//! components, full CR 702.140d downstream reflexive effects, and the CR 730.3a
//! graveyard/library arrange-order UI (a deterministic order is used).

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    if let Some(merging) = state.objects.get_mut(&merging_id) {
        merging.zone = Zone::Battlefield;
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

    // Remove any previous mutate copy effect before deriving the new one, so a
    // re-merge where the survivor remains topmost reads the survivor's intrinsic
    // base values rather than the prior merged form.
    remove_merge_layer_effect(state, target_id);

    let Some((values, printed_ref)) = merged_copiable_values(state, &ordered, topmost_id) else {
        return;
    };

    if let Some(survivor) = state.objects.get_mut(&target_id) {
        survivor.merged_components = ordered;
    }

    install_merge_layer_effect(state, target_id, controller, values, printed_ref);

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
) -> Option<(CopiableValues, Option<crate::types::card::PrintedCardRef>)> {
    let topmost = state.objects.get(&topmost_id)?;
    let printed_ref = topmost
        .base_printed_ref
        .clone()
        .or_else(|| topmost.printed_ref.clone());
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
        replacements.extend(repl.iter().cloned());
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
    Some((values, printed_ref))
}

fn remove_merge_layer_effect(state: &mut GameState, target_id: ObjectId) {
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

fn install_merge_layer_effect(
    state: &mut GameState,
    target_id: ObjectId,
    controller: crate::types::player::PlayerId,
    values: CopiableValues,
    printed_ref: Option<crate::types::card::PrintedCardRef>,
) {
    let effect_id = state.add_transient_continuous_effect(
        target_id,
        controller,
        Duration::Permanent,
        TargetFilter::SpecificObject { id: target_id },
        vec![ContinuousModification::CopyValues {
            values: Box::new(values),
            printed_ref,
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
        // CR 730.3 + S4: route each component to ITS OWN owner's destination zone.
        crate::game::zones::move_to_zone(state, component_id, dest, events);
    }

    // The surviving object's merge identity is cleared by its own
    // `reset_for_battlefield_exit` during the subsequent `move_to_zone`.
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
