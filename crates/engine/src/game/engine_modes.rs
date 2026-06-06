use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCast, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaCost;

use super::ability_utils::{
    ability_target_legality_needs_chosen_x, assign_targets_in_chain,
    auto_select_targets_for_ability, begin_target_selection_for_ability, build_chained_resolved,
    build_target_slots_labelled, flatten_targets_in_chain, random_select_targets_for_ability,
    record_modal_mode_choices, target_constraints_from_modal, validate_modal_indices,
};
use super::engine::EngineError;
use super::engine_stack;
use super::restrictions;
use super::triggers;
use super::{casting, casting_costs};

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
                mode_abilities,
                indices,
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
                mode_abilities,
                indices,
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
    /// CR 700.2: the card's mode definitions and the chosen indices, carried so
    /// per-slot mode labels can be built at the SAME post-flush point as slots
    /// (Finding 4 — slot count is state-dependent; the two vectors must come
    /// from one `build_target_slots_labelled` call).
    mode_abilities: Vec<crate::types::ability::AbilityDefinition>,
    indices: Vec<usize>,
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
        mode_abilities,
        indices,
    } = choice;

    let target_constraints = target_constraints_from_modal(&modal);

    // CR 602.2b + CR 601.2b/c: Activating an ability follows the spell
    // announcement steps. If an activated modal ability's target legality depends
    // on an {X} activation cost, choose X after modes and before targets, then
    // resume through the same deferred target-selection path modal spells use so
    // per-mode labels and X-dependent legality stay in sync.
    if ability_target_legality_needs_chosen_x(&resolved) {
        if let Some(cost) = ability_cost.as_ref() {
            if let Some((mana_cost, remaining)) = casting_costs::extract_x_mana_cost(cost) {
                let mut pending_x = PendingCast::new(source_id, CardId(0), resolved, mana_cost);
                pending_x.activation_cost = remaining;
                pending_x.activation_ability_index = ability_index;
                pending_x.target_constraints = target_constraints;
                pending_x.deferred_target_selection = true;
                let mut chosen_modes = indices.clone();
                chosen_modes.sort_unstable();
                pending_x.chosen_modes = chosen_modes;
                state.pending_cast = Some(Box::new(pending_x));
                return casting_costs::enter_payment_step(state, player, None, events);
            }
        }
    }

    super::layers::flush_layers(state);

    // CR 700.2 / CR 601.2b: Build slots and per-mode labels together against the
    // SAME post-flush state (Finding 4 — never let the two vectors diverge in
    // length). `resolved.context` is the chained ability's context, reapplied
    // per-mode by the labelled builder.
    let (target_slots, mode_labels) = build_target_slots_labelled(
        state,
        &mode_abilities,
        &indices,
        &modal.mode_descriptions,
        source_id,
        player,
        &resolved.context,
        resolved.chosen_x,
    )?;

    if !target_slots.is_empty() {
        // CR 115.1 + CR 701.9b: Random-target modal activated abilities — the
        // game picks each target via `state.rng`. Same auto-resolve shape as the
        // controller-choice degenerate path; routes to push without prompting.
        let resolved_targets = if matches!(
            resolved.target_selection_mode,
            crate::types::ability::TargetSelectionMode::Random
        ) {
            Some(random_select_targets_for_ability(
                state,
                &target_slots,
                &target_constraints,
            )?)
        } else {
            auto_select_targets_for_ability(state, &resolved, &target_slots, &target_constraints)?
        };

        if let Some(targets) = resolved_targets {
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;

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
                mode_labels,
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

    events.push(GameEvent::AbilityActivated {
        player_id: player,
        source_id,
    });
    // CR 702.142b: Emit additional event when a boast ability is activated.
    if let Some(index) = ability_index {
        super::casting_targets::emit_keyword_ability_event_if_tagged(
            state, source_id, index, player, events,
        );
    }
    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::Priority { player })
}

