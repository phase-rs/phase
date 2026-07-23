use crate::types::ability::{ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, TargetSelectionConstraint, TargetSelectionSlot, WaitingFor,
};
use crate::types::player::PlayerId;

use super::ability_utils::{
    assign_selected_slots_in_chain, assign_targets_in_chain, choose_target_for_ability,
    distribution_targets, flatten_targets_in_chain, validate_selected_targets_for_ability,
    TargetSelectionAdvance,
};
use super::casting_targets::extract_distribution_total;
use super::effects;
use super::engine::{resume_pending_continuation_if_priority, EngineError};
use super::triggers::PendingTrigger;
use super::{casting, priority, triggers};

pub(super) fn finalize_trigger_target_selection(
    state: &mut GameState,
    trigger: PendingTrigger,
    ability: ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    let assigned_targets = flatten_targets_in_chain(&ability);
    casting::emit_targeting_events(
        state,
        &assigned_targets,
        trigger.source_id,
        trigger.controller,
        events,
    );

    // CR 601.2d: Division is announced only among the distributing effect's own targets, not sibling-effect targets (which still become targets above).
    let dist_targets = distribution_targets(&ability);

    let mut trigger = trigger;
    let controller = trigger.controller;
    let distribute = trigger.distribute.clone();
    trigger.ability = ability;

    // CR 601.2d + CR 603.3d: When a triggered ability divides damage or
    // counters among its targets, the controller announces that division while
    // putting the ability on the stack, after targets have been chosen.
    if let Some(unit) = distribute {
        if let Some(total) =
            extract_distribution_total(state, &trigger.ability, &trigger.ability.effect)
        {
            if dist_targets.len() == 1 {
                trigger.ability.distribution = Some(vec![(dist_targets[0].clone(), total)]);
            } else {
                // CR 601.2d: Distribution still outstanding. Entry is already
                // on the stack with empty `distribution`; mutate the on-stack
                // ability's targets (so they match what was just chosen) and
                // keep `pending_trigger_entry` set until division completes.
                if !triggers::mutate_pending_trigger_entry(state, &trigger.ability) {
                    // Unexpected dangling cursor: the entry is gone before the
                    // division prompt could open. Recover per CR 608.2b / CR
                    // 800.4a (a stack object that has left the stack does not
                    // resolve) — record the diagnostic, abandon, hand back
                    // priority. Matches the DistributeAmong-return convention
                    // below; the next priority pass re-normalizes (CR 117.3b
                    // would give the active player).
                    triggers::abandon_ceased_pending_trigger(state, &trigger.ability);
                    priority::clear_priority_passes(state);
                    return WaitingFor::Priority { player: controller };
                }
                state.pending_trigger = Some(trigger);
                priority::clear_priority_passes(state);
                return WaitingFor::DistributeAmong {
                    player: controller,
                    total,
                    targets: dist_targets,
                    unit,
                };
            }
        }
    }

    // CR 603.3c + CR 603.3d: Construction complete. The entry is already on
    // the stack (pushed by the pause-path that started selection); mutate its
    // ability with the resolved targets/distribution and clear
    // `pending_trigger_entry` so the resolver may now fire this entry.
    if !triggers::finalize_pending_trigger_entry(state, &trigger.ability) {
        // Unexpected dangling cursor: the entry is no longer on the stack.
        // Recover per CR 608.2b / CR 800.4a (a stack object that has left the
        // stack does not resolve) — record the diagnostic, abandon the dead
        // trigger, and hand control back rather than panic. Returns Priority for
        // the controller (matching the DistributeAmong convention above); the
        // next priority pass re-normalizes (CR 117.3b would give active player).
        triggers::abandon_ceased_pending_trigger(state, &trigger.ability);
        priority::clear_priority_passes(state);
        return WaitingFor::Priority { player: controller };
    }

    priority::clear_priority_passes(state);
    // CR 113.2c + CR 603.2 + CR 603.3b: After the active trigger is on the
    // stack, drain any siblings that were deferred because this one needed
    // input (e.g., the second Boggart Prankster's "you attack" trigger waiting
    // behind the first). If a deferred trigger itself needs input, hand back
    // its WaitingFor; otherwise continue to Priority.
    debug_assert!(
        !triggers::is_pending_trigger_construction_active(state),
        "deferred-trigger drain entered with construction still active",
    );
    if let Some(waiting_for) =
        triggers::drain_deferred_triggers_after_trigger_construction(state, events)
    {
        return waiting_for;
    }
    WaitingFor::Priority { player: controller }
}

