use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCast, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaCost;

use super::ability_utils::{
    assign_targets_in_chain, auto_select_targets_for_ability, begin_target_selection_for_ability,
    build_chained_resolved, build_target_slots, flatten_targets_in_chain,
    record_modal_mode_choices, target_constraints_from_modal, validate_modal_indices,
};
use super::casting;
use super::engine::EngineError;
use super::engine_stack;
use super::restrictions;
use super::triggers;

pub(super) fn handle_ability_mode_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    indices: Vec<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::AbilityModeChoice {
        player,
        modal,
        source_id,
        mode_abilities,
        is_activated,
        ability_index,
        ability_cost,
        unavailable_modes,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for ability mode choice".to_string(),
        ));
    };

    validate_modal_indices(&modal, &indices, &unavailable_modes)?;
    record_modal_mode_choices(state, source_id, &modal, &indices);

    let resolved = build_chained_resolved(&mode_abilities, indices.as_slice(), source_id, player)?;

    if is_activated {
        handle_activated_mode_choice(
            state,
            ActivatedModeChoice {
                player,
                source_id,
                resolved,
                ability_index,
                ability_cost,
                modal,
            },
            events,
        )
    } else {
        handle_triggered_mode_choice(
            state,
            TriggeredModeChoice {
                player,
                source_id,
                resolved,
                modal,
            },
            events,
        )
    }
}

struct ActivatedModeChoice {
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    resolved: crate::types::ability::ResolvedAbility,
    ability_index: Option<usize>,
    ability_cost: Option<crate::types::ability::AbilityCost>,
    modal: crate::types::ability::ModalChoice,
}

fn handle_activated_mode_choice(
    state: &mut GameState,
    choice: ActivatedModeChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let ActivatedModeChoice {
        player,
        source_id,
        resolved,
        ability_index,
        ability_cost,
        modal,
    } = choice;

    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let target_slots = build_target_slots(state, &resolved)?;
    let target_constraints = target_constraints_from_modal(&modal);

    if !target_slots.is_empty() {
        if let Some(targets) =
            auto_select_targets_for_ability(state, &resolved, &target_slots, &target_constraints)?
        {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;

            if let Some(cost) = &ability_cost {
                casting::pay_ability_cost(state, player, source_id, cost, events)?;
            }
            casting::emit_targeting_events(
                state,
                &flatten_targets_in_chain(&resolved),
                source_id,
                player,
                events,
            );

            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            // CR 603.4: Stamp the printed-ability index for per-turn resolution
            // tracking (`AbilityCondition::NthResolutionThisTurn`) before push.
            let mut resolved_with_idx = resolved;
            resolved_with_idx.ability_index = ability_index;
            super::stack::push_to_stack(
                state,
                crate::types::game_state::StackEntry {
                    id: entry_id,
                    source_id,
                    controller: player,
                    kind: crate::types::game_state::StackEntryKind::ActivatedAbility {
                        source_id,
                        ability: resolved_with_idx,
                    },
                },
                events,
            );
            if let Some(index) = ability_index {
                restrictions::record_ability_activation(state, source_id, index);
                // CR 117.1b: Priority permits unbounded activation.
                // `pending_activations` is a per-priority-window AI-guard —
                // see `GameState::pending_activations`.
                state.pending_activations.push((source_id, index));
            }
        } else {
            let selection = begin_target_selection_for_ability(
                state,
                &resolved,
                &target_slots,
                &target_constraints,
            )?;
            let mut pending = PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending.activation_cost = ability_cost;
            pending.activation_ability_index = ability_index;
            pending.target_constraints = target_constraints;
            return Ok(WaitingFor::TargetSelection {
                player,
                pending_cast: Box::new(pending),
                target_slots,
                selection,
            });
        }
    } else {
        if let Some(cost) = &ability_cost {
            casting::pay_ability_cost(state, player, source_id, cost, events)?;
        }
        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        // CR 603.4: Stamp the printed-ability index for per-turn resolution tracking.
        let mut resolved_with_idx = resolved;
        resolved_with_idx.ability_index = ability_index;
        super::stack::push_to_stack(
            state,
            crate::types::game_state::StackEntry {
                id: entry_id,
                source_id,
                controller: player,
                kind: crate::types::game_state::StackEntryKind::ActivatedAbility {
                    source_id,
                    ability: resolved_with_idx,
                },
            },
            events,
        );
        if let Some(index) = ability_index {
            restrictions::record_ability_activation(state, source_id, index);
            // CR 117.1b: Priority permits unbounded activation.
            // `pending_activations` is a per-priority-window AI-guard —
            // see `GameState::pending_activations`.
            state.pending_activations.push((source_id, index));
        }
    }

    events.push(GameEvent::AbilityActivated { source_id });
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::Priority { player })
}

struct TriggeredModeChoice {
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    resolved: crate::types::ability::ResolvedAbility,
    modal: crate::types::ability::ModalChoice,
}

fn handle_triggered_mode_choice(
    state: &mut GameState,
    choice: TriggeredModeChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let TriggeredModeChoice {
        player,
        source_id,
        resolved,
        modal,
    } = choice;

    let mut trigger = state
        .pending_trigger
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
    let target_slots = build_target_slots(state, &resolved)?;
    let target_constraints = target_constraints_from_modal(&modal);

    trigger.ability = resolved;
    trigger.target_constraints = target_constraints.clone();
    trigger.modal = None;
    trigger.mode_abilities.clear();

    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets_for_ability(
            state,
            &trigger.ability,
            &target_slots,
            &target_constraints,
        )? {
            let mut resolved = trigger.ability.clone();
            assign_targets_in_chain(&mut resolved, &targets)?;
            engine_stack::finalize_trigger_target_selection(state, trigger, resolved, events);
        } else {
            let description = trigger.description.clone();
            state.pending_trigger = Some(trigger);
            let pending_trigger = state
                .pending_trigger
                .as_ref()
                .expect("pending trigger stored before target selection");
            let selection = begin_target_selection_for_ability(
                state,
                &pending_trigger.ability,
                &target_slots,
                &target_constraints,
            )?;
            return Ok(WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                target_constraints,
                selection,
                source_id: Some(source_id),
                description,
            });
        }
    } else {
        triggers::push_pending_trigger_to_stack(state, trigger, events);
        state.priority_passes.clear();
        state.priority_pass_count = 0;
    }

    Ok(WaitingFor::Priority { player })
}
