use crate::types::events::GameEvent;
use crate::types::game_state::{CostResume, GameState, PayCostKind, PendingCast, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaCost;

use super::ability_utils::{
    ability_target_legality_needs_chosen_x, assign_targets_in_chain,
    auto_select_targets_for_ability, begin_target_selection_for_ability, build_chained_resolved,
    build_target_slots_labelled, cap_distribution_target_slots, flatten_targets_in_chain,
    random_select_targets_for_ability, record_modal_mode_choices, target_constraints_from_modal,
    validate_modal_indices,
};
use super::engine::EngineError;
use super::engine_stack;
use super::restrictions;
use super::triggers;
use super::{casting, casting_costs, priority};

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
    // per-mode labels and X-dependent legality stay in sync. CR 601.2d: a chosen
    // mode that divides an X-dependent pool is likewise X-bounded (issue #2856).
    let mode_distribute = indices
        .iter()
        .find_map(|&i| mode_abilities.get(i).and_then(|m| m.distribute.clone()));
    if ability_target_legality_needs_chosen_x(&resolved, mode_distribute.as_ref()) {
        if let Some(cost) = ability_cost.as_ref() {
            if let Some((mana_cost, remaining)) = casting_costs::extract_x_mana_cost(cost) {
                let mut pending_x = PendingCast::new(source_id, CardId(0), resolved, mana_cost);
                pending_x.activation_cost = remaining;
                pending_x.activation_ability_index = ability_index;
                pending_x.target_constraints = target_constraints;
                pending_x.distribute = mode_distribute.clone();
                pending_x.deferred_target_selection = true;
                let mut chosen_modes = indices.clone();
                chosen_modes.sort_unstable();
                pending_x.chosen_modes = chosen_modes;
                state.pending_cast = Some(Box::new(pending_x));
                return casting_costs::enter_payment_step(state, player, None, events);
            }
        }
    }

    if let Some(cost) = ability_cost.as_ref() {
        if casting_costs::activation_cost_needs_x_choice(&resolved, cost) {
            // CR 602.2b + CR 601.2f + CR 700.2: After modes are chosen, a
            // symbolic Remove X counters activation cost uses the same pending
            // X announcement path as non-modal activated abilities, then resumes
            // through deferred target selection with the chosen modes preserved.
            let (mana_cost, remaining) = casting::split_alt_cost_components(cost);
            let mut pending_x = PendingCast::new(
                source_id,
                CardId(0),
                resolved,
                mana_cost.unwrap_or(ManaCost::NoCost),
            );
            pending_x.activation_cost = remaining;
            pending_x.activation_ability_index = ability_index;
            pending_x.target_constraints = target_constraints;
            pending_x.distribute = mode_distribute.clone();
            pending_x.deferred_target_selection = true;
            let mut chosen_modes = indices.clone();
            chosen_modes.sort_unstable();
            pending_x.chosen_modes = chosen_modes;
            state.pending_cast = Some(Box::new(pending_x));
            return casting_costs::enter_payment_step(state, player, None, events);
        }

        // CR 118.3 + CR 602.2b: Modal activated abilities detour to the
        // interactive sacrifice prompt before targets or direct cost payment.
        // Non-modal activations take this path in `handle_activate_ability`;
        // without it, `pay_ability_cost` no-ops non-self `Sacrifice` sub-costs.
        if let Some((count, sac_filter)) = casting::find_non_self_sacrifice_cost(cost) {
            let eligible =
                casting::find_eligible_sacrifice_targets(state, player, source_id, sac_filter);
            let (min_count, max_count) = casting::sacrifice_cost_bounds(count, eligible.len());
            if eligible.len() < min_count {
                return Err(EngineError::ActionNotAllowed(
                    "Not enough eligible permanents to sacrifice".into(),
                ));
            }
            let mut pending_sac =
                PendingCast::new(source_id, CardId(0), resolved, ManaCost::NoCost);
            pending_sac.activation_cost = Some(cost.clone());
            pending_sac.activation_ability_index = ability_index;
            pending_sac.target_constraints = target_constraints_from_modal(&modal);
            pending_sac.distribute = mode_distribute.clone();
            pending_sac.deferred_target_selection = true;
            let mut chosen_modes = indices.clone();
            chosen_modes.sort_unstable();
            pending_sac.chosen_modes = chosen_modes;
            return Ok(WaitingFor::PayCost {
                player,
                kind: PayCostKind::Sacrifice,
                choices: eligible,
                count: max_count,
                min_count,
                resume: CostResume::Spell {
                    spell: Box::new(pending_sac),
                },
            });
        }
    }

    super::layers::flush_layers(state);

    // CR 700.2 / CR 601.2b: Build slots and per-mode labels together against the
    // SAME post-flush state (Finding 4 — never let the two vectors diverge in
    // length). `resolved.context` is the chained ability's context, reapplied
    // per-mode by the labelled builder.
    let (mut target_slots, mode_labels) = build_target_slots_labelled(
        state,
        &mode_abilities,
        &indices,
        &modal.mode_descriptions,
        source_id,
        player,
        &resolved.context,
        resolved.chosen_x,
    )?;
    cap_distribution_target_slots(
        state,
        &resolved,
        mode_distribute.as_ref(),
        &mut target_slots,
    );

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
        // CR 606.2: `ability_index` is `Option<usize>` here; classify via the
        // source ability cost when an index is present, else `Normal`. Using the
        // index guard avoids the partial-move of `ability_cost` consumed above.
        kind: ability_index.map_or(
            crate::types::events::ActivatedAbilityKind::Normal,
            |index| super::planeswalker::activated_ability_kind(state, source_id, index),
        ),
    });
    // CR 702.142b: Emit additional event when a boast ability is activated.
    if let Some(index) = ability_index {
        super::casting_targets::emit_keyword_ability_event_if_tagged(
            state, source_id, index, player, events,
        );
    }
    priority::clear_priority_passes(state);
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

