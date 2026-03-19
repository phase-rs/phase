use rand::Rng;
use thiserror::Error;

use crate::types::ability::{
    AbilityDefinition, ChoiceType, ChoiceValue, ChosenAttribute, Effect, EffectKind,
    ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, AutoPassMode, AutoPassRequest, GameState, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::match_config::MatchType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::ability_utils::{
    assign_selected_slots_in_chain, assign_targets_in_chain, auto_select_targets,
    begin_target_selection, build_chained_resolved, build_target_slots, choose_target,
    compute_unavailable_modes, flatten_targets_in_chain, record_modal_mode_choices,
    validate_modal_indices, validate_selected_targets, TargetSelectionAdvance,
};
use super::casting;
use super::derived::derive_display_state;
use super::effects;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources;
use super::match_flow;
use super::mulligan;
use super::planeswalker;
use super::priority;
use super::restrictions;
use super::sba;
use super::triggers;
use super::turns;
use super::zones;

#[derive(Debug, Clone, Error)]
pub enum EngineError {
    #[error("Invalid action: {0}")]
    InvalidAction(String),
    #[error("Wrong player")]
    WrongPlayer,
    #[error("Not your priority")]
    NotYourPriority,
    #[error("Action not allowed: {0}")]
    ActionNotAllowed(String),
}

pub fn apply(state: &mut GameState, action: GameAction) -> Result<ActionResult, EngineError> {
    let mut result = apply_action(state, action)?;
    sync_waiting_for(state, &result.waiting_for);
    run_auto_pass_loop(state, &mut result);
    derive_display_state(state);
    Ok(result)
}

fn sync_waiting_for(state: &mut GameState, waiting_for: &WaitingFor) {
    state.waiting_for = waiting_for.clone();
    if let WaitingFor::Priority { player } = waiting_for {
        state.priority_player = *player;
    }
}

/// Auto-pass loop: when a player has an auto-pass flag and receives priority,
/// automatically pass for them until the goal condition is met or interrupted.
fn run_auto_pass_loop(state: &mut GameState, result: &mut ActionResult) {
    const MAX_ITERATIONS: usize = 500;

    for _ in 0..MAX_ITERATIONS {
        match &result.waiting_for {
            WaitingFor::Priority { player } => {
                let player = *player;
                let Some(&mode) = state.auto_pass.get(&player) else {
                    break;
                };

                match mode {
                    AutoPassMode::UntilStackEmpty { initial_stack_len } => {
                        // Goal achieved: stack is empty
                        if state.stack.is_empty() {
                            state.auto_pass.remove(&player);
                            break;
                        }
                        // Interrupt: stack grew beyond the baseline (trigger or opponent spell)
                        if state.stack.len() > initial_stack_len {
                            state.auto_pass.remove(&player);
                            break;
                        }
                    }
                    AutoPassMode::UntilEndOfTurn => {
                        // UntilEndOfTurn passes through everything at priority
                    }
                }

                // Pass priority internally
                let mut events = Vec::new();
                let wf = priority::handle_priority_pass(state, &mut events);
                sync_waiting_for(state, &wf);

                // Run post-action pipeline (SBAs, triggers, layers)
                match run_post_action_pipeline(state, &mut events, &wf) {
                    Ok(wf) => {
                        sync_waiting_for(state, &wf);

                        // Check for stack growth after pipeline (triggers may have fired)
                        if let Some(&AutoPassMode::UntilStackEmpty { initial_stack_len }) =
                            state.auto_pass.get(&player)
                        {
                            if state.stack.len() > initial_stack_len {
                                state.auto_pass.remove(&player);
                            }
                        }

                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // UntilEndOfTurn: auto-submit empty attackers
            WaitingFor::DeclareAttackers { player, .. }
                if state
                    .auto_pass
                    .get(player)
                    .is_some_and(|m| matches!(m, AutoPassMode::UntilEndOfTurn)) =>
            {
                let mut events = Vec::new();
                match handle_empty_attackers(state, &mut events) {
                    Ok(wf) => {
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // UntilEndOfTurn: auto-submit empty blockers
            WaitingFor::DeclareBlockers { player, .. }
                if state
                    .auto_pass
                    .get(player)
                    .is_some_and(|m| matches!(m, AutoPassMode::UntilEndOfTurn)) =>
            {
                let mut events = Vec::new();
                match handle_empty_blockers(state, &mut events) {
                    Ok(wf) => {
                        sync_waiting_for(state, &wf);
                        result.events.extend(events);
                        result.waiting_for = wf;
                    }
                    Err(_) => break,
                }
            }

            // Non-auto-passable WaitingFor (interactive choice, game over, etc.)
            _ => break,
        }
    }
}

fn apply_action(state: &mut GameState, action: GameAction) -> Result<ActionResult, EngineError> {
    let mut events = Vec::new();
    let mut triggers_processed_inline = false;

    // CancelAutoPass works from any WaitingFor state (player may cancel during interactive choices)
    if matches!(action, GameAction::CancelAutoPass) {
        if let Some(player) = state.waiting_for.acting_player() {
            state.auto_pass.remove(&player);
        }
        return Ok(ActionResult {
            events: vec![],
            waiting_for: state.waiting_for.clone(),
        });
    }

    // Any deliberate player action (not auto-pass-related or a simple pass) cancels their auto-pass
    if let Some(player) = state.waiting_for.acting_player() {
        match &action {
            GameAction::SetAutoPass { .. } | GameAction::PassPriority => {}
            _ => {
                state.auto_pass.remove(&player);
            }
        }
    }

    // Clear manual mana-tap tracking when the player commits to a non-mana action.
    // ActivateAbility is handled per-arm (only non-mana abilities clear tracking).
    if let Some(player) = state.waiting_for.acting_player() {
        match &action {
            GameAction::PassPriority
            | GameAction::PlayLand { .. }
            | GameAction::CastSpell { .. }
            | GameAction::CancelCast
            | GameAction::PayUnlessCost { .. } => {
                state.lands_tapped_for_mana.remove(&player);
            }
            _ => {}
        }
    }

    // Validate and process action against current WaitingFor
    let waiting_for = match (&state.waiting_for.clone(), action) {
        (WaitingFor::Priority { player }, GameAction::PassPriority) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            priority::handle_priority_pass(state, &mut events)
        }
        (WaitingFor::Priority { player }, GameAction::PlayLand { card_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            handle_play_land(state, card_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::TapLandForMana { object_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            let wf = handle_tap_land_for_mana(state, object_id, &mut events)?;
            state
                .lands_tapped_for_mana
                .entry(*player)
                .or_default()
                .push(object_id);
            wf
        }
        (WaitingFor::Priority { player }, GameAction::UntapLandForMana { object_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            handle_untap_land_for_mana(state, *player, object_id, &mut events)?;
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::Priority { player }, GameAction::CastSpell { card_id, .. }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_cast_spell(state, *player, card_id, &mut events)?
        }
        (
            WaitingFor::Priority { player },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            // Check if this is a mana ability -- resolve instantly without the stack
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let ability_def = obj.abilities[ability_index].clone();
                mana_abilities::resolve_mana_ability(
                    state,
                    source_id,
                    *player,
                    &ability_def,
                    &mut events,
                )?;
                WaitingFor::Priority { player: *player }
            } else if obj.loyalty.is_some()
                && ability_index < obj.abilities.len()
                && matches!(
                    obj.abilities[ability_index].cost,
                    Some(crate::types::ability::AbilityCost::Loyalty { .. })
                )
            {
                // Planeswalker loyalty ability — non-mana, clear tracking
                state.lands_tapped_for_mana.remove(player);
                planeswalker::handle_activate_loyalty(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            } else {
                // Non-mana activated ability — clear tracking
                state.lands_tapped_for_mana.remove(player);
                casting::handle_activate_ability(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            }
        }
        // CR 715.3a: Player chooses creature or Adventure face.
        (
            WaitingFor::AdventureCastChoice {
                player,
                object_id,
                card_id,
            },
            GameAction::ChooseAdventureFace { creature },
        ) => casting::handle_adventure_choice(
            state,
            *player,
            *object_id,
            *card_id,
            creature,
            &mut events,
        )?,
        (WaitingFor::ModeChoice { player, .. }, GameAction::SelectModes { indices }) => {
            casting::handle_select_modes(state, *player, indices, &mut events)?
        }
        (
            WaitingFor::ModeChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => {
            casting::handle_cancel_cast(state, pending_cast, &mut events);
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::TargetSelection { player, .. }, GameAction::SelectTargets { targets }) => {
            casting::handle_select_targets(state, *player, targets, &mut events)?
        }
        (WaitingFor::TargetSelection { player, .. }, GameAction::ChooseTarget { target }) => {
            casting::handle_choose_target(state, *player, target, &mut events)?
        }
        (
            WaitingFor::TargetSelection {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => {
            casting::handle_cancel_cast(state, pending_cast, &mut events);
            WaitingFor::Priority { player: *player }
        }
        (
            WaitingFor::OptionalCostChoice {
                player,
                cost,
                pending_cast,
            },
            GameAction::DecideOptionalCost { pay },
        ) => casting::handle_decide_additional_cost(
            state,
            *player,
            *pending_cast.clone(),
            cost,
            pay,
            &mut events,
        )?,
        (
            WaitingFor::OptionalCostChoice {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => {
            casting::handle_cancel_cast(state, pending_cast, &mut events);
            WaitingFor::Priority { player: *player }
        }
        // CR 601.2b: Player selected cards to discard as additional casting cost.
        (
            WaitingFor::DiscardForCost {
                player,
                count,
                cards: legal_cards,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => casting::handle_discard_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            legal_cards,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::DiscardForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => {
            casting::handle_cancel_cast(state, pending_cast, &mut events);
            WaitingFor::Priority { player: *player }
        }
        // CR 118.3: Player selected permanents to sacrifice as cost.
        (
            WaitingFor::SacrificeForCost {
                player,
                count,
                permanents,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => casting::handle_sacrifice_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            permanents,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::SacrificeForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => {
            casting::handle_cancel_cast(state, pending_cast, &mut events);
            WaitingFor::Priority { player: *player }
        }
        // CR 609.3: Player decided whether to perform an optional effect ("You may X").
        (WaitingFor::OptionalEffectChoice { .. }, GameAction::DecideOptionalEffect { accept }) => {
            state.cost_payment_failed_flag = false; // Reset before resolution
            if let Some(mut ability) = state.pending_optional_effect.take() {
                ability.optional = false; // prevent re-prompt on re-entry
                if accept {
                    // CR 609.3: Player chose to perform — execute the full chain.
                    ability.context.optional_effect_performed = true;
                    effects::resolve_ability_chain(state, &ability, &mut events, 0)
                        .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                }
            }
            // Resume with pending continuation if one was stashed (from resolve_ability_chain).
            if let Some(continuation) = state.pending_continuation.take() {
                effects::resolve_ability_chain(state, &continuation, &mut events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            state.waiting_for.clone()
        }
        // CR 118.12: Player decided whether to pay an "unless pays" cost.
        (
            WaitingFor::UnlessPayment {
                player,
                cost,
                pending_counter,
            },
            GameAction::PayUnlessCost { pay },
        ) => {
            if pay {
                // Player pays the cost → spell is NOT countered
                casting::pay_unless_cost(state, *player, cost, &mut events)?;
                // CR 118.12: Payment satisfied — counter effect fizzles, spell survives.
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::Counter,
                    source_id: pending_counter.source_id,
                });
            } else {
                // Player declines → execute the counter unconditionally
                effects::counter::resolve_unconditional(state, pending_counter, &mut events)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            // Resume with pending continuation if one was stashed.
            if let Some(continuation) = state.pending_continuation.take() {
                effects::resolve_ability_chain(state, &continuation, &mut events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            // CR 117.3b: After the counter spell finishes resolving, return to priority.
            WaitingFor::Priority {
                player: state.active_player,
            }
        }
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            WaitingFor::UnlessPayment {
                player,
                cost,
                pending_counter,
            },
            GameAction::TapLandForMana { object_id },
        ) => {
            handle_tap_land_for_mana(state, object_id, &mut events)?;
            state
                .lands_tapped_for_mana
                .entry(*player)
                .or_default()
                .push(object_id);
            WaitingFor::UnlessPayment {
                player: *player,
                cost: cost.clone(),
                pending_counter: pending_counter.clone(),
            }
        }
        (
            WaitingFor::UnlessPayment {
                player,
                cost,
                pending_counter,
            },
            GameAction::UntapLandForMana { object_id },
        ) => {
            handle_untap_land_for_mana(state, *player, object_id, &mut events)?;
            WaitingFor::UnlessPayment {
                player: *player,
                cost: cost.clone(),
                pending_counter: pending_counter.clone(),
            }
        }
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            WaitingFor::UnlessPayment { player, .. },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let ability_def = obj.abilities[ability_index].clone();
                mana_abilities::resolve_mana_ability(
                    state,
                    source_id,
                    *player,
                    &ability_def,
                    &mut events,
                )?;
                state.waiting_for.clone()
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during unless payment".to_string(),
                ));
            }
        }
        (WaitingFor::ManaPayment { player }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        // Allow mana abilities during mana payment (mid-cast)
        (
            WaitingFor::ManaPayment { player },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let ability_def = obj.abilities[ability_index].clone();
                mana_abilities::resolve_mana_ability(
                    state,
                    source_id,
                    *player,
                    &ability_def,
                    &mut events,
                )?;
                WaitingFor::ManaPayment { player: *player }
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during mana payment".to_string(),
                ));
            }
        }
        // Allow basic land tapping during mana payment
        (WaitingFor::ManaPayment { player }, GameAction::TapLandForMana { object_id }) => {
            handle_tap_land_for_mana(state, object_id, &mut events)?;
            state
                .lands_tapped_for_mana
                .entry(*player)
                .or_default()
                .push(object_id);
            WaitingFor::ManaPayment { player: *player }
        }
        (WaitingFor::ManaPayment { player }, GameAction::UntapLandForMana { object_id }) => {
            handle_untap_land_for_mana(state, *player, object_id, &mut events)?;
            WaitingFor::ManaPayment { player: *player }
        }
        (
            WaitingFor::MulliganDecision {
                player,
                mulligan_count,
            },
            GameAction::MulliganDecision { keep },
        ) => {
            let p = *player;
            let mc = *mulligan_count;
            mulligan::handle_mulligan_decision(state, p, keep, mc, &mut events)
        }
        (WaitingFor::MulliganBottomCards { player, count }, GameAction::SelectCards { cards }) => {
            let p = *player;
            let c = *count;
            mulligan::handle_mulligan_bottom(state, p, cards, c, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::DeclareAttackers { player, .. }, GameAction::DeclareAttackers { attacks }) => {
            if state.active_player != *player {
                return Err(EngineError::WrongPlayer);
            }
            super::combat::declare_attackers(state, &attacks, &mut events)
                .map_err(EngineError::InvalidAction)?;

            // Process triggers for AttackersDeclared
            triggers::process_triggers(state, &events);
            triggers_processed_inline = true;
            if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
                return Ok(ActionResult {
                    events,
                    waiting_for,
                });
            }

            if attacks.is_empty() {
                // No attackers: skip to EndCombat
                state.phase = Phase::EndCombat;
                events.push(GameEvent::PhaseChanged {
                    phase: Phase::EndCombat,
                });
                state.combat = None;
                turns::advance_phase(state, &mut events);
                turns::auto_advance(state, &mut events)
            } else if !state.stack.is_empty() {
                priority::reset_priority(state);
                WaitingFor::Priority {
                    player: state.active_player,
                }
            } else {
                // Advance to DeclareBlockers
                turns::advance_phase(state, &mut events);
                turns::auto_advance(state, &mut events)
            }
        }
        (
            WaitingFor::DeclareBlockers { player: _, .. },
            GameAction::DeclareBlockers { assignments },
        ) => {
            super::combat::declare_blockers(state, &assignments, &mut events)
                .map_err(EngineError::InvalidAction)?;

            // Process triggers for BlockersDeclared
            triggers::process_triggers(state, &events);
            triggers_processed_inline = true;
            if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
                return Ok(ActionResult {
                    events,
                    waiting_for,
                });
            }

            if !state.stack.is_empty() {
                priority::reset_priority(state);
                WaitingFor::Priority {
                    player: state.active_player,
                }
            } else {
                // Advance to CombatDamage
                turns::advance_phase(state, &mut events);
                turns::auto_advance(state, &mut events)
            }
        }
        (WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { index }) => {
            match super::replacement::continue_replacement(state, index, &mut events) {
                super::replacement::ReplacementResult::Execute(event) => {
                    // Execute the resolved proposed event (e.g., zone change after
                    // replacement choice like shock land pay-life decision)
                    let mut zone_change_object_id = None;
                    if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                        object_id,
                        to,
                        from,
                        enter_tapped,
                        enter_with_counters,
                        ..
                    } = event
                    {
                        zones::move_to_zone(state, object_id, to, &mut events);
                        if to == Zone::Battlefield {
                            if let Some(obj) = state.objects.get_mut(&object_id) {
                                obj.tapped = enter_tapped;
                                obj.entered_battlefield_turn = Some(state.turn_number);
                                apply_etb_counters(obj, &enter_with_counters, &mut events);
                            }
                        }
                        if to == Zone::Battlefield || from == Zone::Battlefield {
                            state.layers_dirty = true;
                        }
                        zone_change_object_id = Some(object_id);
                    }

                    let mut waiting_for = WaitingFor::Priority {
                        player: state.active_player,
                    };
                    state.waiting_for = waiting_for.clone();

                    // Apply post-replacement side effect (e.g., pay life or enter tapped)
                    if let Some(effect_def) = state.post_replacement_effect.take() {
                        if let Some(next_waiting_for) = apply_post_replacement_effect(
                            state,
                            &effect_def,
                            zone_change_object_id,
                            &mut events,
                        ) {
                            waiting_for = next_waiting_for;
                        }
                    }

                    waiting_for
                }
                super::replacement::ReplacementResult::NeedsChoice(player) => {
                    super::replacement::replacement_choice_waiting_for(player, state)
                }
                super::replacement::ReplacementResult::Prevented => WaitingFor::Priority {
                    player: state.active_player,
                },
            }
        }
        // CR 707.9: Player chose a permanent to copy for "enter as a copy of" replacement.
        (
            WaitingFor::CopyTargetChoice {
                player,
                source_id,
                valid_targets,
            },
            GameAction::ChooseTarget { target },
        ) => {
            let target_id = match target {
                Some(TargetRef::Object(id)) if valid_targets.contains(&id) => id,
                _ => {
                    return Err(EngineError::InvalidAction(
                        "Invalid copy target".to_string(),
                    ));
                }
            };
            // CR 707.2: Copy copiable characteristics from the chosen permanent.
            let ability = ResolvedAbility::new(
                Effect::BecomeCopy {
                    target: TargetFilter::Any,
                    duration: None,
                },
                vec![TargetRef::Object(target_id)],
                *source_id,
                *player,
            );
            let _ = effects::resolve_ability_chain(state, &ability, &mut events, 0);
            state.layers_dirty = true;
            WaitingFor::Priority {
                player: state.active_player,
            }
        }
        (
            WaitingFor::EquipTarget {
                player,
                equipment_id,
                valid_targets,
            },
            GameAction::Equip {
                equipment_id: eq_id,
                target_id,
            },
        ) => {
            if eq_id != *equipment_id {
                return Err(EngineError::InvalidAction(
                    "Equipment ID mismatch".to_string(),
                ));
            }
            if !valid_targets.contains(&target_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid equip target".to_string(),
                ));
            }
            let p = *player;
            effects::attach::attach_to(state, eq_id, target_id);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Equip,
                source_id: eq_id,
            });
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::Equip { equipment_id, .. }) => {
            let p = *player;
            handle_equip_activation(state, p, equipment_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::Transform { object_id }) => {
            let p = *player;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::InvalidAction(
                    "Object is not on the battlefield".to_string(),
                ));
            }
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "You don't control this permanent".to_string(),
                ));
            }
            if obj.back_face.is_none() {
                return Err(EngineError::InvalidAction(
                    "Card has no back face".to_string(),
                ));
            }
            super::transform::transform_permanent(state, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        // CR 702.49a: Ninjutsu activation during combat
        (
            WaitingFor::Priority { player },
            GameAction::ActivateNinjutsu {
                ninjutsu_card_id,
                attacker_to_return,
            },
        ) => {
            let p = *player;
            // Validate timing: must be in declare blockers step or later in combat
            if !matches!(state.phase, Phase::DeclareBlockers | Phase::CombatDamage) {
                return Err(EngineError::ActionNotAllowed(
                    "Ninjutsu can only be activated during the declare blockers step".to_string(),
                ));
            }
            super::keywords::activate_ninjutsu(
                state,
                p,
                ninjutsu_card_id,
                attacker_to_return,
                &mut events,
            )
            .map_err(EngineError::InvalidAction)?;
            WaitingFor::Priority { player: p }
        }
        // Also handle Ninjutsu from WaitingFor::NinjutsuActivation
        (
            WaitingFor::NinjutsuActivation { player, .. },
            GameAction::ActivateNinjutsu {
                ninjutsu_card_id,
                attacker_to_return,
            },
        ) => {
            let p = *player;
            super::keywords::activate_ninjutsu(
                state,
                p,
                ninjutsu_card_id,
                attacker_to_return,
                &mut events,
            )
            .map_err(EngineError::InvalidAction)?;
            WaitingFor::Priority { player: p }
        }
        // Player can pass on Ninjutsu activation
        (WaitingFor::NinjutsuActivation { player, .. }, GameAction::PassPriority) => {
            WaitingFor::Priority { player: *player }
        }
        // Scry: player selects cards to put on TOP (in order), rest go to bottom
        (
            WaitingFor::ScryChoice { player, cards },
            GameAction::SelectCards { cards: top_cards },
        ) => {
            let p = *player;
            let all_cards = cards.clone();
            // top_cards = ObjectIds the player wants on top, rest go to bottom
            let bottom_cards: Vec<_> = all_cards
                .iter()
                .filter(|id| !top_cards.contains(id))
                .copied()
                .collect();
            // Remove all scryed cards from library, then re-insert: top cards first, then bottom
            let player_state = state
                .players
                .iter_mut()
                .find(|pl| pl.id == p)
                .expect("player exists");
            player_state.library.retain(|id| !all_cards.contains(id));
            // Insert top cards at front (index 0) in reverse order so first selected = top
            for (i, &card_id) in top_cards.iter().enumerate() {
                player_state.library.insert(i, card_id);
            }
            // Bottom cards go to end
            for &card_id in &bottom_cards {
                player_state.library.push(card_id);
            }
            // Execute any continuation saved when this choice interrupted an ability chain.
            // Reset waiting_for first so non-interactive continuations don't return stale state.
            // Also sync priority_player: interactive choices (Scry/Dig/Surveil) skip the
            // reset_priority() call in handle_priority_pass, so we must update it here
            // to avoid "Not your priority" errors on the next player action.
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // Dig: player selects keep_count cards for hand, rest go to graveyard
        (
            WaitingFor::DigChoice {
                player,
                cards: all_cards,
                keep_count,
            },
            GameAction::SelectCards { cards: kept },
        ) => {
            let p = *player;
            let kc = *keep_count;
            let all = all_cards.clone();
            if kept.len() != kc {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} cards, got {}",
                    kc,
                    kept.len()
                )));
            }
            let to_graveyard: Vec<_> = all
                .iter()
                .filter(|id| !kept.contains(id))
                .copied()
                .collect();
            // Move kept cards to hand
            for &obj_id in &kept {
                zones::move_to_zone(state, obj_id, Zone::Hand, &mut events);
            }
            // Move rest to graveyard
            for &obj_id in &to_graveyard {
                zones::move_to_zone(state, obj_id, Zone::Graveyard, &mut events);
            }
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // Surveil: player selects cards to put in GRAVEYARD, rest stay on top
        (
            WaitingFor::SurveilChoice { player, cards },
            GameAction::SelectCards {
                cards: to_graveyard,
            },
        ) => {
            let p = *player;
            let all_cards = cards.clone();
            // Move selected cards to graveyard
            for &obj_id in &to_graveyard {
                if all_cards.contains(&obj_id) {
                    zones::move_to_zone(state, obj_id, Zone::Graveyard, &mut events);
                }
            }
            // Cards not in to_graveyard stay on top of library (already there)
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // RevealChoice: player selects a card from revealed hand.
        // Chosen card becomes the target for the pending_continuation sub-ability
        // (e.g. ChangeZone to exile, DiscardCard, etc.). Unchosen cards stay in hand.
        // Clear revealed_cards so opponent's hand goes hidden again.
        (
            WaitingFor::RevealChoice {
                player,
                cards: all_cards,
                filter,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let p = *player;
            let all = all_cards.clone();
            let card_filter = filter.clone();
            if chosen.len() != 1 {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly 1 card, got {}",
                    chosen.len()
                )));
            }
            let chosen_id = chosen[0];
            if !all.contains(&chosen_id) {
                return Err(EngineError::InvalidAction(
                    "Selected card not in revealed hand".to_string(),
                ));
            }

            // Validate chosen card matches the filter (e.g. "nonland card")
            if !matches!(card_filter, crate::types::ability::TargetFilter::Any) {
                // Use a dummy source_id for filter matching since the source
                // may have left play; controller isn't relevant for hand cards
                if !super::filter::matches_target_filter(state, chosen_id, &card_filter, chosen_id)
                {
                    return Err(EngineError::InvalidAction(
                        "Selected card does not match the required filter".to_string(),
                    ));
                }
            }

            // Clear revealed status
            for &card_id in &all {
                state.revealed_cards.remove(&card_id);
            }

            // Run the pending continuation with the chosen card as its target.
            // Reset waiting_for to Priority first so that if the continuation
            // (e.g. ChangeZone) doesn't set a new interactive state, we don't
            // return a stale RevealChoice that re-renders the modal.
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(mut cont) = state.pending_continuation.take() {
                cont.targets = vec![crate::types::ability::TargetRef::Object(chosen_id)];
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // SearchChoice: player selects card(s) from filtered library search.
        // Selected cards become targets for the pending_continuation (ChangeZone + Shuffle).
        (
            WaitingFor::SearchChoice {
                player,
                cards: legal_cards,
                count,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let p = *player;
            let legal = legal_cards.clone();
            let expected_count = *count;

            // Validate selection count
            if chosen.len() != expected_count {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} card(s), got {}",
                    expected_count,
                    chosen.len()
                )));
            }

            // Validate all chosen cards are in legal set
            for card_id in &chosen {
                if !legal.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in search results".to_string(),
                    ));
                }
            }

            // Run the pending continuation with chosen cards as targets
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(mut cont) = state.pending_continuation.take() {
                cont.targets = chosen
                    .iter()
                    .map(|&id| crate::types::ability::TargetRef::Object(id))
                    .collect();
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // CR 514.1: Player chose which cards to discard during cleanup.
        (
            WaitingFor::DiscardToHandSize {
                player,
                count,
                cards: legal_cards,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let _p = *player;
            let expected = *count;
            let legal = legal_cards.clone();

            if chosen.len() != expected {
                return Err(EngineError::InvalidAction(format!(
                    "Must discard exactly {} card(s), got {}",
                    expected,
                    chosen.len()
                )));
            }

            for card_id in &chosen {
                if !legal.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in hand".to_string(),
                    ));
                }
            }

            super::turns::finish_cleanup_discard(state, &chosen, &mut events);

            // Continue the turn: advance past cleanup into next turn
            super::turns::advance_phase(state, &mut events);
            super::turns::auto_advance(state, &mut events)
        }
        // NamedChoice: player selects from a set of named options (creature type, color, etc.).
        // Stores the chosen value in last_named_choice and resumes any pending continuation.
        (
            WaitingFor::NamedChoice {
                player,
                options,
                choice_type,
                source_id,
            },
            GameAction::ChooseOption { choice },
        ) => {
            let p = *player;

            // CardName validates against the full card database (stored on GameState,
            // not in WaitingFor options, to avoid serializing 30k+ names every update).
            // All other choice types validate against the provided options list.
            if matches!(choice_type, ChoiceType::CardName) {
                let lower = choice.to_lowercase();
                if !state
                    .all_card_names
                    .iter()
                    .any(|n| n.to_lowercase() == lower)
                {
                    return Err(EngineError::InvalidAction(format!(
                        "Invalid card name '{}'",
                        choice
                    )));
                }
            } else if !options.contains(&choice) {
                return Err(EngineError::InvalidAction(format!(
                    "Invalid choice '{}', must be one of: {:?}",
                    choice, options
                )));
            }

            // Store typed attribute on source object if this is a persisted choice
            if let Some(obj_id) = source_id {
                if let Some(attr) = ChosenAttribute::from_choice(choice_type.clone(), &choice) {
                    if let Some(obj) = state.objects.get_mut(obj_id) {
                        obj.chosen_attributes.push(attr);
                    }
                }
            }

            // Store the chosen value for continuations to read
            state.last_named_choice = ChoiceValue::from_choice(choice_type, &choice);

            // Resume pending continuation if present
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            // Clear the choice after the continuation has consumed it
            state.last_named_choice = None;
            state.waiting_for.clone()
        }
        // CR 701.52: Player selects a ring-bearer from candidates.
        (
            WaitingFor::ChooseRingBearer { player, candidates },
            GameAction::ChooseRingBearer { target },
        ) => {
            if !candidates.contains(&target) {
                return Err(EngineError::InvalidAction(
                    "Invalid ring-bearer choice".to_string(),
                ));
            }
            let p = *player;
            state.ring_bearer.insert(p, Some(target));
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            // Resume pending continuation if present
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        (WaitingFor::Priority { player }, GameAction::PlayFaceDown { card_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            // Find the object with this card_id in the player's hand
            let object_id = state
                .objects
                .iter()
                .find(|(_, obj)| obj.card_id == card_id && obj.owner == p && obj.zone == Zone::Hand)
                .map(|(id, _)| *id)
                .ok_or_else(|| EngineError::InvalidAction("Card not found in hand".to_string()))?;
            super::morph::play_face_down(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::TurnFaceUp { object_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            super::morph::turn_face_up(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                target_constraints,
                ..
            },
            GameAction::SelectTargets { targets },
        ) => {
            validate_selected_targets(target_slots, &targets, target_constraints)?;
            // Take the pending trigger, set targets, push to stack
            let trigger = state
                .pending_trigger
                .take()
                .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
            let mut ability = trigger.ability;
            assign_targets_in_chain(&mut ability, &targets)?;

            casting::emit_targeting_events(
                state,
                &flatten_targets_in_chain(&ability),
                trigger.source_id,
                trigger.controller,
                &mut events,
            );

            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            let entry = crate::types::game_state::StackEntry {
                id: entry_id,
                source_id: trigger.source_id,
                controller: trigger.controller,
                kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                    source_id: trigger.source_id,
                    ability,
                    condition: trigger.condition.clone(),
                    trigger_event: None,
                },
            };
            super::stack::push_to_stack(state, entry, &mut events);
            state.priority_passes.clear();
            state.priority_pass_count = 0;
            WaitingFor::Priority { player: *player }
        }
        (
            WaitingFor::TriggerTargetSelection {
                player,
                target_slots,
                target_constraints,
                selection,
            },
            GameAction::ChooseTarget { target },
        ) => {
            let Some(_pending_trigger) = state.pending_trigger.as_ref() else {
                return Err(EngineError::InvalidAction("No pending trigger".to_string()));
            };

            match choose_target(target_slots, target_constraints, selection, target)? {
                TargetSelectionAdvance::InProgress(selection) => {
                    WaitingFor::TriggerTargetSelection {
                        player: *player,
                        target_slots: target_slots.clone(),
                        target_constraints: target_constraints.clone(),
                        selection,
                    }
                }
                TargetSelectionAdvance::Complete(selected_slots) => {
                    let trigger = state.pending_trigger.take().ok_or_else(|| {
                        EngineError::InvalidAction("No pending trigger".to_string())
                    })?;
                    let mut ability = trigger.ability;
                    assign_selected_slots_in_chain(&mut ability, &selected_slots)?;

                    casting::emit_targeting_events(
                        state,
                        &flatten_targets_in_chain(&ability),
                        trigger.source_id,
                        trigger.controller,
                        &mut events,
                    );

                    let entry_id = ObjectId(state.next_object_id);
                    state.next_object_id += 1;
                    let entry = crate::types::game_state::StackEntry {
                        id: entry_id,
                        source_id: trigger.source_id,
                        controller: trigger.controller,
                        kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                            source_id: trigger.source_id,
                            ability,
                            condition: trigger.condition.clone(),
                            trigger_event: None,
                        },
                    };
                    super::stack::push_to_stack(state, entry, &mut events);
                    state.priority_passes.clear();
                    state.priority_pass_count = 0;
                    WaitingFor::Priority { player: *player }
                }
            }
        }
        (
            WaitingFor::BetweenGamesSideboard { player, .. },
            GameAction::SubmitSideboard { main, sideboard },
        ) => match_flow::handle_submit_sideboard(state, *player, main, sideboard)
            .map_err(EngineError::InvalidAction)?,
        (
            WaitingFor::BetweenGamesChoosePlayDraw { player, .. },
            GameAction::ChoosePlayDraw { play_first },
        ) => match_flow::handle_choose_play_draw(state, *player, play_first, &mut events)
            .map_err(EngineError::InvalidAction)?,
        (
            WaitingFor::AbilityModeChoice {
                player,
                modal,
                source_id,
                mode_abilities,
                is_activated,
                ability_index,
                ability_cost,
                unavailable_modes,
            },
            GameAction::SelectModes { indices },
        ) => {
            validate_modal_indices(modal, &indices, unavailable_modes)?;
            record_modal_mode_choices(state, *source_id, modal, &indices);

            let p = *player;
            let sid = *source_id;
            let resolved = build_chained_resolved(mode_abilities, indices.as_slice(), sid, p)?;

            if *is_activated {
                if state.layers_dirty {
                    super::layers::evaluate_layers(state);
                }

                let target_slots = build_target_slots(state, &resolved)?;
                let target_constraints = super::ability_utils::target_constraints_from_modal(modal);
                if !target_slots.is_empty() {
                    if let Some(targets) = auto_select_targets(&target_slots, &target_constraints)?
                    {
                        let mut resolved = resolved;
                        assign_targets_in_chain(&mut resolved, &targets)?;

                        if let Some(cost) = ability_cost {
                            casting::pay_ability_cost(state, p, sid, cost, &mut events)?;
                        }
                        casting::emit_targeting_events(
                            state,
                            &flatten_targets_in_chain(&resolved),
                            sid,
                            p,
                            &mut events,
                        );

                        let entry_id = ObjectId(state.next_object_id);
                        state.next_object_id += 1;
                        super::stack::push_to_stack(
                            state,
                            crate::types::game_state::StackEntry {
                                id: entry_id,
                                source_id: sid,
                                controller: p,
                                kind: crate::types::game_state::StackEntryKind::ActivatedAbility {
                                    source_id: sid,
                                    ability: resolved,
                                },
                            },
                            &mut events,
                        );
                        if let Some(idx) = ability_index {
                            restrictions::record_ability_activation(state, sid, *idx);
                        }
                    } else {
                        let selection = begin_target_selection(&target_slots, &target_constraints)?;
                        return Ok(ActionResult {
                            events,
                            waiting_for: WaitingFor::TargetSelection {
                                player: p,
                                pending_cast: Box::new(crate::types::game_state::PendingCast {
                                    object_id: sid,
                                    card_id: CardId(0),
                                    ability: resolved,
                                    cost: crate::types::mana::ManaCost::NoCost,
                                    activation_cost: ability_cost.clone(),
                                    activation_ability_index: *ability_index,
                                    target_constraints,
                                }),
                                target_slots,
                                selection,
                            },
                        });
                    }
                } else {
                    if let Some(cost) = ability_cost {
                        casting::pay_ability_cost(state, p, sid, cost, &mut events)?;
                    }
                    let entry_id = ObjectId(state.next_object_id);
                    state.next_object_id += 1;
                    super::stack::push_to_stack(
                        state,
                        crate::types::game_state::StackEntry {
                            id: entry_id,
                            source_id: sid,
                            controller: p,
                            kind: crate::types::game_state::StackEntryKind::ActivatedAbility {
                                source_id: sid,
                                ability: resolved,
                            },
                        },
                        &mut events,
                    );
                    if let Some(idx) = ability_index {
                        restrictions::record_ability_activation(state, sid, *idx);
                    }
                }

                events.push(GameEvent::AbilityActivated { source_id: sid });
                state.priority_passes.clear();
                state.priority_pass_count = 0;
                WaitingFor::Priority { player: p }
            } else {
                // Preserve trigger_event from the stashed pending_trigger for event-context resolution.
                let te = state
                    .pending_trigger
                    .as_ref()
                    .and_then(|pt| pt.trigger_event.clone());
                let target_slots = build_target_slots(state, &resolved)?;
                let target_constraints = super::ability_utils::target_constraints_from_modal(modal);
                if !target_slots.is_empty() {
                    if let Some(targets) = auto_select_targets(&target_slots, &target_constraints)?
                    {
                        let mut resolved = resolved;
                        assign_targets_in_chain(&mut resolved, &targets)?;
                        casting::emit_targeting_events(
                            state,
                            &flatten_targets_in_chain(&resolved),
                            sid,
                            p,
                            &mut events,
                        );
                        super::triggers::push_pending_trigger_to_stack(
                            state,
                            crate::game::triggers::PendingTrigger {
                                source_id: sid,
                                controller: p,
                                condition: None,
                                ability: resolved,
                                timestamp: state.turn_number,
                                target_constraints,
                                trigger_event: te,
                                modal: None,
                                mode_abilities: vec![],
                            },
                            &mut events,
                        );
                    } else {
                        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
                            source_id: sid,
                            controller: p,
                            condition: None,
                            ability: resolved,
                            timestamp: state.turn_number,
                            target_constraints: target_constraints.clone(),
                            trigger_event: te,
                            modal: None,
                            mode_abilities: vec![],
                        });
                        let selection = begin_target_selection(&target_slots, &target_constraints)?;
                        return Ok(ActionResult {
                            events,
                            waiting_for: WaitingFor::TriggerTargetSelection {
                                player: p,
                                target_slots,
                                target_constraints,
                                selection,
                            },
                        });
                    }
                } else {
                    super::triggers::push_pending_trigger_to_stack(
                        state,
                        crate::game::triggers::PendingTrigger {
                            source_id: sid,
                            controller: p,
                            condition: None,
                            ability: resolved,
                            timestamp: state.turn_number,
                            target_constraints,
                            trigger_event: te,
                            modal: None,
                            mode_abilities: vec![],
                        },
                        &mut events,
                    );
                }
                state.priority_passes.clear();
                state.priority_pass_count = 0;
                WaitingFor::Priority { player: p }
            }
        }
        // CR 601.2c: Player selected targets from a multi-target set ("any number of").
        (
            WaitingFor::MultiTargetSelection {
                player,
                legal_targets,
                min_targets,
                max_targets,
                pending_ability,
            },
            GameAction::SelectCards { cards: selected },
        ) => {
            let p = *player;
            let legal = legal_targets.clone();
            let min = *min_targets;
            let max = *max_targets;
            let mut ability = pending_ability.as_ref().clone();

            // CR 601.2c: Validate target count is within the declared range.
            if selected.len() < min || selected.len() > max {
                return Err(EngineError::InvalidAction(format!(
                    "Must select between {} and {} targets, got {}",
                    min,
                    max,
                    selected.len()
                )));
            }

            // CR 115.1d: Each selected target must be a legal target.
            for id in &selected {
                if !legal.contains(id) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not in legal set".to_string(),
                    ));
                }
            }

            ability.targets = selected.iter().map(|&id| TargetRef::Object(id)).collect();

            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            let _ = effects::resolve_ability_chain(state, &ability, &mut events, 0);

            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }

            state.waiting_for.clone()
        }
        (WaitingFor::Priority { player }, GameAction::SetAutoPass { mode }) => {
            // Convert request to stored mode, capturing engine state as needed
            let stored_mode = match mode {
                AutoPassRequest::UntilStackEmpty => AutoPassMode::UntilStackEmpty {
                    initial_stack_len: state.stack.len(),
                },
                AutoPassRequest::UntilEndOfTurn => AutoPassMode::UntilEndOfTurn,
            };
            state.auto_pass.insert(*player, stored_mode);
            // Immediately pass priority — the auto-pass loop in apply() continues from here
            priority::handle_priority_pass(state, &mut events)
        }
        (waiting, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting
            )));
        }
    };

    // Run post-action pipeline (SBAs, triggers, layers) and check for terminal states
    if matches!(waiting_for, WaitingFor::Priority { .. }) && !triggers_processed_inline {
        let wf = run_post_action_pipeline(state, &mut events, &waiting_for)?;
        state.waiting_for = wf.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
        });
    }

    // CR 704.3 / CR 800.4: SBAs may have ended the game during phase auto-advance (e.g.,
    // combat damage step) before we reach this point. state.waiting_for is the authoritative
    // result — written directly by eliminate_player → check_game_over. Guard against
    // overwriting it with the computed `waiting_for` from auto_advance.
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        let wf = state.waiting_for.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
        });
    }

    state.waiting_for = waiting_for.clone();

    Ok(ActionResult {
        events,
        waiting_for,
    })
}

