use crate::types::ability::{ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, TargetSelectionConstraint, TargetSelectionSlot, WaitingFor,
};
use crate::types::player::PlayerId;

use super::ability_utils::{
    assign_selected_slots_in_chain, assign_targets_in_chain, choose_target_for_ability,
    flatten_targets_in_chain, validate_selected_targets_for_ability, TargetSelectionAdvance,
};
use super::casting_targets::extract_distribution_total;
use super::effects;
use super::engine::{resume_pending_continuation_if_priority, EngineError};
use super::triggers::PendingTrigger;
use super::{casting, triggers};

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
            if assigned_targets.len() == 1 {
                trigger.ability.distribution = Some(vec![(assigned_targets[0].clone(), total)]);
            } else {
                // CR 601.2d: Distribution still outstanding. Entry is already
                // on the stack with empty `distribution`; mutate the on-stack
                // ability's targets (so they match what was just chosen) and
                // keep `pending_trigger_entry` set until division completes.
                triggers::mutate_pending_trigger_entry(state, &trigger.ability);
                state.pending_trigger = Some(trigger);
                state.priority_passes.clear();
                state.priority_pass_count = 0;
                return WaitingFor::DistributeAmong {
                    player: controller,
                    total,
                    targets: assigned_targets,
                    unit,
                };
            }
        }
    }

    // CR 603.3c + CR 603.3d: Construction complete. The entry is already on
    // the stack (pushed by the pause-path that started selection); mutate its
    // ability with the resolved targets/distribution and clear
    // `pending_trigger_entry` so the resolver may now fire this entry.
    triggers::finalize_pending_trigger_entry(state, &trigger.ability);

    state.priority_passes.clear();
    state.priority_pass_count = 0;
    // CR 113.2c + CR 603.2 + CR 603.3b: After the active trigger is on the
    // stack, drain any siblings that were deferred because this one needed
    // input (e.g., the second Boggart Prankster's "you attack" trigger waiting
    // behind the first). If a deferred trigger itself needs input, hand back
    // its WaitingFor; otherwise continue to Priority.
    debug_assert!(
        !triggers::is_pending_trigger_construction_active(state),
        "deferred-trigger drain entered with construction still active",
    );
    if let Some(waiting_for) = triggers::drain_deferred_trigger_queue(state, events) {
        return waiting_for;
    }
    WaitingFor::Priority { player: controller }
}

pub(super) fn handle_trigger_target_selection_select_targets(
    state: &mut GameState,
    _player: PlayerId,
    target_slots: &[TargetSelectionSlot],
    target_constraints: &[TargetSelectionConstraint],
    targets: Vec<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 706.2 + CR 603.12: Re-stamp the carried die-roll result into
    // resolution scope before validating targets, so a dynamic
    // `TargetSelectionConstraint::TotalManaValue` whose cap is the
    // `where X is the result` value (`EventContextAmount`) resolves against the
    // rolled number during the legality check. The next `apply()` clears
    // `die_result_this_resolution`, so this cannot leak.
    state.die_result_this_resolution = state.pending_trigger.as_ref().and_then(|t| t.die_result);
    let trigger = state
        .pending_trigger
        .as_ref()
        .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
    validate_selected_targets_for_ability(
        state,
        &trigger.ability,
        target_slots,
        &targets,
        target_constraints,
    )?;
    let trigger = state
        .pending_trigger
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
    let mut ability = trigger.ability.clone();
    assign_targets_in_chain(state, &mut ability, &targets)?;

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
    let (player, target_slots, mode_labels, target_constraints, selection, source_id, description) =
        match waiting_for {
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                mode_labels,
                target_constraints,
                selection,
                source_id,
                description,
            } => (
                player,
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

    // CR 706.2 + CR 603.12: Re-stamp the carried die-roll result into resolution
    // scope before computing/validating legal targets, so a dynamic
    // `TotalManaValue` cap (`EventContextAmount` = "where X is the result")
    // resolves against the rolled number during the step-by-step choose walk.
    // Cleared by the next `apply()`, so it cannot leak.
    state.die_result_this_resolution = state.pending_trigger.as_ref().and_then(|t| t.die_result);

    let Some(pending_trigger) = state.pending_trigger.as_ref() else {
        return Err(EngineError::InvalidAction("No pending trigger".to_string()));
    };

    match choose_target_for_ability(
        state,
        &pending_trigger.ability,
        &target_slots,
        &target_constraints,
        &selection,
        target,
    )? {
        // CR 700.2b: preserve the inbound mode labels unchanged across the
        // step-by-step walk — the slot→mode mapping does not change.
        TargetSelectionAdvance::InProgress(selection) => Ok(WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            mode_labels,
            target_constraints,
            selection,
            source_id,
            description,
        }),
        TargetSelectionAdvance::Complete(selected_slots) => {
            let trigger = state
                .pending_trigger
                .take()
                .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
            let mut ability = trigger.ability.clone();
            assign_selected_slots_in_chain(state, &mut ability, &selected_slots)?;

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