/// CR 706.2 + CR 603.12: Re-stamp the pending trigger's carried die-roll
/// result into resolution scope before computing or validating targets, so a
/// dynamic `TargetSelectionConstraint::TotalManaValue` cap whose value is
/// `EventContextAmount` ("where X is the result") resolves against the rolled
/// number during the legality check or the step-by-step choose walk. The next
/// `apply()` clears `die_result_this_resolution`, so this cannot leak.
fn restamp_pending_die_result(state: &mut GameState) {
    state.die_result_this_resolution = state.pending_trigger.as_ref().and_then(|t| t.die_result);
}

pub(super) fn handle_trigger_target_selection_select_targets(
    state: &mut GameState,
    _player: PlayerId,
    target_slots: &[TargetSelectionSlot],
    target_constraints: &[TargetSelectionConstraint],
    targets: Vec<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    restamp_pending_die_result(state);
    let Some(pending) = state.pending_trigger.as_ref() else {
        return Err(EngineError::InvalidAction("No pending trigger".to_string()));
    };
    let mut ability = pending.ability.clone();
    // Read the firing batch's subject count out of `pending` before any mutation
    // of `state`, so the shared borrow of `state.pending_trigger` ends here.
    let pending_trigger_event = pending.trigger_event.clone();
    let pending_trigger_events = if state.pending_trigger_event_batch.is_empty() {
        pending_trigger_event.iter().cloned().collect::<Vec<_>>()
    } else {
        state.pending_trigger_event_batch.clone()
    };
    let pending_match_count = pending.subject_match_count;
    // CR 601.2c + CR 603.2c: A variable target count ("up to X target creatures,
    // where X is the number milled this way") is fixed when the ability is put on
    // the stack and does not change. Re-stamp the full firing event context into
    // resolution scope so target legality recomputation sees the same event player
    // and subject count as the original prompt. This must wrap BOTH validation and
    // assignment: each recomputes `target_slot_specs`. Save/restore (not a bare
    // stamp) because trigger context is resolution-local and must not leak into the
    // next action. Mirrors the choose-target walk and the auto-target path's
    // push/restore_trigger_event_context in triggers.rs.
    let context_snapshot = super::triggers::push_trigger_event_context(
        state,
        pending_trigger_event.as_ref(),
        &pending_trigger_events,
        pending_match_count,
    );
    let select_result = match validate_selected_targets_for_ability(
        state,
        &ability,
        target_slots,
        &targets,
        target_constraints,
    ) {
        Ok(()) => assign_targets_in_chain(state, &mut ability, &targets),
        Err(e) => Err(e),
    };
    super::triggers::restore_trigger_event_context(state, context_snapshot);
    select_result?;
    // CR 603.3d: Consume the pending trigger only after the fallible assignment
    // succeeds. `apply()` does not roll back on Err and `sync_waiting_for` never
    // runs after an Err, so taking the trigger before assignment would strand
    // `waiting_for = TriggerTargetSelection` with no pending trigger, bricking
    // every later action. Taking after success leaves state recoverable.
    let trigger = state
        .pending_trigger
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;

    Ok(finalize_trigger_target_selection(
        state, trigger, ability, events,
    ))
}