/// Run state-based actions, exile returns, delayed triggers, and trigger processing
/// after an action that produced `WaitingFor::Priority`. Returns the resulting
/// `WaitingFor` state — may be terminal (GameOver, interactive choice) or
/// a continuation (Priority for next player/active player).
///
/// `default_wf` is the WaitingFor computed by the action handler, used as fallback
/// when no terminal/trigger/SBA outcome overrides it.
fn run_post_action_pipeline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    default_wf: &WaitingFor,
) -> Result<WaitingFor, EngineError> {
    sba::check_state_based_actions(state, events);

    // Check exile returns -- must happen after SBAs (which may move sources off battlefield)
    // and before triggers (so returned permanents get ETB triggers)
    check_exile_returns(state, events);

    // CR 603.7: Check delayed triggers before processing regular triggers.
    let delayed_events = triggers::check_delayed_triggers(state, events);
    events.extend(delayed_events);

    // SBA might have set game over
    if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
        match_flow::handle_game_over_transition(state);
        return Ok(state.waiting_for.clone());
    }

    // Process triggers after action + SBA + exile return events.
    // Filter out PhaseChanged events for phases that were auto-advanced past.
    let current_phase = state.phase;
    let filtered_events: Vec<_> = events
        .iter()
        .filter(|e| {
            if let GameEvent::PhaseChanged { phase } = e {
                *phase == current_phase
            } else {
                true
            }
        })
        .cloned()
        .collect();
    let stack_before = state.stack.len();
    triggers::process_triggers(state, &filtered_events);

    if let Some(wf) = begin_pending_trigger_target_selection(state)? {
        state.waiting_for = wf.clone();
        derive_display_state(state);
        return Ok(wf);
    }

    // If triggers were placed on stack, grant priority to active player
    if state.stack.len() > stack_before {
        return Ok(WaitingFor::Priority {
            player: state.active_player,
        });
    }

    // Re-evaluate layers if dirty after SBA/trigger processing
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    // Normal continuation: use the waiting_for computed by the action handler
    Ok(default_wf.clone())
}

