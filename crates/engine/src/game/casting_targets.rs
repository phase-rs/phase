use crate::types::ability::TargetRef;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    CastingVariant, GameState, PendingCast, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

use super::ability_utils::{
    assign_selected_slots_in_chain, assign_targets_in_chain, auto_select_targets,
    begin_target_selection, build_chained_resolved, build_target_slots, choose_target,
    flatten_targets_in_chain, validate_modal_indices, validate_selected_targets,
    TargetSelectionAdvance,
};
use super::casting::{emit_targeting_events, pay_ability_cost};
use super::casting_costs::check_additional_cost_or_pay;
use super::engine::EngineError;
use super::restrictions;
use super::stack;

/// Handle mode selection for a modal spell.
///
/// Combines chosen mode abilities into a single ResolvedAbility chain (sub_abilities),
/// then proceeds to targeting or directly to payment.
pub(crate) fn handle_select_modes(
    state: &mut GameState,
    player: PlayerId,
    indices: Vec<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (modal, pending) = match &state.waiting_for {
        WaitingFor::ModeChoice {
            modal,
            pending_cast,
            ..
        } => (modal.clone(), *pending_cast.clone()),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for mode selection".to_string(),
            ));
        }
    };

    // Spells resolve once — no cross-resolution mode constraints apply.
    validate_modal_indices(&modal, &indices, &[])?;

    // CR 702.172b: Spree mode costs are additional costs — sum chosen modes and add to base cost.
    // TODO CR 702.172b: When "cast without paying mana cost" is implemented, Spree mode costs
    // must be paid separately (additional costs are not waived). Refactor to separate cost tracking.
    let total_cost = if modal.mode_costs.is_empty() {
        pending.cost.clone()
    } else {
        let spree_total = indices
            .iter()
            .fold(crate::types::mana::ManaCost::zero(), |acc, &idx| {
                restrictions::add_mana_cost(&acc, &modal.mode_costs[idx])
            });
        restrictions::add_mana_cost(&pending.cost, &spree_total)
    };

    // Get the card's abilities to build combined resolved ability from chosen modes
    let obj = state
        .objects
        .get(&pending.object_id)
        .ok_or_else(|| EngineError::InvalidAction("Modal spell object not found".to_string()))?;
    let abilities = obj.abilities.clone();

    // Build a chain of ResolvedAbility from chosen modes (in order)
    let resolved = build_chained_resolved(&abilities, &indices, pending.object_id, player)?;

    // Check for targeting on the combined ability
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &pending.target_constraints)? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;
            return check_additional_cost_or_pay(
                state,
                player,
                pending.object_id,
                pending.card_id,
                resolved,
                &total_cost,
                CastingVariant::Normal,
                events,
            );
        }

        let selection = begin_target_selection(&target_slots, &pending.target_constraints)?;
        let mut pending_sel =
            PendingCast::new(pending.object_id, pending.card_id, resolved, total_cost);
        pending_sel.target_constraints = pending.target_constraints;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending_sel),
            target_slots,
            selection,
        });
    }

    // No targets needed -- check additional cost, then pay
    check_additional_cost_or_pay(
        state,
        player,
        pending.object_id,
        pending.card_id,
        resolved,
        &total_cost,
        CastingVariant::Normal,
        events,
    )
}

/// Handle target selection for a pending cast.
pub(crate) fn handle_select_targets(
    state: &mut GameState,
    player: PlayerId,
    targets: Vec<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Extract PendingCast from WaitingFor::TargetSelection
    let pending = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            ..
        } => {
            validate_selected_targets(target_slots, &targets, &pending_cast.target_constraints)?;
            *pending_cast.clone()
        }
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for target selection".to_string(),
            ));
        }
    };

    let mut ability = pending.ability;
    assign_targets_in_chain(&mut ability, &targets)?;

    if let Some(ability_index) = pending.activation_ability_index {
        if let Some(ref activation_cost) = pending.activation_cost {
            pay_ability_cost(state, player, pending.object_id, activation_cost, events)?;
        }

        let assigned_targets = flatten_targets_in_chain(&ability);
        emit_targeting_events(state, &assigned_targets, pending.object_id, player, events);

        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        stack::push_to_stack(
            state,
            StackEntry {
                id: entry_id,
                source_id: pending.object_id,
                controller: player,
                kind: StackEntryKind::ActivatedAbility {
                    source_id: pending.object_id,
                    ability,
                },
            },
            events,
        );

        restrictions::record_ability_activation(state, pending.object_id, ability_index);
        events.push(GameEvent::AbilityActivated {
            source_id: pending.object_id,
        });
        state.priority_passes.clear();
        state.priority_pass_count = 0;
        return Ok(WaitingFor::Priority { player });
    }

    check_additional_cost_or_pay(
        state,
        player,
        pending.object_id,
        pending.card_id,
        ability,
        &pending.cost,
        CastingVariant::Normal,
        events,
    )
}

pub(crate) fn handle_choose_target(
    state: &mut GameState,
    player: PlayerId,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (pending, target_slots, selection) = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            selection,
            ..
        } => (
            *pending_cast.clone(),
            target_slots.clone(),
            selection.clone(),
        ),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for target selection".to_string(),
            ));
        }
    };

    match choose_target(
        &target_slots,
        &pending.target_constraints,
        &selection,
        target,
    )? {
        TargetSelectionAdvance::InProgress(selection) => Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending),
            target_slots,
            selection,
        }),
        TargetSelectionAdvance::Complete(selected_slots) => {
            let mut ability = pending.ability;
            assign_selected_slots_in_chain(&mut ability, &selected_slots)?;

            if let Some(ability_index) = pending.activation_ability_index {
                if let Some(ref activation_cost) = pending.activation_cost {
                    pay_ability_cost(state, player, pending.object_id, activation_cost, events)?;
                }

                let assigned_targets = flatten_targets_in_chain(&ability);
                emit_targeting_events(state, &assigned_targets, pending.object_id, player, events);

                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                stack::push_to_stack(
                    state,
                    StackEntry {
                        id: entry_id,
                        source_id: pending.object_id,
                        controller: player,
                        kind: StackEntryKind::ActivatedAbility {
                            source_id: pending.object_id,
                            ability,
                        },
                    },
                    events,
                );

                restrictions::record_ability_activation(state, pending.object_id, ability_index);
                events.push(GameEvent::AbilityActivated {
                    source_id: pending.object_id,
                });
                state.priority_passes.clear();
                state.priority_pass_count = 0;
                return Ok(WaitingFor::Priority { player });
            }

            check_additional_cost_or_pay(
                state,
                player,
                pending.object_id,
                pending.card_id,
                ability,
                &pending.cost,
                CastingVariant::Normal,
                events,
            )
        }
    }
}