pub(super) fn handle_trigger_target_selection_choose_target(
    state: &mut GameState,
    waiting_for: WaitingFor,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (
        player,
        trigger_controller,
        trigger_event,
        trigger_events,
        target_slots,
        mode_labels,
        target_constraints,
        selection,
        source_id,
        description,
    ) = match waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            trigger_controller,
            trigger_event,
            trigger_events,
            target_slots,
            mode_labels,
            target_constraints,
            selection,
            source_id,
            description,
        } => (
            player,
            trigger_controller,
            trigger_event,
            trigger_events,
            target_slots,
            mode_labels,
            target_constraints,
            selection,
            source_id,
            description,
        ),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for trigger target selection".to_string(),
            ));
        }
    };

    restamp_pending_die_result(state);

    let Some(pending_trigger) = state.pending_trigger.as_ref() else {
        return Err(EngineError::InvalidAction("No pending trigger".to_string()));
    };
    // Clone the ability and read the firing batch's subject count before mutating
    // `current_trigger_match_count`, ending the shared borrow of `pending_trigger`.
    let walk_ability = pending_trigger.ability.clone();
    let pending_trigger_event = pending_trigger.trigger_event.clone();
    let pending_trigger_events = if state.pending_trigger_event_batch.is_empty() {
        pending_trigger_event.iter().cloned().collect::<Vec<_>>()
    } else {
        state.pending_trigger_event_batch.clone()
    };
    let pending_match_count = pending_trigger.subject_match_count;

    // CR 601.2c + CR 603.2c: Re-stamp the firing batch's subject count into
    // resolution scope for the ENTIRE step-by-step walk, not only final assignment.
    // Each `ChooseTarget` builds the *next* slot's legal-target set via
    // `build_target_selection_progress_for_ability`, which recomputes
    // `target_slot_specs` and so re-resolves `multi_target.max = EventContextAmount`.
    // With `current_trigger_match_count == None` mid-walk the bounds collapse to 0,
    // the per-slot `TargetSlotSpec`s vanish, and the CR 601.2c same-instance
    // distinctness filter is silently bypassed via the `slot.legal_targets`
    // fallback in `legal_targets_for_spec_slot`. Save/restore (not a bare stamp)
    // because `current_trigger_match_count` is not cleared at `apply()` start.
    // Mirrors the Complete branch below and the auto-target path's
    // push/restore_trigger_event_context in triggers.rs.
    let context_snapshot = super::triggers::push_trigger_event_context(
        state,
        pending_trigger_event.as_ref(),
        &pending_trigger_events,
        pending_match_count,
    );
    let advance = choose_target_for_ability(
        state,
        &walk_ability,
        &target_slots,
        &target_constraints,
        &selection,
        target,
    );
    super::triggers::restore_trigger_event_context(state, context_snapshot);

    match advance? {
        // CR 700.2b: preserve the inbound mode labels unchanged across the
        // step-by-step walk — the slot→mode mapping does not change.
        TargetSelectionAdvance::InProgress(selection) => Ok(WaitingFor::TriggerTargetSelection {
            player,
            trigger_controller,
            trigger_event,
            trigger_events,
            target_slots,
            mode_labels,
            target_constraints,
            selection,
            source_id,
            description,
        }),
        TargetSelectionAdvance::Complete(selected_slots) => {
            let Some(pending) = state.pending_trigger.as_ref() else {
                return Err(EngineError::InvalidAction("No pending trigger".to_string()));
            };
            let mut ability = pending.ability.clone();
            // Read the firing batch's subject count out of `pending` before any
            // mutation of `state`, so the shared borrow of `state.pending_trigger`
            // ends here.
            let pending_trigger_event = pending.trigger_event.clone();
            let pending_trigger_events = if state.pending_trigger_event_batch.is_empty() {
                pending_trigger_event.iter().cloned().collect::<Vec<_>>()
            } else {
                state.pending_trigger_event_batch.clone()
            };
            let pending_match_count = pending.subject_match_count;
            // CR 601.2c + CR 603.2c: A variable target count ("up to X target
            // creatures, where X is the number milled this way") is fixed when the
            // ability is put on the stack and does not change. Re-stamp the firing
            // batch's subject count into resolution scope so `multi_target.max =
            // EventContextAmount` resolves to that count on this later `apply()`
            // instead of collapsing to 0. Save/restore (not a bare stamp) because
            // `current_trigger_match_count` is not cleared at `apply()` start, so a
            // bare stamp would leak into the next resolution. Mirrors the
            // auto-target path's push/restore_trigger_event_context in triggers.rs.
            let context_snapshot = super::triggers::push_trigger_event_context(
                state,
                pending_trigger_event.as_ref(),
                &pending_trigger_events,
                pending_match_count,
            );
            let assign_result =
                assign_selected_slots_in_chain(state, &mut ability, &selected_slots);
            super::triggers::restore_trigger_event_context(state, context_snapshot);
            assign_result?;
            // CR 603.3d: Consume the pending trigger only after the fallible
            // assignment succeeds. `apply()` does not roll back on Err and
            // `sync_waiting_for` never runs after an Err, so taking the trigger
            // before assignment would strand `waiting_for = TriggerTargetSelection`
            // with no pending trigger, bricking every later action. Taking after
            // success leaves state recoverable.
            let trigger = state
                .pending_trigger
                .take()
                .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;

            Ok(finalize_trigger_target_selection(
                state, trigger, ability, events,
            ))
        }
    }
}

pub(super) fn handle_multi_target_selection(
    state: &mut GameState,
    waiting_for: WaitingFor,
    selected: &[crate::types::identifiers::ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (player, legal_targets, min_targets, max_targets, pending_ability) = match waiting_for {
        WaitingFor::MultiTargetSelection {
            player,
            legal_targets,
            min_targets,
            max_targets,
            pending_ability,
        } => (
            player,
            legal_targets,
            min_targets,
            max_targets,
            pending_ability.as_ref().clone(),
        ),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for multi-target selection".to_string(),
            ));
        }
    };

    if selected.len() < min_targets || selected.len() > max_targets {
        return Err(EngineError::InvalidAction(format!(
            "Must select between {} and {} targets, got {}",
            min_targets,
            max_targets,
            selected.len()
        )));
    }

    for id in selected {
        if !legal_targets.contains(id) {
            return Err(EngineError::InvalidAction(
                "Selected target not in legal set".to_string(),
            ));
        }
    }

    let mut ability = pending_ability;
    ability.targets = selected.iter().map(|&id| TargetRef::Object(id)).collect();

    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = player;
    let _ = effects::resolve_ability_chain(state, &ability, events, 0);
    resume_pending_continuation_if_priority(state, events)?;

    Ok(state.waiting_for.clone())
}