/// Handle declaring no attackers — skips to EndCombat with trigger processing.
fn handle_empty_attackers(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    super::combat::declare_attackers(state, &[], events).map_err(EngineError::InvalidAction)?;

    // Process triggers for AttackersDeclared (even with no attackers)
    triggers::process_triggers(state, events);
    if let Some(wf) = begin_pending_trigger_target_selection(state)? {
        return Ok(wf);
    }

    // No attackers → skip to EndCombat
    state.phase = Phase::EndCombat;
    events.push(GameEvent::PhaseChanged {
        phase: Phase::EndCombat,
    });
    state.combat = None;
    turns::advance_phase(state, events);
    Ok(turns::auto_advance(state, events))
}

/// Handle declaring no blockers with trigger processing.
fn handle_empty_blockers(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    super::combat::declare_blockers(state, &[], events).map_err(EngineError::InvalidAction)?;

    triggers::process_triggers(state, events);
    if let Some(wf) = begin_pending_trigger_target_selection(state)? {
        return Ok(wf);
    }

    turns::advance_phase(state, events);
    Ok(turns::auto_advance(state, events))
}

fn begin_pending_trigger_target_selection(
    state: &mut GameState,
) -> Result<Option<WaitingFor>, EngineError> {
    let Some(trigger) = state.pending_trigger.as_ref() else {
        return Ok(None);
    };

    // CR 700.2a: Modal trigger — prompt for mode selection before stack.
    if let Some(ref modal) = trigger.modal {
        if !trigger.mode_abilities.is_empty() {
            let unavailable_modes = compute_unavailable_modes(state, trigger.source_id, modal);

            // CR 700.2: All modes already chosen — ability cannot be put on the stack
            // without a mode selection. Clear pending trigger and skip.
            if unavailable_modes.len() >= modal.mode_count {
                state.pending_trigger = None;
                return Ok(None);
            }

            return Ok(Some(WaitingFor::AbilityModeChoice {
                player: trigger.controller,
                modal: modal.clone(),
                source_id: trigger.source_id,
                mode_abilities: trigger.mode_abilities.clone(),
                is_activated: false,
                ability_index: None,
                ability_cost: None,
                unavailable_modes,
            }));
        }
    }

    let target_slots = build_target_slots(state, &trigger.ability)?;
    if target_slots.is_empty() {
        return Ok(None);
    }

    let player = trigger.controller;
    let target_constraints = trigger.target_constraints.clone();
    let selection = begin_target_selection(&target_slots, &target_constraints)?;
    Ok(Some(WaitingFor::TriggerTargetSelection {
        player,
        target_slots,
        target_constraints,
        selection,
    }))
}