struct TriggeredModeChoice {
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    resolved: crate::types::ability::ResolvedAbility,
    modal: crate::types::ability::ModalChoice,
    /// CR 700.2b: mode definitions + chosen indices, carried so per-slot mode
    /// labels build from the same state as the slots (Finding 4).
    mode_abilities: Vec<crate::types::ability::AbilityDefinition>,
    indices: Vec<usize>,
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
        mode_abilities,
        indices,
    } = choice;

    let mut trigger = state
        .pending_trigger
        .take()
        .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
    // CR 700.2 / CR 700.2b: slots + per-mode labels built together (Finding 4).
    let (target_slots, mode_labels) = build_target_slots_labelled(
        state,
        &mode_abilities,
        &indices,
        &modal.mode_descriptions,
        source_id,
        player,
        &resolved.context,
        // CR 107.1b: Triggered abilities don't use a chosen X here.
        None,
    )?;
    let target_constraints = target_constraints_from_modal(&modal);

    trigger.ability = resolved;
    trigger.target_constraints = target_constraints.clone();
    trigger.modal = None;
    trigger.mode_abilities.clear();

    if !target_slots.is_empty() {
        // CR 115.1 + CR 701.9b: Random-target triggered abilities — game picks
        // via `state.rng` instead of prompting the controller.
        let resolved_targets = if matches!(
            trigger.ability.target_selection_mode,
            crate::types::ability::TargetSelectionMode::Random
        ) {
            Some(random_select_targets_for_ability(
                state,
                &target_slots,
                &target_constraints,
            )?)
        } else {
            auto_select_targets_for_ability(
                state,
                &trigger.ability,
                &target_slots,
                &target_constraints,
            )?
        };

        if let Some(targets) = resolved_targets {
            let mut resolved = trigger.ability.clone();
            assign_targets_in_chain(state, &mut resolved, &targets)?;
            // CR 113.2c + CR 603.2 + CR 603.3b: `finalize_trigger_target_selection`
            // already drains the deferred-trigger queue and surfaces the next
            // WaitingFor if a sibling trigger needs input; use that result
            // instead of falling through to Priority below.
            return Ok(engine_stack::finalize_trigger_target_selection(
                state, trigger, resolved, events,
            ));
        } else {
            // CR 601.2c + CR 603.3d: Mode chosen but target choice still
            // outstanding. The entry is already on the stack (pushed at modal
            // pause-time); mutate its ability with the resolved mode so the
            // target prompt operates on the chosen mode. `pending_trigger_entry`
            // stays set — construction continues through target selection.
            triggers::mutate_pending_trigger_entry(state, &trigger.ability);
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
            // CR 601.2c + CR 603.3d + CR 109.5: a targeted "of their choice" trigger
            // routes target selection to the scoped (upkeep) player, not the source's
            // controller. Magus is non-modal so this is defensive class-consistency
            // with the non-modal path in `begin_pending_trigger_target_selection`.
            let player = pending_trigger
                .ability
                .target_chooser
                .as_ref()
                .and_then(|f| {
                    crate::game::targeting::resolve_effect_player_ref(
                        state,
                        &pending_trigger.ability,
                        f,
                    )
                })
                .unwrap_or(player);
            return Ok(WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                mode_labels,
                target_constraints,
                selection,
                source_id: Some(source_id),
                description,
            });
        }
    } else {
        // CR 603.3c: Mode chosen and no further input needed. Entry is already
        // on the stack (pushed at modal pause-time); mutate its ability with
        // the resolved mode and clear `pending_trigger_entry` so the resolver
        // may fire this entry.
        triggers::finalize_pending_trigger_entry(state, &trigger.ability);
        state.priority_passes.clear();
        state.priority_pass_count = 0;
        // CR 113.2c + CR 603.2 + CR 603.3b: Drain siblings deferred behind this
        // modal trigger so each independent instance reaches the stack
        // (issue #416).
        debug_assert!(
            !triggers::is_pending_trigger_construction_active(state),
            "deferred-trigger drain entered with construction still active",
        );
        if let Some(waiting_for) = triggers::drain_deferred_trigger_queue(state, events) {
            return Ok(waiting_for);
        }
    }

    Ok(WaitingFor::Priority { player })
}