/// CR 700.2b (override) + CR 701.9b (analogous): Complete a modal *triggered*
/// ability whose `selection` is `Random` (Cult of Skaro "choose one at random")
/// without prompting `modal.chooser`. The game draws the mode index/indices via
/// `random_select_modal_indices` (seeded `state.rng`), then routes through the
/// SAME finalization path the interactive controller-choice flow uses
/// (`handle_triggered_mode_choice`) so target legality, per-mode labels, and
/// stack-entry mutation stay identical.
///
/// Preconditions (the "push first, choose second" contract — see
/// `dispatch_pending_trigger_context`): `state.pending_trigger` is set and its
/// stack entry is already pushed and tracked by `state.pending_trigger_entry`.
///
/// Returns `Ok(None)` when no mode can be chosen (CR 603.3c) so the caller drops
/// the trigger exactly as the all-modes-unavailable branch does.
pub(super) fn resolve_random_modal_trigger(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    modal: crate::types::ability::ModalChoice,
    mode_abilities: Vec<crate::types::ability::AbilityDefinition>,
    unavailable_modes: &[usize],
    events: &mut Vec<GameEvent>,
) -> Result<Option<WaitingFor>, EngineError> {
    let Some(indices) =
        super::ability_utils::random_select_modal_indices(state, &modal, unavailable_modes)
    else {
        // CR 603.3c: No legal mode — drop the trigger. The interactive branches
        // already removed the in-flight stack entry before this point, so just
        // clear the cursor here.
        if let Some(entry_id) = state.pending_trigger_entry.take() {
            if state.stack.back().map(|e| e.id) == Some(entry_id) {
                state.stack.pop_back();
                state.stack_paid_facts.remove(&entry_id);
                state.stack_trigger_event_batches.remove(&entry_id);
            }
        }
        state.pending_trigger = None;
        return Ok(None);
    };

    // CR 700.2: Track per-turn/per-game mode usage exactly as the interactive
    // path does, then build the chained resolved ability for the drawn modes.
    record_modal_mode_choices(state, source_id, &modal, &indices);
    let resolved = build_chained_resolved(&mode_abilities, indices.as_slice(), source_id, player)?;

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
    .map(Some)
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
    // CR 603.2 + CR 109.4: Re-establish the trigger event context for
    // the duration of mode-target computation. The modal was paused for mode
    // choice (`trigger_dispatch`) AFTER restoring the context to its pre-dispatch
    // value, so `state.current_trigger_event` is now unset. A chosen mode body
    // whose target filter references the triggering event — e.g. Grenzo, Havoc
    // Raiser's "Goad target creature that player controls" (`ControllerRef::
    // TriggeringPlayer`) — must resolve "that player" to the damaged player while
    // its legal targets are computed here, exactly as the dispatch-time
    // `filter_modes_by_target_legality` did. Without this, the Goad slot finds no
    // legal target and `build_target_slots_labelled` errors ("No legal targets
    // available"). Restored on every return path below.
    let trigger_event_batch = state.pending_trigger_event_batch.clone();
    let mode_context_snapshot = triggers::push_trigger_event_context(
        state,
        trigger.trigger_event.as_ref(),
        &trigger_event_batch,
        trigger.subject_match_count,
    );
    // CR 700.2 / CR 700.2b: slots + per-mode labels built together (Finding 4).
    let (target_slots, mode_labels) = match build_target_slots_labelled(
        state,
        &mode_abilities,
        &indices,
        &modal.mode_descriptions,
        source_id,
        player,
        &resolved.context,
        // CR 107.1b: Triggered abilities don't use a chosen X here.
        None,
    ) {
        Ok(pair) => pair,
        Err(err) => {
            triggers::restore_trigger_event_context(state, mode_context_snapshot);
            return Err(err);
        }
    };
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
            match random_select_targets_for_ability(state, &target_slots, &target_constraints) {
                Ok(targets) => Some(targets),
                Err(err) => {
                    triggers::restore_trigger_event_context(state, mode_context_snapshot);
                    return Err(err);
                }
            }
        } else {
            match auto_select_targets_for_ability(
                state,
                &trigger.ability,
                &target_slots,
                &target_constraints,
            ) {
                Ok(targets) => targets,
                Err(err) => {
                    triggers::restore_trigger_event_context(state, mode_context_snapshot);
                    return Err(err);
                }
            }
        };

        if let Some(targets) = resolved_targets {
            // Targets resolved; the trigger event context is no longer needed
            // here — the resulting stack entry carries `trigger_event` for the
            // resolution-time re-establishment in `stack::resolve_top`.
            triggers::restore_trigger_event_context(state, mode_context_snapshot);
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
            if !triggers::mutate_pending_trigger_entry(state, &trigger.ability) {
                // Unexpected dangling cursor: the entry is gone before the target
                // prompt could open. Recover per CR 608.2b / CR 800.4a (a stack
                // object that has left the stack does not resolve) — record the
                // diagnostic, abandon, return priority (re-normalized next pass;
                // CR 117.3b would give the active player).
                triggers::restore_trigger_event_context(state, mode_context_snapshot);
                triggers::abandon_ceased_pending_trigger(state, &trigger.ability);
                return Ok(WaitingFor::Priority { player });
            }
            let description = trigger.description.clone();
            state.pending_trigger = Some(trigger);
            let pending_trigger = state
                .pending_trigger
                .as_ref()
                .expect("pending trigger stored before target selection");
            let selection = match begin_target_selection_for_ability(
                state,
                &pending_trigger.ability,
                &target_slots,
                &target_constraints,
            ) {
                Ok(selection) => selection,
                Err(err) => {
                    triggers::restore_trigger_event_context(state, mode_context_snapshot);
                    return Err(err);
                }
            };
            // CR 601.2c + CR 603.3d + CR 109.5: a targeted "of their choice" trigger
            // routes target selection to the scoped (upkeep) player, not the source's
            // controller. Magus is non-modal so this is defensive class-consistency
            // with the non-modal path in `begin_pending_trigger_target_selection`.
            // Snapshot all `pending_trigger` reads into locals here so the trigger
            // event context can be restored (needs `&mut state`) before returning.
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
            let trigger_controller = pending_trigger.controller;
            let trigger_event = pending_trigger.trigger_event.clone();
            // Slot legality computed; the pending `TriggerTargetSelection` carries
            // `trigger_event` so the per-slot prompt re-establishes the context.
            triggers::restore_trigger_event_context(state, mode_context_snapshot);
            return Ok(WaitingFor::TriggerTargetSelection {
                player,
                trigger_controller: Some(trigger_controller),
                trigger_event,
                trigger_events: state.pending_trigger_event_batch.clone(),
                target_slots,
                mode_labels,
                target_constraints,
                selection,
                source_id: Some(source_id),
                description,
            });
        }
    } else {
        // No target slots for the chosen mode; the trigger event context is no
        // longer needed during construction (the resolver re-establishes it).
        triggers::restore_trigger_event_context(state, mode_context_snapshot);
        // CR 603.3c: Mode chosen and no further input needed. Entry is already
        // on the stack (pushed at modal pause-time); mutate its ability with
        // the resolved mode and clear `pending_trigger_entry` so the resolver
        // may fire this entry.
        if !triggers::finalize_pending_trigger_entry(state, &trigger.ability) {
            // Unexpected dangling cursor: the entry is no longer on the stack.
            // Recover per CR 608.2b / CR 800.4a (a stack object that has left the
            // stack does not resolve) — record the diagnostic, abandon, and hand
            // back priority instead of panicking (re-normalized next pass; CR
            // 117.3b would give the active player).
            triggers::abandon_ceased_pending_trigger(state, &trigger.ability);
            priority::clear_priority_passes(state);
            return Ok(WaitingFor::Priority { player });
        }
        priority::clear_priority_passes(state);
        // CR 113.2c + CR 603.2 + CR 603.3b: Drain siblings deferred behind this
        // modal trigger so each independent instance reaches the stack
        // (issue #416).
        debug_assert!(
            !triggers::is_pending_trigger_construction_active(state),
            "deferred-trigger drain entered with construction still active",
        );
        if let Some(waiting_for) =
            triggers::drain_deferred_triggers_after_trigger_construction(state, events)
        {
            return Ok(waiting_for);
        }
    }

    Ok(WaitingFor::Priority { player })
}