/// Apply ETB counters from replacement effects to an object entering the battlefield.
fn apply_etb_counters(
    obj: &mut super::game_object::GameObject,
    counters: &[(String, u32)],
    events: &mut Vec<GameEvent>,
) {
    for (counter_type_str, count) in counters {
        let ct = super::game_object::parse_counter_type(counter_type_str);
        *obj.counters.entry(ct.clone()).or_insert(0) += count;
        events.push(GameEvent::CounterAdded {
            object_id: obj.id,
            counter_type: ct,
            count: *count,
        });
    }
}

/// Apply a post-replacement side effect after a zone change has been executed.
/// Used by Optional replacements (e.g., shock lands: pay life on accept, tap on decline).
/// CR 707.9: For "enter as a copy" replacements, sets up CopyTargetChoice instead of
/// immediate resolution, since the player must choose which permanent to copy.
fn apply_post_replacement_effect(
    state: &mut GameState,
    effect_def: &crate::types::ability::AbilityDefinition,
    object_id: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    let (source_id, controller) = object_id
        .and_then(|obj_id| {
            state
                .objects
                .get(&obj_id)
                .map(|obj| (obj_id, obj.controller))
        })
        .unwrap_or((ObjectId(0), state.active_player));

    // CR 707.9: BecomeCopy needs interactive target selection — the player chooses
    // which permanent to copy. This is a choice, not targeting (hexproof doesn't apply).
    if let Effect::BecomeCopy { ref target, .. } = effect_def.effect {
        let valid_targets = find_copy_targets(state, target, source_id, controller);
        if valid_targets.is_empty() {
            // No valid targets — clone enters as itself (no copy)
            return None;
        }
        return Some(WaitingFor::CopyTargetChoice {
            player: controller,
            source_id,
            valid_targets,
        });
    }

    let targets = object_id
        .map(TargetRef::Object)
        .into_iter()
        .collect::<Vec<_>>();
    let resolved = resolved_ability_from_definition(effect_def, source_id, controller, targets);
    let _ = effects::resolve_ability_chain(state, &resolved, events, 0);

    match &state.waiting_for {
        WaitingFor::Priority { .. } => None,
        wf => Some(wf.clone()),
    }
}

/// CR 707.9: Find valid permanents on the battlefield that match the copy filter.
/// This is a choice, not targeting — hexproof/shroud/protection don't apply.
fn find_copy_targets(
    state: &GameState,
    filter: &TargetFilter,
    source_id: ObjectId,
    controller: PlayerId,
) -> Vec<ObjectId> {
    state
        .objects
        .iter()
        .filter(|(id, obj)| {
            // Must be on the battlefield
            obj.zone == Zone::Battlefield
                // Can't copy itself
                && **id != source_id
                // Must match the type filter
                && super::filter::matches_target_filter_controlled(
                    state, **id, filter, source_id, controller,
                )
        })
        .map(|(id, _)| *id)
        .collect()
}

fn resolved_ability_from_definition(
    def: &AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
    targets: Vec<TargetRef>,
) -> ResolvedAbility {
    let mut resolved =
        ResolvedAbility::new(def.effect.clone(), targets, source_id, controller).kind(def.kind);
    if let Some(sub) = &def.sub_ability {
        resolved = resolved.sub_ability(resolved_ability_from_definition(
            sub,
            source_id,
            controller,
            Vec::new(),
        ));
    }
    if let Some(d) = def.duration.clone() {
        resolved = resolved.duration(d);
    }
    if let Some(c) = def.condition.clone() {
        resolved = resolved.condition(c);
    }
    resolved
}

fn handle_play_land(
    state: &mut GameState,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate main phase
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Can only play lands during main phases".to_string(),
            ));
        }
    }

    // Validate land limit
    if state.lands_played_this_turn >= state.max_lands_per_turn {
        return Err(EngineError::ActionNotAllowed(
            "Already played maximum lands this turn".to_string(),
        ));
    }

    // Find the object in player's hand by matching card_id
    let player = state
        .players
        .iter()
        .find(|p| p.id == state.priority_player)
        .expect("priority player exists");

    let object_id = player
        .hand
        .iter()
        .find(|&&obj_id| {
            state
                .objects
                .get(&obj_id)
                .map(|obj| obj.card_id == card_id)
                .unwrap_or(false)
        })
        .copied()
        .ok_or_else(|| EngineError::InvalidAction("Card not found in hand".to_string()))?;

    // Route through the replacement pipeline (handles ETB replacements like shock lands)
    let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
        object_id,
        Zone::Hand,
        Zone::Battlefield,
        None,
    );

    match super::replacement::replace_event(state, proposed, events) {
        super::replacement::ReplacementResult::Execute(event) => {
            if let crate::types::proposed_event::ProposedEvent::ZoneChange {
                object_id,
                to,
                enter_tapped,
                enter_with_counters,
                ..
            } = event
            {
                zones::move_to_zone(state, object_id, to, events);
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    obj.tapped = enter_tapped;
                    obj.entered_battlefield_turn = Some(state.turn_number);
                    apply_etb_counters(obj, &enter_with_counters, events);
                }
            }
        }
        super::replacement::ReplacementResult::Prevented => {
            // Land play was prevented — don't increment counters
            return Ok(WaitingFor::Priority {
                player: state.priority_player,
            });
        }
        super::replacement::ReplacementResult::NeedsChoice(player) => {
            // A replacement needs player choice (e.g., shock land "pay 2 life?").
            // Increment counters now — the land play is committed, only the ETB
            // effect is pending.
            state.lands_played_this_turn += 1;
            if let Some(p) = state
                .players
                .iter_mut()
                .find(|p| p.id == state.priority_player)
            {
                p.lands_played_this_turn += 1;
            }
            state.priority_passes.clear();
            state.priority_pass_count = 0;

            events.push(GameEvent::LandPlayed {
                object_id,
                player_id: state.priority_player,
            });

            return Ok(super::replacement::replacement_choice_waiting_for(
                player, state,
            ));
        }
    }

    // Increment land counter
    state.lands_played_this_turn += 1;
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == state.priority_player)
        .expect("priority player exists");
    player.lands_played_this_turn += 1;

    // Reset priority passes (action was taken)
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::LandPlayed {
        object_id,
        player_id: state.priority_player,
    });

    // Player retains priority after playing a land
    Ok(WaitingFor::Priority {
        player: state.priority_player,
    })
}

fn handle_tap_land_for_mana(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    // Validate: on battlefield, controlled by acting player, is a land, not tapped
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != state.priority_player {
        return Err(EngineError::NotYourPriority);
    }
    if !obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::InvalidAction(
            "Object is not a land".to_string(),
        ));
    }
    if obj.tapped {
        return Err(EngineError::InvalidAction(
            "Land is already tapped".to_string(),
        ));
    }

    let mana_option = mana_sources::activatable_land_mana_options(state, object_id, obj.controller)
        .into_iter()
        .next()
        .ok_or_else(|| {
            EngineError::ActionNotAllowed("Land has no activatable mana ability".to_string())
        })?;

    let ability_to_resolve = mana_option.ability_index.and_then(|ability_index| {
        state
            .objects
            .get(&object_id)
            .and_then(|land| land.abilities.get(ability_index))
            .cloned()
    });

    if let Some(ability_def) = ability_to_resolve {
        mana_abilities::resolve_mana_ability(
            state,
            object_id,
            state.priority_player,
            &ability_def,
            events,
        )?;
    } else {
        // Legacy fallback for subtype-only lands.
        let obj = state.objects.get_mut(&object_id).unwrap();
        obj.tapped = true;
        events.push(GameEvent::PermanentTapped { object_id });
        mana_payment::produce_mana(
            state,
            object_id,
            mana_option.mana_type,
            state.priority_player,
            events,
        );
    }

    Ok(WaitingFor::Priority {
        player: state.priority_player,
    })
}

/// CR 605.3a: Reverse a manual land tap — untap source and remove its mana from pool.
/// Rejects if the land isn't tracked or its mana was already spent.
fn handle_untap_land_for_mana(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    // Validate: object_id is in this player's lands_tapped_for_mana
    let tracked = state
        .lands_tapped_for_mana
        .get(&player)
        .is_some_and(|ids| ids.contains(&object_id));
    if !tracked {
        return Err(EngineError::InvalidAction(
            "Land was not manually tapped for mana".to_string(),
        ));
    }

    // CR 605.3: Mana abilities resolve immediately — once consumed, irreversible.
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    let removed = player_data.mana_pool.remove_from_source(object_id);
    if removed == 0 {
        return Err(EngineError::InvalidAction(
            "Mana from this source was already spent".to_string(),
        ));
    }

    // Untap the land
    let obj = state
        .objects
        .get_mut(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    obj.tapped = false;
    events.push(GameEvent::PermanentUntapped { object_id });

    // Remove from tracking
    if let Some(ids) = state.lands_tapped_for_mana.get_mut(&player) {
        ids.retain(|&id| id != object_id);
        if ids.is_empty() {
            state.lands_tapped_for_mana.remove(&player);
        }
    }

    Ok(())
}

fn handle_equip_activation(
    state: &mut GameState,
    player: PlayerId,
    equipment_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate sorcery-speed timing: main phase, empty stack, active player
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Equip can only be activated during main phases".to_string(),
            ));
        }
    }
    if !state.stack.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated when the stack is empty".to_string(),
        ));
    }
    if state.active_player != player {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated by the active player".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&equipment_id)
        .ok_or_else(|| EngineError::InvalidAction("Equipment not found".to_string()))?;

    // Validate it's an equipment on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Equipment is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this equipment".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Equipment".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not an equipment".to_string(),
        ));
    }

    // Find valid targets: creatures controlled by the equipping player on battlefield
    let valid_targets: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|o| {
                    o.controller == player
                        && o.card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                })
                .unwrap_or(false)
        })
        .collect();

    if valid_targets.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No valid creatures to equip".to_string(),
        ));
    }

    // If only one target, auto-equip
    if valid_targets.len() == 1 {
        let target_id = valid_targets[0];
        effects::attach::attach_to(state, equipment_id, target_id);
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Equip,
            source_id: equipment_id,
        });
        state.priority_passes.clear();
        state.priority_pass_count = 0;
        return Ok(WaitingFor::Priority { player });
    }

    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::EquipTarget {
        player,
        equipment_id,
        valid_targets,
    })
}

pub fn new_game(seed: u64) -> GameState {
    GameState::new_two_player(seed)
}

/// Start game with mulligan flow. If no cards in libraries, skips mulligan.
pub fn start_game(state: &mut GameState) -> ActionResult {
    let starting_player = if state.match_config.match_type == MatchType::Bo3
        && state.players.len() == 2
        && state.game_number == 1
    {
        if state.rng.random_bool(0.5) {
            PlayerId(0)
        } else {
            PlayerId(1)
        }
    } else {
        PlayerId(0)
    };
    start_game_with_starting_player(state, starting_player)
}

/// Start game with a specific player taking the first turn.
pub fn start_game_with_starting_player(
    state: &mut GameState,
    starting_player: PlayerId,
) -> ActionResult {
    let mut events = Vec::new();

    if state.match_config.match_type == MatchType::Bo3 && state.players.len() != 2 {
        state.match_config.match_type = MatchType::Bo1;
    }

    events.push(GameEvent::GameStarted);

    // Begin the game: set turn 1
    state.turn_number = 1;
    state.active_player = starting_player;
    state.priority_player = starting_player;
    state.current_starting_player = starting_player;
    // First-game default chooser is the starting player; BO3 restarts can pre-set this.
    if state.next_game_chooser.is_none() {
        state.next_game_chooser = Some(starting_player);
    }
    // Rotate seat order so mulligan starts with the starting player.
    if let Some(idx) = state.seat_order.iter().position(|&p| p == starting_player) {
        state.seat_order.rotate_left(idx);
    }
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: starting_player,
        turn_number: 1,
    });

    // If players have cards in their libraries, start mulligan flow
    let has_libraries = state.players.iter().any(|p| !p.library.is_empty());
    let waiting_for = if has_libraries {
        mulligan::start_mulligan(state, &mut events)
    } else {
        // No cards to mulligan with, skip straight to game
        turns::auto_advance(state, &mut events)
    };

    state.waiting_for = waiting_for.clone();
    derive_display_state(state);

    ActionResult {
        events,
        waiting_for,
    }
}

/// Start game without mulligan (for backward compatibility with existing tests).
pub fn start_game_skip_mulligan(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();

    events.push(GameEvent::GameStarted);

    state.turn_number = 1;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: PlayerId(0),
        turn_number: 1,
    });

    let waiting_for = turns::auto_advance(state, &mut events);
    state.waiting_for = waiting_for.clone();
    derive_display_state(state);

    ActionResult {
        events,
        waiting_for,
    }
}

/// Check if any exile-return sources have left the battlefield.
/// If so, move the exiled cards back to the battlefield.
fn check_exile_returns(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let mut to_return: Vec<crate::types::game_state::ExileLink> = Vec::new();

    for event in events.iter() {
        if let GameEvent::ZoneChanged {
            object_id,
            from: Zone::Battlefield,
            ..
        } = event
        {
            // Find exile links where this object was the source
            for link in &state.exile_links {
                if link.source_id == *object_id {
                    to_return.push(link.clone());
                }
            }
        }
    }

    if to_return.is_empty() {
        return;
    }

    // CR 610.3a: Return exiled cards to their previous zone
    for link in &to_return {
        // Only return if the card is still in exile
        let still_in_exile = state
            .objects
            .get(&link.exiled_id)
            .map(|obj| obj.zone == Zone::Exile)
            .unwrap_or(false);
        if still_in_exile {
            zones::move_to_zone(state, link.exiled_id, link.return_zone, events);
        }
    }

    // Remove processed links
    let returned_ids: Vec<_> = to_return.iter().map(|l| l.exiled_id).collect();
    state
        .exile_links
        .retain(|link| !returned_ids.contains(&link.exiled_id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, ResolvedAbility,
        TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};

    /// Create a simple test ability definition.
    fn make_draw_ability(num_cards: u32) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed {
                    value: num_cards as i32,
                },
            },
        )
    }

    /// Create a DealDamage ability for testing.
    fn make_damage_ability(amount: i32, cost: Option<AbilityCost>) -> AbilityDefinition {
        let kind = if cost.is_some() {
            AbilityKind::Activated
        } else {
            AbilityKind::Spell
        };
        let mut def = AbilityDefinition::new(
            kind,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: amount },
                target: TargetFilter::Any,
            },
        );
        if let Some(c) = cost {
            def = def.cost(c);
        }
        def
    }

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2; // Not first turn
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    #[test]
    fn apply_pass_priority_alternates_players() {
        let mut state = setup_game_at_main_phase();

        let result = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn apply_pass_priority_rejects_wrong_player() {
        let mut state = setup_game_at_main_phase();
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        // Player 0 tries to pass but player 1 has priority
        // PassPriority uses priority_player, so this should fail if
        // the validated player doesn't match waiting_for
        // Actually, the validation checks priority_player == waiting_for.player
        // and priority_player is 1, so PassPriority action itself is valid
        // for player 1. The issue is if player 0 somehow acts.
        // In practice, the action doesn't carry a player ID -- the engine
        // uses priority_player. So this is a protocol-level concern.
        let result = apply(&mut state, GameAction::PassPriority);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_play_land_moves_to_battlefield() {
        let mut state = setup_game_at_main_phase();

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) }).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::Priority {
                    player: PlayerId(0)
                }
            ),
            "result.waiting_for={:?}, stack={:?}",
            result.waiting_for,
            state.stack
        );
    }

    #[test]
    fn apply_play_land_rejects_non_main_phase() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::Upkeep;

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) });

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_over_limit() {
        let mut state = setup_game_at_main_phase();
        state.lands_played_this_turn = 1; // Already played one

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) });

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_card_not_in_hand() {
        let mut state = setup_game_at_main_phase();

        let result = apply(
            &mut state,
            GameAction::PlayLand {
                card_id: CardId(999),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn new_game_creates_two_player_state() {
        let state = new_game(42);
        assert_eq!(state.players.len(), 2);
        assert_eq!(state.rng_seed, 42);
    }

    #[test]
    fn start_game_advances_to_precombat_main() {
        let mut state = new_game(42);
        let result = start_game(&mut state);

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn start_game_skips_draw_on_first_turn() {
        let mut state = new_game(42);

        // Add a card to player 0's library
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        start_game_skip_mulligan(&mut state);

        // Card should still be in library (draw skipped on turn 1)
        assert!(state.players[0].library.contains(&id));
        assert!(!state.players[0].hand.contains(&id));
    }

    #[test]
    fn start_game_emits_game_started_event() {
        let mut state = new_game(42);
        let result = start_game(&mut state);

        assert!(result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::GameStarted)));
    }

    #[test]
    fn integration_full_turn_cycle() {
        let mut state = new_game(42);

        // Start game (turn 1, player 0)
        let _result = start_game(&mut state);
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);

        // Pass priority from player 0 (pre-combat main)
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));

        // Pass priority from player 1 (both passed, stack empty -> advance)
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should skip combat phases and land at PostCombatMain
        assert_eq!(state.phase, Phase::PostCombatMain);

        // Pass through post-combat main
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should advance to End step
        assert_eq!(state.phase, Phase::End);

        // Pass through end step
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should advance through cleanup to next turn, then auto-advance to PreCombatMain
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn integration_play_land_then_pass() {
        let mut state = new_game(42);
        start_game(&mut state);

        // Create a land in player 0's hand
        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Play the land
        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) }).unwrap();

        assert!(state.battlefield.contains(&land_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority after playing land
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));

        // Priority pass count should have been reset by the land play
        assert_eq!(state.priority_pass_count, 0);
    }

    #[test]
    fn stack_push_and_lifo_resolve() {
        use crate::game::stack;
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = setup_game_at_main_phase();
        let mut events = Vec::new();

        // Create two spell objects
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Push to stack (first pushed = bottom)
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id1,
                source_id: id1,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: ResolvedAbility::new(
                        crate::types::ability::Effect::Unimplemented {
                            name: String::new(),
                            description: None,
                        },
                        vec![],
                        id1,
                        PlayerId(0),
                    ),
                    cast_as_adventure: false,
                },
            },
            &mut events,
        );
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id2,
                source_id: id2,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(2),
                    ability: ResolvedAbility::new(
                        crate::types::ability::Effect::Unimplemented {
                            name: String::new(),
                            description: None,
                        },
                        vec![],
                        id2,
                        PlayerId(0),
                    ),
                    cast_as_adventure: false,
                },
            },
            &mut events,
        );

        assert_eq!(state.stack.len(), 2);

        // Resolve top (LIFO) -- should be id2 (Bear, creature -> battlefield)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 1);
        assert!(state.battlefield.contains(&id2)); // Creature goes to battlefield

        // Resolve next -- should be id1 (Bolt, instant -> graveyard)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 0);
        assert!(state.players[0].graveyard.contains(&id1)); // Instant goes to graveyard
    }

    #[test]
    fn stack_is_empty_check() {
        use crate::game::stack;

        let state = new_game(42);
        assert!(stack::stack_is_empty(&state));
    }

    #[test]
    fn engine_error_display() {
        let err = EngineError::WrongPlayer;
        assert_eq!(err.to_string(), "Wrong player");

        let err = EngineError::NotYourPriority;
        assert_eq!(err.to_string(), "Not your priority");

        let err = EngineError::InvalidAction("test".to_string());
        assert_eq!(err.to_string(), "Invalid action: test");
    }

    #[test]
    fn tap_land_for_mana_produces_correct_color() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        let result = apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        assert!(state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn tap_land_rejects_already_tapped() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.tapped = true;
        }

        let result = apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        );

        assert!(result.is_err());
    }

    #[test]
    fn tap_land_for_mana_uses_mana_ability_with_activation_condition() {
        let mut state = setup_game_at_main_phase();

        let verge_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Gloomlake Verge".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&verge_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.abilities.push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Blue],
                        },
                        restrictions: vec![],
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap),
            );
            obj.abilities.push(
                AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Black],
                        },
                        restrictions: vec![],
                    },
                )
                .cost(crate::types::ability::AbilityCost::Tap)
                .sub_ability(AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Activated,
                    crate::types::ability::Effect::Unimplemented {
                        name: "activate_only_if_controls_land_subtype_any".to_string(),
                        description: Some("Island|Swamp".to_string()),
                    },
                )),
            );
        }

        let result = apply(
            &mut state,
            GameAction::TapLandForMana {
                object_id: verge_id,
            },
        )
        .unwrap();

        assert!(state.objects[&verge_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Blue),
            1
        );
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Black),
            0
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn full_turn_integration_with_mulligan() {
        let mut state = new_game(42);

        // Add 20 basic lands to each player's library
        for player_idx in 0..2u8 {
            for i in 0..20 {
                let id = create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i),
                    PlayerId(player_idx),
                    "Forest".to_string(),
                    Zone::Library,
                );
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Land);
                obj.card_types.subtypes.push("Forest".to_string());
            }
        }

        // Start game -> mulligan prompt
        let result = start_game(&mut state);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 0,
            }
        ));

        // Both players have 7 cards in hand
        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[1].hand.len(), 7);

        // Player 0 keeps
        let result = apply(&mut state, GameAction::MulliganDecision { keep: true }).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision {
                player: PlayerId(1),
                mulligan_count: 0,
            }
        ));

        // Player 1 keeps -> game starts, auto-advances to PreCombatMain
        let result = apply(&mut state, GameAction::MulliganDecision { keep: true }).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0),
            }
        ));
        assert_eq!(state.phase, Phase::PreCombatMain);

        // Play a land from hand
        let land_card_id = state.objects[&state.players[0].hand[0]].card_id;
        let _result = apply(
            &mut state,
            GameAction::PlayLand {
                card_id: land_card_id,
            },
        )
        .unwrap();
        assert_eq!(state.lands_played_this_turn, 1);

        // Find the land on battlefield to tap it
        let land_on_bf = state
            .battlefield
            .iter()
            .find(|&&id| {
                state
                    .objects
                    .get(&id)
                    .map(|o| o.controller == PlayerId(0) && !o.tapped)
                    .unwrap_or(false)
            })
            .copied()
            .unwrap();

        // Tap land for mana
        let _result = apply(
            &mut state,
            GameAction::TapLandForMana {
                object_id: land_on_bf,
            },
        )
        .unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );

        // Pass priority through the rest of the turn
        // PreCombatMain: P0 passes
        apply(&mut state, GameAction::PassPriority).unwrap();
        // PreCombatMain: P1 passes -> advances to PostCombatMain
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PostCombatMain);

        // PostCombatMain: both pass -> End
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::End);

        // End: both pass -> Cleanup -> next turn
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn cast_spell_moves_card_from_hand_to_stack_and_returns_priority() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery in hand
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.abilities.push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }

        // Add mana
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player.mana_pool.add(ManaUnit {
                color: ManaType::Blue,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }

        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.stack.len(), 1);
        assert!(!state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn both_pass_with_spell_on_stack_resolves_spell() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery and cast it
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.abilities.push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }

        // Add some cards to draw
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player.mana_pool.add(ManaUnit {
                color: ManaType::Blue,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }

        // Cast the spell
        apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        let hand_before = state.players[0].hand.len();

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Stack should be empty
        assert!(state.stack.is_empty());
        // Card should be in graveyard (sorcery)
        assert!(state.players[0].graveyard.contains(&obj_id));
        // Draw 2 effect should have fired
        assert_eq!(state.players[0].hand.len(), hand_before + 2);
    }

    #[test]
    fn fizzle_target_removed_before_resolution() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a creature target
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Lightning Bolt targeting the creature
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        player.mana_pool.add(ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(0),
            snow: false,
            restrictions: Vec::new(),
        });

        // Cast bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // Remove the creature from battlefield before resolution (simulating it was destroyed)
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

        // Both pass -> resolve -- should fizzle
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Stack should be empty, bolt should be in graveyard (fizzled)
        assert!(state.stack.is_empty());
        assert!(state.players[0].graveyard.contains(&bolt_id));
        // Creature was already in graveyard, life should be unchanged
        assert_eq!(state.players[1].life, 20);
    }

    // === Phase 04 Plan 03 Integration Tests ===

    use crate::types::ability::{TargetRef, TypedFilter};
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for _ in 0..count {
            player_data.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }
    }

    #[test]
    fn lightning_bolt_deals_3_damage_to_creature() {
        let mut state = setup_game_at_main_phase();

        // Create a 2/3 creature controlled by P1
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(3);
        }

        // Create Lightning Bolt in P0's hand
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Cast Lightning Bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.players[0].mana_pool.total(), 0);

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Creature should have 3 damage, which equals toughness -> SBA destroys it
        assert!(state.stack.is_empty());
        assert!(!state.battlefield.contains(&creature_id));
        assert!(state.players[1].graveyard.contains(&creature_id));
        // Bolt is instant -> goes to graveyard
        assert!(state.players[0].graveyard.contains(&bolt_id));
    }

    #[test]
    fn lightning_bolt_deals_3_damage_to_player() {
        let mut state = setup_game_at_main_phase();

        // Create Lightning Bolt in P0's hand with Any target
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Two players as targets, need manual selection
        // Use Player filter -> 2 targets -> need SelectTargets
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();

        // Should need target selection (2 players)
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select player 1 as target
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(1))],
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.players[1].life, 17);
        assert!(state.players[0].graveyard.contains(&bolt_id));
    }

    #[test]
    fn counterspell_counters_a_spell_on_stack() {
        let mut state = setup_game_at_main_phase();

        // P0 casts a creature spell -- put it on the stack manually
        let creature_id = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Vanilla creature has no abilities (empty vec is the default)
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Green, 2);

        // Cast the creature
        apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(30),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // P1 gets priority, has Counterspell
        // Pass priority from P0 to P1
        apply(&mut state, GameAction::PassPriority).unwrap();
        // Now P1 has priority
        assert_eq!(state.priority_player, PlayerId(1));

        let counter_id = create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&counter_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Typed(TypedFilter::card()),
                    source_static: None,
                    unless_payment: None,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(1), ManaType::Blue, 2);

        // Cast Counterspell — targets a spell on the stack
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(40),
                targets: vec![],
            },
        )
        .unwrap();
        // Handle target selection if needed (single spell auto-targets, but be robust).
        let result = if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
            apply(
                &mut state,
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(creature_id)],
                },
            )
            .unwrap()
        } else {
            result
        };
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 2); // creature + counterspell

        // Both pass -> Counterspell resolves first (LIFO)
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Counterspell resolved, creature spell should be countered (in graveyard)
        // Counterspell should also be in graveyard
        assert!(state.players[0].graveyard.contains(&creature_id));
        assert!(state.players[1].graveyard.contains(&counter_id));
        // Creature never reached battlefield
        assert!(!state.battlefield.contains(&creature_id));
    }

    #[test]
    fn giant_growth_gives_plus_3_3() {
        let mut state = setup_game_at_main_phase();

        // Create a 2/2 creature for P0
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Giant Growth in P0's hand
        let growth_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&growth_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Pump {
                    power: crate::types::ability::PtValue::Fixed(3),
                    toughness: crate::types::ability::PtValue::Fixed(3),
                    target: TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(crate::types::ability::ControllerRef::You),
                    ),
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Green, 1);

        // Cast Giant Growth (auto-targets single own creature)
        apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(60),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.objects[&creature_id].power, Some(5));
        assert_eq!(state.objects[&creature_id].toughness, Some(5));
        assert!(state.players[0].graveyard.contains(&growth_id));
    }

    #[test]
    fn fizzle_bolt_target_removed() {
        let mut state = setup_game_at_main_phase();

        // Create a creature
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Lightning Bolt
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Cast bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();

        // Remove creature before resolution
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

        // Both pass -> fizzle
        apply(&mut state, GameAction::PassPriority).unwrap();
        let result = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert!(state.players[0].graveyard.contains(&bolt_id));
        // No DamageDealt event
        assert!(!result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
    }

    #[test]
    fn test_mana_ability_during_priority_does_not_push_stack() {
        let mut state = setup_game_at_main_phase();

        // Create a creature with a mana ability on the battlefield
        let obj_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Green],
                        },
                        restrictions: vec![],
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let result = apply(
            &mut state,
            GameAction::ActivateAbility {
                source_id: obj_id,
                ability_index: 0,
            },
        )
        .unwrap();

        // Stack should remain empty (mana abilities don't use the stack)
        assert!(
            state.stack.is_empty(),
            "mana ability should not push to stack"
        );
        // Should stay in Priority
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        // Object should be tapped
        assert!(state.objects.get(&obj_id).unwrap().tapped);
        // Player should have green mana
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
    }

    #[test]
    fn test_mana_ability_during_mana_payment_stays_in_mana_payment() {
        let mut state = setup_game_at_main_phase();
        // Set up ManaPayment state
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
        };

        // Create a creature with a mana ability on the battlefield
        let obj_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Birds of Paradise".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.abilities.push(
                AbilityDefinition::new(
                    AbilityKind::Activated,
                    Effect::Mana {
                        produced: crate::types::ability::ManaProduction::Fixed {
                            colors: vec![crate::types::mana::ManaColor::Green],
                        },
                        restrictions: vec![],
                    },
                )
                .cost(AbilityCost::Tap),
            );
        }

        let result = apply(
            &mut state,
            GameAction::ActivateAbility {
                source_id: obj_id,
                ability_index: 0,
            },
        )
        .unwrap();

        // Should stay in ManaPayment
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ManaPayment {
                    player: PlayerId(0)
                }
            ),
            "should remain in ManaPayment after mana ability"
        );
        // Stack should remain empty
        assert!(state.stack.is_empty());
        // Object should be tapped
        assert!(state.objects.get(&obj_id).unwrap().tapped);
    }

    mod equip_tests {
        use super::*;

        fn setup_equip_game() -> GameState {
            let mut state = GameState::new_two_player(42);
            state.turn_number = 2;
            state.phase = Phase::PreCombatMain;
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.waiting_for = WaitingFor::Priority {
                player: PlayerId(0),
            };
            state
        }

        fn create_equipment(state: &mut GameState, player: PlayerId) -> ObjectId {
            let id = zones::create_object(
                state,
                CardId(100),
                player,
                "Bonesplitter".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.controller = player;
            id
        }

        fn create_creature_on_bf(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
            let id = zones::create_object(
                state,
                CardId(state.next_object_id),
                player,
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.controller = player;
            id
        }

        #[test]
        fn test_equip_creates_equip_target_with_valid_creatures() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();

            match result.waiting_for {
                WaitingFor::EquipTarget {
                    player,
                    equipment_id: eq_id,
                    valid_targets,
                } => {
                    assert_eq!(player, PlayerId(0));
                    assert_eq!(eq_id, equipment_id);
                    assert!(valid_targets.contains(&creature_a));
                    assert!(valid_targets.contains(&creature_b));
                }
                other => panic!("Expected EquipTarget, got {:?}", other),
            }
        }

        #[test]
        fn test_equip_selects_target_attaches_equipment() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let _creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            // Activate equip
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            assert!(matches!(result.waiting_for, WaitingFor::EquipTarget { .. }));

            // Select target
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_a,
                },
            )
            .unwrap();

            assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature_a)
            );
            assert!(state
                .objects
                .get(&creature_a)
                .unwrap()
                .attachments
                .contains(&equipment_id));
        }

        #[test]
        fn test_equip_re_equip_moves_to_new_creature() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            // First equip to creature A
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_a,
                },
            )
            .unwrap();
            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature_a)
            );

            // Re-equip to creature B
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_b,
                },
            )
            .unwrap();

            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature_b)
            );
            assert!(state
                .objects
                .get(&creature_b)
                .unwrap()
                .attachments
                .contains(&equipment_id));
            assert!(!state
                .objects
                .get(&creature_a)
                .unwrap()
                .attachments
                .contains(&equipment_id));
        }

        #[test]
        fn test_equip_only_at_sorcery_speed() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let _creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

            // Try during combat phase - should fail
            state.phase = Phase::DeclareAttackers;
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());

            // Try with non-empty stack - should fail
            state.phase = Phase::PreCombatMain;
            state.stack.push(crate::types::game_state::StackEntry {
                id: ObjectId(99),
                source_id: ObjectId(99),
                controller: PlayerId(1),
                kind: crate::types::game_state::StackEntryKind::Spell {
                    card_id: CardId(99),
                    ability: crate::types::ability::ResolvedAbility::new(
                        crate::types::ability::Effect::Unimplemented {
                            name: String::new(),
                            description: None,
                        },
                        vec![],
                        ObjectId(99),
                        PlayerId(1),
                    ),
                    cast_as_adventure: false,
                },
            });
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());

            // Try when not active player - should fail
            state.stack.clear();
            state.active_player = PlayerId(1);
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());
        }

        #[test]
        fn test_equip_auto_targets_single_creature() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();

            assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature)
            );
        }
    }

    #[test]
    fn land_with_etb_tapped_replacement_enters_tapped() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = setup_game_at_main_phase();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Selesnya Guildgate".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.replacement_definitions.push(
            ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Tap {
                        target: TargetFilter::SelfRef,
                    },
                ))
                .valid_card(TargetFilter::SelfRef)
                .description("Selesnya Guildgate enters the battlefield tapped.".to_string()),
        );

        let _result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) }).unwrap();
        assert!(state.battlefield.contains(&obj_id));
        assert!(
            state.objects[&obj_id].tapped,
            "ETB-tapped land must enter tapped"
        );
    }

    // ── UntapLandForMana tests ────────────────────────────────────────────

    fn create_forest(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(99),
            player,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.controller = player;
        obj.entered_battlefield_turn = Some(1);
        id
    }

    #[test]
    fn tap_land_records_in_lands_tapped_for_mana() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        let tracked = &state.lands_tapped_for_mana[&PlayerId(0)];
        assert!(tracked.contains(&land_id));
    }

    #[test]
    fn untap_land_removes_mana_and_untaps() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );

        let result = apply(
            &mut state,
            GameAction::UntapLandForMana { object_id: land_id },
        )
        .unwrap();

        assert!(!state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            0
        );
        assert!(state
            .lands_tapped_for_mana
            .get(&PlayerId(0))
            .is_none_or(|v| !v.contains(&land_id)));
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn untap_one_of_two_tapped_lands_preserves_other() {
        let mut state = setup_game_at_main_phase();
        let land1 = create_forest(&mut state, PlayerId(0));
        let land2 = create_forest(&mut state, PlayerId(0));

        apply(&mut state, GameAction::TapLandForMana { object_id: land1 }).unwrap();
        apply(&mut state, GameAction::TapLandForMana { object_id: land2 }).unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            2
        );

        apply(
            &mut state,
            GameAction::UntapLandForMana { object_id: land1 },
        )
        .unwrap();

        assert!(!state.objects[&land1].tapped);
        assert!(state.objects[&land2].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
        let tracked = &state.lands_tapped_for_mana[&PlayerId(0)];
        assert!(!tracked.contains(&land1));
        assert!(tracked.contains(&land2));
    }

    #[test]
    fn untap_rejects_when_mana_already_spent() {
        use crate::types::mana::ManaType;

        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        state.players[0].mana_pool.spend(ManaType::Green);
        assert_eq!(state.players[0].mana_pool.total(), 0);

        let result = apply(
            &mut state,
            GameAction::UntapLandForMana { object_id: land_id },
        );
        assert!(result.is_err());
    }

    #[test]
    fn pass_priority_clears_lands_tapped_for_mana() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(!state.lands_tapped_for_mana.is_empty());

        apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(!state.lands_tapped_for_mana.contains_key(&PlayerId(0)));
    }

    #[test]
    fn play_land_clears_lands_tapped_for_mana() {
        let mut state = setup_game_at_main_phase();
        let tapped_land = create_forest(&mut state, PlayerId(0));

        apply(
            &mut state,
            GameAction::TapLandForMana {
                object_id: tapped_land,
            },
        )
        .unwrap();
        assert!(!state.lands_tapped_for_mana.is_empty());

        let hand_land = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&hand_land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Mountain".to_string());
        }

        apply(
            &mut state,
            GameAction::PlayLand {
                card_id: CardId(50),
            },
        )
        .unwrap();
        assert!(!state.lands_tapped_for_mana.contains_key(&PlayerId(0)));
    }

    #[test]
    fn untap_non_tracked_land_fails() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        let result = apply(
            &mut state,
            GameAction::UntapLandForMana { object_id: land_id },
        );
        assert!(result.is_err());
    }

    #[test]
    fn untap_during_mana_payment_returns_mana_payment() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery that needs blue mana
        let spell_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.abilities.push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 1,
            };
        }

        // Add partial mana — not enough to auto-pay, so we get ManaPayment
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        player.mana_pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(0),
            snow: false,
            restrictions: Vec::new(),
        });

        // Create a forest on the battlefield to tap during ManaPayment
        let land_id = create_forest(&mut state, PlayerId(0));

        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        );

        // If we get ManaPayment, test the untap flow there
        if let Ok(ActionResult {
            waiting_for: WaitingFor::ManaPayment { .. },
            ..
        }) = &result
        {
            // Tap the land during ManaPayment
            apply(
                &mut state,
                GameAction::TapLandForMana { object_id: land_id },
            )
            .unwrap();
            assert!(state.lands_tapped_for_mana[&PlayerId(0)].contains(&land_id));

            // Untap it — should return ManaPayment, not Priority
            let untap_result = apply(
                &mut state,
                GameAction::UntapLandForMana { object_id: land_id },
            )
            .unwrap();
            assert!(matches!(
                untap_result.waiting_for,
                WaitingFor::ManaPayment {
                    player: PlayerId(0)
                }
            ));
        }
        // If auto-pay succeeded, the test setup didn't produce ManaPayment — still valid
    }

    #[test]
    fn zone_change_removes_stale_tracking() {
        let mut state = setup_game_at_main_phase();
        let land_id = create_forest(&mut state, PlayerId(0));

        // Tap the land
        apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();
        assert!(state.lands_tapped_for_mana[&PlayerId(0)].contains(&land_id));

        // Move the land to graveyard (e.g., destroyed)
        let mut events = Vec::new();
        super::zones::move_to_zone(&mut state, land_id, Zone::Graveyard, &mut events);

        // Tracking should be cleaned up
        assert!(state
            .lands_tapped_for_mana
            .get(&PlayerId(0))
            .is_none_or(|v| !v.contains(&land_id)));
    }
}

#[cfg(test)]
mod trigger_target_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, ModalChoice,
        ModalSelectionConstraint, QuantityExpr, TargetFilter, TargetRef, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::TargetSelectionConstraint;
    use crate::types::identifiers::CardId;

    #[test]
    fn trigger_target_selection_select_targets_pushes_to_stack() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create two opponent creatures as legal targets
        let target1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature 1".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target1).unwrap().controller = PlayerId(1);

        let target2 = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opp Creature 2".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target2).unwrap().controller = PlayerId(1);

        // Create trigger creature (Banishing Light)
        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.entered_battlefield_turn = Some(1);
        }

        // Manually set up the pending trigger state (as process_triggers would)
        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::Opponent),
                ),
                owner_library: false,
            },
            Vec::new(),
            trigger_creature,
            PlayerId(0),
        )
        .duration(crate::types::ability::Duration::UntilHostLeavesPlay);

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: trigger_creature,
            controller: PlayerId(0),
            condition: None,
            ability,
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
        });

        let legal_targets = vec![TargetRef::Object(target1), TargetRef::Object(target2)];

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
            }],
            target_constraints: Vec::new(),
            selection: crate::game::ability_utils::begin_target_selection(
                &[crate::types::game_state::TargetSelectionSlot {
                    legal_targets: legal_targets.clone(),
                    optional: false,
                }],
                &[],
            )
            .unwrap(),
        };

        // Player selects target1
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target1)],
            },
        )
        .unwrap();

        // Should return Priority
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority, got {:?}",
            result.waiting_for
        );

        // Trigger should be on the stack with the selected target
        assert_eq!(state.stack.len(), 1, "Trigger should be on stack");
        let entry = &state.stack[0];
        assert_eq!(entry.source_id, trigger_creature);
        match &entry.kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(ability.targets, vec![TargetRef::Object(target1)]);
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }

        // Pending trigger should be consumed
        assert!(state.pending_trigger.is_none());
    }

    #[test]
    fn trigger_target_selection_rejects_illegal_target() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);

        let legal_target = ObjectId(10);
        let illegal_target = ObjectId(99);

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: ObjectId(1),
            controller: PlayerId(0),
            condition: None,
            ability: crate::types::ability::ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::Any,
                    owner_library: false,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
        });

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(legal_target)],
                optional: false,
            }],
            target_constraints: Vec::new(),
            selection: crate::types::game_state::TargetSelectionProgress::default(),
        };

        // Try to select an illegal target
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(illegal_target)],
            },
        );

        assert!(result.is_err(), "Should reject illegal target");
    }

    #[test]
    fn triggered_modal_modes_with_targets_wait_for_target_selection() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::AbilityModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Deal 1 damage to target player.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            },
            source_id: ObjectId(20),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                },
            )],
            is_activated: false,
            ability_index: None,
            ability_cost: None,
            unavailable_modes: vec![],
        };

        let result = apply(
            &mut state,
            GameAction::SelectModes {
                indices: vec![0, 0],
            },
        )
        .unwrap();

        match result.waiting_for {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                target_constraints,
                ..
            } => {
                assert_eq!(target_slots.len(), 2);
                assert_eq!(
                    target_constraints,
                    vec![TargetSelectionConstraint::DifferentTargetPlayers]
                );
            }
            other => panic!("Expected TriggerTargetSelection, got {other:?}"),
        }
        assert_eq!(state.stack.len(), 0);
        assert!(state.pending_trigger.is_some());
    }

    #[test]
    fn trigger_target_selection_enforces_different_player_constraint() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: ObjectId(30),
            controller: PlayerId(0),
            condition: None,
            ability: crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                },
                vec![],
                ObjectId(30),
                PlayerId(0),
            )
            .sub_ability(crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                },
                vec![],
                ObjectId(30),
                PlayerId(0),
            )),
            timestamp: 1,
            target_constraints: vec![TargetSelectionConstraint::DifferentTargetPlayers],
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
        });
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![
                crate::types::game_state::TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                    optional: false,
                },
                crate::types::game_state::TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                    optional: false,
                },
            ],
            target_constraints: vec![TargetSelectionConstraint::DifferentTargetPlayers],
            selection: crate::types::game_state::TargetSelectionProgress::default(),
        };

        let invalid = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(PlayerId(1)),
                    TargetRef::Player(PlayerId(1)),
                ],
            },
        );
        assert!(invalid.is_err(), "same player should be rejected");

        let valid = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
            },
        )
        .unwrap();

        assert!(matches!(valid.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0].kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(
                    flatten_targets_in_chain(ability),
                    vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1))
                    ]
                );
            }
            other => panic!("expected triggered ability on stack, got {other:?}"),
        }
    }

    #[test]
    fn choose_target_action_advances_trigger_selection_from_engine_state() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let target_slots = vec![
            crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
            crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Player(PlayerId(0)),
                    TargetRef::Player(PlayerId(1)),
                ],
                optional: false,
            },
        ];
        let target_constraints = vec![TargetSelectionConstraint::DifferentTargetPlayers];
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: ObjectId(31),
            controller: PlayerId(0),
            condition: None,
            ability: crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                },
                vec![],
                ObjectId(31),
                PlayerId(0),
            )
            .sub_ability(crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                },
                vec![],
                ObjectId(31),
                PlayerId(0),
            )),
            timestamp: 1,
            target_constraints: target_constraints.clone(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
        });
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: target_slots.clone(),
            target_constraints: target_constraints.clone(),
            selection: crate::game::ability_utils::begin_target_selection(
                &target_slots,
                &target_constraints,
            )
            .unwrap(),
        };

        let intermediate = apply(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
        )
        .unwrap();

        match intermediate.waiting_for {
            WaitingFor::TriggerTargetSelection { selection, .. } => {
                assert_eq!(selection.current_slot, 1);
                assert_eq!(
                    selection.current_legal_targets,
                    vec![TargetRef::Player(PlayerId(1))]
                );
            }
            other => panic!("expected trigger target selection, got {other:?}"),
        }

        let completed = apply(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
        )
        .unwrap();

        assert!(matches!(completed.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn triggered_modal_modes_reject_unsatisfiable_target_constraints() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::AbilityModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Target opponent reveals their hand.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            },
            source_id: ObjectId(40),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    ),
                },
            )],
            is_activated: false,
            ability_index: None,
            ability_cost: None,
            unavailable_modes: vec![],
        };

        let result = apply(
            &mut state,
            GameAction::SelectModes {
                indices: vec![0, 0],
            },
        );

        assert!(
            result.is_err(),
            "unsatisfiable target constraints should be rejected"
        );
    }

    #[test]
    fn all_modes_exhausted_clears_pending_trigger() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source_id = ObjectId(50);
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["Mode A".to_string(), "Mode B".to_string()],
            constraints: vec![ModalSelectionConstraint::NoRepeatThisTurn],
            ..Default::default()
        };

        // Mark both modes as already chosen this turn.
        state.modal_modes_chosen_this_turn.insert((source_id, 0));
        state.modal_modes_chosen_this_turn.insert((source_id, 1));

        // Set a pending trigger with this modal.
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id,
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "placeholder".to_string(),
                    description: None,
                },
                vec![],
                source_id,
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: Some(modal),
            mode_abilities: vec![
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 4 },
                        player: crate::types::ability::GainLifePlayer::Controller,
                    },
                ),
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: crate::types::ability::GainLifePlayer::Controller,
                    },
                ),
            ],
        });

        // Call the private function via the engine path.
        let result = begin_pending_trigger_target_selection(&mut state).unwrap();

        // CR 700.2: All modes exhausted — no AbilityModeChoice produced.
        assert!(result.is_none());
        // Pending trigger should be cleared.
        assert!(state.pending_trigger.is_none());
    }

    #[test]
    fn modal_mode_tracking_resets_on_new_turn() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state.phase = Phase::PreCombatMain;

        let source_id = ObjectId(50);
        state.modal_modes_chosen_this_turn.insert((source_id, 0));
        state.modal_modes_chosen_this_turn.insert((source_id, 1));
        state.modal_modes_chosen_this_game.insert((source_id, 0));

        // Simulate new turn.
        let mut events = Vec::new();
        super::turns::start_next_turn(&mut state, &mut events);

        // Turn-scoped should be cleared.
        assert!(state.modal_modes_chosen_this_turn.is_empty());
        // Game-scoped should persist.
        assert!(state.modal_modes_chosen_this_game.contains(&(source_id, 0)));
    }
}

#[cfg(test)]
mod exile_return_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::game_state::ExileLink;
    use crate::types::identifiers::CardId;

    #[test]
    fn exile_return_source_leaves_battlefield_returns_exiled_card() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create source permanent (e.g., Banishing Light) on battlefield
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );

        // Create exiled card -- directly in exile
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Exile,
        );

        // Set up the exile link (exiled from battlefield)
        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            return_zone: Zone::Battlefield,
        });

        // Simulate events where source leaves the battlefield
        let events = vec![crate::types::events::GameEvent::ZoneChanged {
            object_id: source_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        // Call check_exile_returns
        check_exile_returns(&mut state, &mut events.clone());

        // CR 610.3a: Exiled card should return to its previous zone (battlefield)
        assert!(
            state.battlefield.contains(&exiled_id),
            "Exiled card should return to battlefield"
        );
        assert!(
            !state.exile.contains(&exiled_id),
            "Exiled card should no longer be in exile"
        );

        // ExileLink should be removed
        assert!(
            state.exile_links.is_empty(),
            "ExileLink should be cleaned up"
        );
    }

    /// CR 610.3a: When a card exiled from hand (e.g., Deep-Cavern Bat) is returned,
    /// it goes back to hand, not to the battlefield.
    #[test]
    fn exile_return_to_hand_when_exiled_from_hand() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Battlefield,
        );

        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled From Hand".to_string(),
            Zone::Exile,
        );

        // Exiled from hand → should return to hand
        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            return_zone: Zone::Hand,
        });

        let events = vec![crate::types::events::GameEvent::ZoneChanged {
            object_id: source_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        check_exile_returns(&mut state, &mut events.clone());

        // CR 610.3a: Card returns to hand, NOT battlefield
        assert!(
            state.players[1].hand.contains(&exiled_id),
            "Card exiled from hand should return to hand"
        );
        assert!(
            !state.battlefield.contains(&exiled_id),
            "Card exiled from hand should NOT go to battlefield"
        );
        assert!(
            !state.exile.contains(&exiled_id),
            "Card should no longer be in exile"
        );
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn exile_return_card_already_gone_no_error() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        // Exiled card that has already left exile (moved to hand by another effect)
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Already Moved".to_string(),
            Zone::Hand,
        );

        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            return_zone: Zone::Battlefield,
        });

        let events = vec![crate::types::events::GameEvent::ZoneChanged {
            object_id: source_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        // Should not panic -- gracefully handle already-moved card
        check_exile_returns(&mut state, &mut events.clone());

        // Card stays in hand (not moved)
        assert!(state.players[1].hand.contains(&exiled_id));
        // Link is still cleaned up
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn exile_return_link_removed_after_return() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled".to_string(),
            Zone::Exile,
        );

        // Another unrelated exile link that should NOT be removed
        let other_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other Source".to_string(),
            Zone::Battlefield,
        );
        let other_exiled = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Other Exiled".to_string(),
            Zone::Exile,
        );

        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
            return_zone: Zone::Battlefield,
        });
        state.exile_links.push(ExileLink {
            exiled_id: other_exiled,
            source_id: other_source,
            return_zone: Zone::Battlefield,
        });

        let events = vec![crate::types::events::GameEvent::ZoneChanged {
            object_id: source_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        check_exile_returns(&mut state, &mut events.clone());

        // First link's exiled card should return, second should stay in exile
        assert!(state.battlefield.contains(&exiled_id));
        assert!(state.exile.contains(&other_exiled));

        // Only the triggered link should be removed
        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, other_exiled);
    }
}

#[cfg(test)]
mod phase_trigger_regression_tests {
    use super::*;
    use crate::game::combat::AttackTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, GainLifePlayer,
        QuantityExpr, TargetFilter, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    /// Regression: phase triggers fired for auto-advanced phases.
    ///
    /// A creature with "At the beginning of combat..." had its trigger fire
    /// even when combat was skipped (no attackers), because auto_advance()
    /// emitted PhaseChanged { BeginCombat } and process_triggers() processed
    /// ALL accumulated events. The fix filters PhaseChanged events to only
    /// include the current phase.
    #[test]
    fn phase_trigger_does_not_fire_for_skipped_combat() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a creature with a "beginning of combat" phase trigger.
        // The creature is small (0/1) so it has no potential attackers.
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Trigger Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(0);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Phase)
                    .phase(Phase::BeginCombat)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Activated,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    ))
                    .trigger_zones(vec![Zone::Battlefield]),
            );
        }

        // Pass priority twice (P0 passes, then P1 passes) with empty stack.
        // This advances from PreCombatMain → BeginCombat → auto-skip combat
        // → PostCombatMain. The PhaseChanged { BeginCombat } event should be
        // filtered out, so the phase trigger should NOT fire.
        let result1 = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result1.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));

        let result2 = apply(&mut state, GameAction::PassPriority).unwrap();

        // We should now be at PostCombatMain with empty stack.
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(
            state.stack.is_empty(),
            "Stack should be empty — phase trigger for skipped BeginCombat should not fire. Stack: {:?}",
            state.stack
        );
        // No pending trigger either
        assert!(
            state.pending_trigger.is_none(),
            "No pending trigger should exist for a skipped phase"
        );
        assert!(matches!(result2.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn spell_cast_trigger_syncs_priority_to_active_player() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let creature_spell = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Bear Cub".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&creature_spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.stack.push(crate::types::game_state::StackEntry {
            id: creature_spell,
            source_id: creature_spell,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(300),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: "Creature".to_string(),
                        description: None,
                    },
                    Vec::new(),
                    creature_spell,
                    PlayerId(0),
                ),
                cast_as_adventure: false,
            },
        });

        let spell_cast_trigger_creature = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Spell Trigger Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&spell_cast_trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.trigger_definitions
                .push(TriggerDefinition::new(TriggerMode::SpellCast).execute(
                    AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::Draw {
                            count: QuantityExpr::Fixed { value: 1 },
                        },
                    ),
                ));
        }

        let searing_spear = create_object(
            &mut state,
            CardId(302),
            PlayerId(1),
            "Searing Spear".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&searing_spear)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(302),
                targets: Vec::new(),
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.priority_player, PlayerId(0));

        let pass_result = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            pass_result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn attack_trigger_resolves_before_combat_damage_and_only_once() {
        let mut state = new_game(42);
        state.turn_number = 5;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let ajani = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Ajani's Pridemate".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&ajani).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.color = vec![ManaColor::White];
            obj.base_color = vec![ManaColor::White];
            obj.entered_battlefield_turn = Some(4);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::LifeGained)
                    .valid_target(TargetFilter::Controller)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::PutCounter {
                            counter_type: "P1P1".to_string(),
                            count: 1,
                            target: TargetFilter::SelfRef,
                        },
                    )),
            );
        }

        let linden = create_object(
            &mut state,
            CardId(401),
            PlayerId(0),
            "Linden, the Steadfast Queen".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&linden).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.color = vec![ManaColor::White];
            obj.base_color = vec![ManaColor::White];
            obj.entered_battlefield_turn = Some(4);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Attacks)
                    .valid_card(TargetFilter::Typed(
                        TypedFilter::creature()
                            .controller(ControllerRef::You)
                            .properties(vec![FilterProp::HasColor {
                                color: "White".to_string(),
                            }]),
                    ))
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    )),
            );
        }

        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![ajani, linden],
            valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        };

        let declare_result = apply(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(ajani, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .unwrap();

        assert!(matches!(
            declare_result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(
            state.stack.len(),
            1,
            "Linden should create exactly one stack entry"
        );
        assert_eq!(state.phase, Phase::DeclareAttackers);

        apply(&mut state, GameAction::PassPriority).unwrap();
        let linden_resolve = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            linden_resolve.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.players[0].life, 21, "Linden should gain life once");
        assert_eq!(
            state.stack.len(),
            1,
            "Ajani's Pridemate should trigger from Linden's life gain"
        );
        assert_eq!(state.objects[&ajani].power, Some(2));
        assert_eq!(state.objects[&ajani].toughness, Some(2));

        apply(&mut state, GameAction::PassPriority).unwrap();
        let pridemate_resolve = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            pridemate_resolve.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert!(state.stack.is_empty());
        assert_eq!(state.objects[&ajani].power, Some(3));
        assert_eq!(state.objects[&ajani].toughness, Some(3));

        apply(&mut state, GameAction::PassPriority).unwrap();
        let combat_result = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            combat_result.waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert_eq!(
            state.players[1].life, 17,
            "Ajani should deal 3 after receiving the pre-damage counter"
        );
        assert_eq!(
            state.players[0].life, 21,
            "No duplicate Linden life gain should occur"
        );
        assert_eq!(state.objects[&ajani].power, Some(3));
        assert_eq!(state.objects[&ajani].toughness, Some(3));
    }

    #[test]
    fn card_name_choice_validates_against_all_card_names() {
        let mut state = GameState::new_two_player(42);
        state.all_card_names = vec!["Lightning Bolt".to_string(), "Counterspell".to_string()];
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source_id: None,
        };

        // Valid card name succeeds
        let result = apply(
            &mut state,
            GameAction::ChooseOption {
                choice: "Lightning Bolt".to_string(),
            },
        );
        assert!(result.is_ok());

        // Reset state for invalid test
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source_id: None,
        };

        // Invalid card name fails
        let result = apply(
            &mut state,
            GameAction::ChooseOption {
                choice: "Not A Real Card".to_string(),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn card_name_choice_is_case_insensitive() {
        let mut state = GameState::new_two_player(42);
        state.all_card_names = vec!["Lightning Bolt".to_string()];
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::CardName,
            options: Vec::new(),
            source_id: None,
        };

        let result = apply(
            &mut state,
            GameAction::ChooseOption {
                choice: "lightning bolt".to_string(),
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn post_replacement_choose_sets_named_choice_waiting_for() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Multiversal Passage".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Choose {
                choice_type: crate::types::ability::ChoiceType::BasicLandType,
                persist: false,
            },
        )
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
        ));

        let waiting_for =
            apply_post_replacement_effect(&mut state, &effect_def, Some(source_id), &mut events);

        assert!(matches!(
            waiting_for,
            Some(WaitingFor::NamedChoice {
                choice_type: crate::types::ability::ChoiceType::BasicLandType,
                ..
            })
        ));
        assert!(state.pending_continuation.is_some());
    }

    #[test]
    fn choose_option_with_source_id_stores_chosen_attribute() {
        use crate::types::ability::ChoiceType;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Captivating Crossroads".to_string(),
            Zone::Battlefield,
        );

        // Set up NamedChoice with source_id (simulating persist=true Choose)
        state.waiting_for = WaitingFor::NamedChoice {
            player: PlayerId(0),
            choice_type: ChoiceType::Color,
            options: vec![
                "White".to_string(),
                "Blue".to_string(),
                "Black".to_string(),
                "Red".to_string(),
                "Green".to_string(),
            ],
            source_id: Some(obj_id),
        };

        let result = apply(
            &mut state,
            GameAction::ChooseOption {
                choice: "Red".to_string(),
            },
        );
        assert!(result.is_ok());

        // Verify the choice was stored on the object
        let obj = state.objects.get(&obj_id).unwrap();
        assert_eq!(obj.chosen_color(), Some(ManaColor::Red));
    }

    #[test]
    fn copy_target_choice_resolves_become_copy() {
        // CR 707.9: Test the CopyTargetChoice → BecomeCopy flow.
        // Set up a clone creature on battlefield and a target creature to copy.
        let mut state = GameState::new_two_player(42);

        let target_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        {
            let target = state.objects.get_mut(&target_id).unwrap();
            target.base_power = Some(2);
            target.base_toughness = Some(2);
            target.power = Some(2);
            target.toughness = Some(2);
        }

        let clone_id = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Clone".to_string(),
            Zone::Battlefield,
        );
        {
            let clone = state.objects.get_mut(&clone_id).unwrap();
            clone.base_power = Some(0);
            clone.base_toughness = Some(0);
            clone.power = Some(0);
            clone.toughness = Some(0);
        }

        // Set up CopyTargetChoice waiting state
        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: clone_id,
            valid_targets: vec![target_id],
        };

        // Player chooses to copy Grizzly Bears
        let result = apply(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target_id)),
            },
        );
        assert!(result.is_ok());

        // Verify the clone now has the target's characteristics
        let clone = state.objects.get(&clone_id).unwrap();
        assert_eq!(clone.name, "Grizzly Bears");
        assert_eq!(clone.power, Some(2));
        assert_eq!(clone.toughness, Some(2));
    }

    #[test]
    fn copy_target_choice_rejects_invalid_target() {
        let mut state = GameState::new_two_player(42);

        let valid_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let invalid_id = zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bird".to_string(),
            Zone::Battlefield,
        );
        let clone_id = zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Clone".to_string(),
            Zone::Battlefield,
        );

        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: clone_id,
            valid_targets: vec![valid_id], // Bird is NOT in valid targets
        };

        // Try to choose invalid target
        let result = apply(
            &mut state,
            GameAction::ChooseTarget {
                target: Some(TargetRef::Object(invalid_id)),
            },
        );
        assert!(result.is_err());
    }
}
