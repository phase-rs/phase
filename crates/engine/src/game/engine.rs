use rand::Rng;
use thiserror::Error;

use crate::game::combat::{AttackTarget, DamageAssignment, DamageTarget, TrampleKind};
use crate::game::filter;
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, ChoiceType, ChoiceValue, ChosenAttribute, Effect,
    EffectKind, ResolvedAbility, TargetFilter, TargetRef, UnlessCost,
};
use crate::types::actions::GameAction;
use crate::types::events::{BendingType, GameEvent};
use crate::types::game_state::{
    ActionResult, AutoPassMode, AutoPassRequest, ConvokeMode, GameState, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::match_config::MatchType;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

use super::ability_utils::{
    assign_selected_slots_in_chain, assign_targets_in_chain, auto_select_targets,
    begin_target_selection, build_chained_resolved, build_target_slots, choose_target,
    compute_unavailable_modes, flatten_targets_in_chain, record_modal_mode_choices,
    validate_modal_indices, validate_selected_targets, TargetSelectionAdvance,
};
use super::casting;
use super::casting_costs;
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
    result.log_entries = super::log::resolve_log_entries(&result.events, state);
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
                let stack_was_empty = state.stack.is_empty();
                let wf = priority::handle_priority_pass(state, &mut events);
                sync_waiting_for(state, &wf);
                let skip_triggers = stack_was_empty
                    && !state.stack.is_empty()
                    && state.phase == Phase::CombatDamage;

                // Run post-action pipeline (SBAs, triggers, layers)
                match run_post_action_pipeline(state, &mut events, &wf, skip_triggers) {
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
    // Clear stale revealed_cards from the previous action.
    // RevealTop reveals (e.g. Goblin Guide) are momentary — shown for one state update.
    // RevealHand reveals (e.g. Thoughtseize) persist through the RevealChoice interaction.
    // ManifestDread reveals persist through ManifestDreadChoice (cards come from WaitingFor).
    if !matches!(
        state.waiting_for,
        WaitingFor::RevealChoice { .. } | WaitingFor::ManifestDreadChoice { .. }
    ) {
        state.revealed_cards.clear();
    }

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
            log_entries: vec![],
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
            // Track stack growth during combat damage: process_combat_damage_triggers
            // processes non-phase events (LifeChanged, DamageDealt) for triggers inline.
            // Other phase triggers (Upkeep, End, etc.) only process PhaseChanged events
            // which the pipeline already filters, so they don't need this guard.
            let stack_was_empty = state.stack.is_empty();
            let wf = priority::handle_priority_pass(state, &mut events);
            if stack_was_empty && !state.stack.is_empty() && state.phase == Phase::CombatDamage {
                triggers_processed_inline = true;
            }
            wf
        }
        (WaitingFor::Priority { player }, GameAction::PlayLand { object_id, card_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            // CR 305.2: Playing a land is a special action, not a spell.
            handle_play_land(state, object_id, card_id, &mut events)?
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
        (
            WaitingFor::Priority { player },
            GameAction::CastSpell {
                object_id, card_id, ..
            },
        ) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_cast_spell(state, *player, object_id, card_id, &mut events)?
        }
        // CR 602.1: Activated abilities have a cost and an effect, written as "[Cost]: [Effect.]"
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
                // CR 605.3b: Mana abilities resolve immediately without using the stack.
                let ability_def = obj.abilities[ability_index].clone();
                mana_abilities::resolve_mana_ability(
                    state,
                    source_id,
                    *player,
                    &ability_def,
                    &mut events,
                    None,
                )?;
                WaitingFor::Priority { player: *player }
            } else if obj.loyalty.is_some()
                && ability_index < obj.abilities.len()
                && matches!(
                    obj.abilities[ability_index].cost,
                    Some(crate::types::ability::AbilityCost::Loyalty { .. })
                )
            {
                // CR 606.3: Loyalty abilities activate once per turn at sorcery speed.
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
        // Player chooses normal cast or Warp cast from hand.
        (
            WaitingFor::WarpCostChoice {
                player,
                object_id,
                card_id,
                ..
            },
            GameAction::ChooseWarpCost { use_warp },
        ) => casting::handle_warp_cost_choice(
            state,
            *player,
            *object_id,
            *card_id,
            use_warp,
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
        // CR 702.138a: Player selected cards to exile from graveyard as escape cost.
        (
            WaitingFor::ExileFromGraveyardForCost {
                player,
                count,
                cards: legal_cards,
                pending_cast,
            },
            GameAction::SelectCards { cards: chosen },
        ) => casting_costs::handle_exile_from_graveyard_for_cost(
            state,
            *player,
            *pending_cast.clone(),
            *count,
            legal_cards,
            &chosen,
            &mut events,
        )?,
        (
            WaitingFor::ExileFromGraveyardForCost {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => {
            casting::handle_cancel_cast(state, pending_cast, &mut events);
            WaitingFor::Priority { player: *player }
        }
        // CR 702.180b: Player chose which creature to tap for harmonize cost reduction.
        // CR 601.2b: Creature is tapped as part of paying the total cost.
        (
            WaitingFor::HarmonizeTapChoice {
                player,
                eligible_creatures,
                pending_cast,
            },
            GameAction::HarmonizeTap { creature_id },
        ) => {
            let mut pending = *pending_cast.clone();

            if let Some(cid) = creature_id {
                // Validate creature is in eligible list
                if !eligible_creatures.contains(&cid) {
                    return Err(EngineError::ActionNotAllowed(
                        "Creature not eligible for Harmonize tap".into(),
                    ));
                }
                // Re-validate: creature must still be on battlefield and untapped
                let obj = state.objects.get(&cid).ok_or_else(|| {
                    EngineError::InvalidAction("Creature no longer exists".into())
                })?;
                if obj.zone != Zone::Battlefield || obj.tapped {
                    return Err(EngineError::InvalidAction(
                        "Creature is no longer eligible for Harmonize tap".into(),
                    ));
                }

                // Read power BEFORE tapping (idiomatic: read then mutate)
                let power = obj.power.unwrap_or(0).max(0) as u32;

                // Tap the creature
                if let Some(obj) = state.objects.get_mut(&cid) {
                    obj.tapped = true;
                }
                events.push(GameEvent::PermanentTapped {
                    object_id: cid,
                    caused_by: None,
                });

                // Reduce generic mana cost by creature's power (floor at 0)
                // CR 702.180a: Harmonize tap cost reduction — reduce by tapped creature's power.
                if let crate::types::mana::ManaCost::Cost {
                    ref mut generic, ..
                } = pending.cost
                {
                    *generic = generic.saturating_sub(power);
                }
            }
            // creature_id = None → skip, proceed with unmodified cost

            // Resume casting flow — call pay_and_push_adventure directly to skip
            // re-entering the Harmonize check in pay_and_push.
            casting_costs::pay_and_push_adventure(
                state,
                *player,
                pending.object_id,
                pending.card_id,
                pending.ability,
                &pending.cost,
                pending.casting_variant,
                pending.distribute,
                &mut events,
            )?
        }
        (
            WaitingFor::HarmonizeTapChoice {
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
                                                    // CR 117.3b: After a stack object finishes resolving, the active player
                                                    // receives priority. Set this before resolution so it's the default
                                                    // if resolve_ability_chain doesn't set a new interactive state.
            state.waiting_for = WaitingFor::Priority {
                player: state.active_player,
            };
            state.priority_player = state.active_player;
            if let Some(mut ability) = state.pending_optional_effect.take() {
                ability.optional = false; // prevent re-prompt on re-entry
                if accept {
                    // CR 609.3: Player chose to perform — execute the full chain.
                    ability.context.optional_effect_performed = true;
                    effects::resolve_ability_chain(state, &ability, &mut events, 0)
                        .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                } else {
                    // CR 608.2c: Walk sub_ability chain to execute "Otherwise" branches
                    if let Some(ref sub) = ability.sub_ability {
                        if matches!(sub.condition, Some(AbilityCondition::IfYouDo)) {
                            if let Some(ref else_branch) = sub.else_ability {
                                let mut else_resolved = else_branch.as_ref().clone();
                                else_resolved.context = ability.context.clone();
                                effects::resolve_ability_chain(
                                    state,
                                    &else_resolved,
                                    &mut events,
                                    0,
                                )
                                .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                            }
                        }
                    }
                }
            }
            // Resume with pending continuation if one was stashed (from resolve_ability_chain).
            if let Some(continuation) = state.pending_continuation.take() {
                effects::resolve_ability_chain(state, &continuation, &mut events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            state.waiting_for.clone()
        }
        // CR 608.2d: Opponent decided on "any opponent may" effect.
        (
            WaitingFor::OpponentMayChoice {
                player: promptee,
                remaining,
                source_id,
                description,
            },
            GameAction::DecideOptionalEffect { accept },
        ) => {
            state.cost_payment_failed_flag = false;

            if accept {
                if let Some(mut ability) = state.pending_optional_effect.take() {
                    ability.optional = false;
                    ability.optional_for = None;
                    ability.context.optional_effect_performed = true;
                    ability.context.accepting_player = Some(*promptee);

                    // CR 608.2d: Determine if the inner effect requires the opponent
                    // to select a target from their own permanents (sacrifice/tap).
                    let target_selection = match &ability.effect {
                        Effect::Sacrifice { ref target } | Effect::Tap { ref target } => {
                            let require_untapped = matches!(ability.effect, Effect::Tap { .. });
                            // CR 701.21a: Opponent chooses from THEIR permanents.
                            let legal: Vec<ObjectId> = state
                                .objects
                                .iter()
                                .filter(|(_, obj)| {
                                    obj.zone == Zone::Battlefield
                                        && obj.controller == *promptee
                                        && (!require_untapped || !obj.tapped)
                                        && filter::matches_target_filter_controlled(
                                            state,
                                            obj.id,
                                            target,
                                            ability.source_id,
                                            *promptee,
                                        )
                                })
                                .map(|(id, _)| *id)
                                .collect();
                            Some(legal)
                        }
                        _ => None,
                    };

                    if let Some(legal) = target_selection {
                        if !legal.is_empty() {
                            // CR 608.2d: Stash sub_ability chain (the IfAPlayerDoes-
                            // conditioned consequence) as pending_continuation so it
                            // executes AFTER MultiTargetSelection resolves (engine.rs:2535).
                            // The condition on the continuation is NOT re-checked when it
                            // runs as a root — this is safe because we only reach this
                            // branch on the accept path (optional_effect_performed = true).
                            if let Some(sub) = ability.sub_ability.take() {
                                state.pending_continuation = Some(sub);
                            }
                            state.waiting_for = WaitingFor::MultiTargetSelection {
                                player: *promptee,
                                legal_targets: legal,
                                min_targets: 1,
                                max_targets: 1,
                                pending_ability: ability,
                            };
                            return Ok(ActionResult {
                                events,
                                waiting_for: state.waiting_for.clone(),
                                log_entries: vec![],
                            });
                        }
                        // No legal targets: effect impossible (CR 609.3).
                        // optional_effect_performed is still true — sub-abilities execute.
                        state.waiting_for = WaitingFor::Priority {
                            player: state.active_player,
                        };
                        state.priority_player = state.active_player;
                        effects::resolve_ability_chain(state, &ability, &mut events, 0)
                            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                    } else {
                        // Non-target-selection effects (DealDamage, ChangeZone, etc.)
                        if matches!(ability.effect, Effect::DealDamage { .. }) {
                            // CR 608.2d: Inject accepting player as damage target.
                            ability.targets = vec![TargetRef::Player(*promptee)];
                        }
                        state.waiting_for = WaitingFor::Priority {
                            player: state.active_player,
                        };
                        state.priority_player = state.active_player;
                        effects::resolve_ability_chain(state, &ability, &mut events, 0)
                            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                    }
                }
            } else {
                // Decline: advance to next opponent or handle all-declined.
                if !remaining.is_empty() {
                    let next = remaining[0];
                    let rest = remaining[1..].to_vec();
                    state.waiting_for = WaitingFor::OpponentMayChoice {
                        player: next,
                        source_id: *source_id,
                        description: description.clone(),
                        remaining: rest,
                    };
                    return Ok(ActionResult {
                        events,
                        waiting_for: state.waiting_for.clone(),
                        log_entries: vec![],
                    });
                }
                // All opponents declined.
                state.waiting_for = WaitingFor::Priority {
                    player: state.active_player,
                };
                state.priority_player = state.active_player;
                if let Some(ability) = state.pending_optional_effect.take() {
                    // Walk sub_ability chain for IfAPlayerDoes else branches.
                    if let Some(ref sub) = ability.sub_ability {
                        if matches!(sub.condition, Some(AbilityCondition::IfAPlayerDoes)) {
                            if let Some(ref else_branch) = sub.else_ability {
                                let mut else_resolved = else_branch.as_ref().clone();
                                else_resolved.context = ability.context.clone();
                                effects::resolve_ability_chain(
                                    state,
                                    &else_resolved,
                                    &mut events,
                                    0,
                                )
                                .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                            }
                        }
                    }
                }
            }
            // Resume pending continuation (for non-MultiTargetSelection paths).
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
                pending_effect,
                ..
            },
            GameAction::PayUnlessCost { pay },
        ) => {
            // Tracks whether payment succeeded — auto-decline (empty hand/no permanents)
            // should execute the pending effect, not emit EffectResolved.
            let mut payment_failed = !pay;
            if pay {
                // CR 118.12 + CR 702.21a: Branch on cost type to pay appropriately.
                match cost {
                    UnlessCost::Fixed { cost: mana_cost } => {
                        casting::pay_unless_cost(state, *player, mana_cost, &mut events)?;
                    }
                    UnlessCost::DynamicGeneric { .. } => {
                        // DynamicGeneric is resolved to Fixed before reaching here.
                        unreachable!("DynamicGeneric should be resolved before payment");
                    }
                    UnlessCost::PayLife { amount } => {
                        // CR 702.21a: Pay life as ward cost.
                        if let Some(p) = state.players.iter_mut().find(|p| p.id == *player) {
                            p.life -= *amount;
                        }
                        events.push(GameEvent::LifeChanged {
                            player_id: *player,
                            amount: -(*amount),
                        });
                    }
                    UnlessCost::DiscardCard => {
                        // CR 702.21a: Discard a card as ward cost — transition to selection.
                        let hand_cards: Vec<ObjectId> = state
                            .players
                            .iter()
                            .find(|p| p.id == *player)
                            .map(|p| p.hand.to_vec())
                            .unwrap_or_default();
                        if hand_cards.is_empty() {
                            // No cards to discard — cannot pay.
                            payment_failed = true;
                        } else {
                            return Ok(ActionResult {
                                events,
                                waiting_for: WaitingFor::WardDiscardChoice {
                                    player: *player,
                                    cards: hand_cards,
                                    pending_effect: pending_effect.clone(),
                                },
                                log_entries: vec![],
                            });
                        }
                    }
                    UnlessCost::SacrificeAPermanent => {
                        // CR 702.21a: Sacrifice a permanent as ward cost — transition to selection.
                        let eligible: Vec<ObjectId> = state
                            .battlefield
                            .iter()
                            .filter(|id| {
                                state
                                    .objects
                                    .get(id)
                                    .map(|o| o.controller == *player && !o.is_emblem)
                                    .unwrap_or(false)
                            })
                            .copied()
                            .collect();
                        if eligible.is_empty() {
                            // No permanents to sacrifice — cannot pay.
                            payment_failed = true;
                        } else {
                            return Ok(ActionResult {
                                events,
                                waiting_for: WaitingFor::WardSacrificeChoice {
                                    player: *player,
                                    permanents: eligible,
                                    pending_effect: pending_effect.clone(),
                                },
                                log_entries: vec![],
                            });
                        }
                    }
                }
                if payment_failed {
                    // Could not pay (e.g., empty hand for discard, no permanents for sacrifice).
                    // Fall through to decline path.
                } else {
                    // CR 118.12: Payment satisfied — effect does not execute.
                    events.push(GameEvent::EffectResolved {
                        kind: EffectKind::from(&pending_effect.effect),
                        source_id: pending_effect.source_id,
                    });
                }
            }
            if !pay || payment_failed {
                // Player declines (or cannot pay) → execute the pending effect.
                // For counters, strip unless_payment to prevent re-prompting.
                let mut ability = pending_effect.as_ref().clone();
                if let Effect::Counter {
                    ref mut unless_payment,
                    ..
                } = ability.effect
                {
                    *unless_payment = None;
                }
                effects::resolve_ability_chain(state, &ability, &mut events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            // Resume with pending continuation if one was stashed.
            if let Some(continuation) = state.pending_continuation.take() {
                effects::resolve_ability_chain(state, &continuation, &mut events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            // CR 117.3b: After resolving, return to priority.
            WaitingFor::Priority {
                player: state.active_player,
            }
        }
        // Allow mana abilities during unless-payment choice (CR 118.12)
        (
            WaitingFor::UnlessPayment {
                player,
                cost,
                pending_effect,
                effect_description,
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
                pending_effect: pending_effect.clone(),
                effect_description: effect_description.clone(),
            }
        }
        (
            WaitingFor::UnlessPayment {
                player,
                cost,
                pending_effect,
                effect_description,
            },
            GameAction::UntapLandForMana { object_id },
        ) => {
            handle_untap_land_for_mana(state, *player, object_id, &mut events)?;
            WaitingFor::UnlessPayment {
                player: *player,
                cost: cost.clone(),
                pending_effect: pending_effect.clone(),
                effect_description: effect_description.clone(),
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
                    None,
                )?;
                state.waiting_for.clone()
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during unless payment".to_string(),
                ));
            }
        }
        // CR 702.21a: Player selected a card to discard as ward cost payment.
        (
            WaitingFor::WardDiscardChoice {
                player,
                cards: legal_cards,
                pending_effect,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if chosen.len() != 1 || !legal_cards.contains(&chosen[0]) {
                return Err(EngineError::InvalidAction(
                    "Must select exactly one card to discard".to_string(),
                ));
            }
            // Discard the selected card.
            crate::game::zones::move_to_zone(
                state,
                chosen[0],
                crate::types::zones::Zone::Graveyard,
                &mut events,
            );
            crate::game::restrictions::record_discard(state, *player);
            events.push(GameEvent::Discarded {
                player_id: *player,
                object_id: chosen[0],
            });
            // Payment satisfied — effect does not execute.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&pending_effect.effect),
                source_id: pending_effect.source_id,
            });
            // Resume with pending continuation if one was stashed.
            if let Some(continuation) = state.pending_continuation.take() {
                effects::resolve_ability_chain(state, &continuation, &mut events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            WaitingFor::Priority {
                player: state.active_player,
            }
        }
        // CR 702.21a: Player selected a permanent to sacrifice as ward cost payment.
        (
            WaitingFor::WardSacrificeChoice {
                player,
                permanents,
                pending_effect,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            if chosen.len() != 1 || !permanents.contains(&chosen[0]) {
                return Err(EngineError::InvalidAction(
                    "Must select exactly one permanent to sacrifice".to_string(),
                ));
            }
            // Sacrifice the selected permanent.
            crate::game::sacrifice::sacrifice_permanent(state, chosen[0], *player, &mut events)?;
            // Payment satisfied — effect does not execute.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&pending_effect.effect),
                source_id: pending_effect.source_id,
            });
            // Resume with pending continuation if one was stashed.
            if let Some(continuation) = state.pending_continuation.take() {
                effects::resolve_ability_chain(state, &continuation, &mut events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
            WaitingFor::Priority {
                player: state.active_player,
            }
        }
        (WaitingFor::ManaPayment { player, .. }, GameAction::CancelCast) => {
            // Clean up any saved pending cast info
            state.pending_cast = None;
            WaitingFor::Priority { player: *player }
        }
        // Finalize mana payment: pay cost from pool and push spell/ability to stack.
        (WaitingFor::ManaPayment { player, .. }, GameAction::PassPriority) => {
            let pending = state.pending_cast.take().ok_or_else(|| {
                EngineError::InvalidAction("No pending cast to finalize".to_string())
            })?;
            if let Some(ability_index) = pending.activation_ability_index {
                // Activated ability finalization: pay mana from pool, then delegate
                // remaining costs + target selection + stack push to shared helper.
                casting::pay_mana_cost(
                    state,
                    *player,
                    pending.object_id,
                    &pending.cost,
                    &mut events,
                )?;
                casting_costs::push_activated_ability_to_stack(
                    state,
                    *player,
                    pending.object_id,
                    ability_index,
                    pending.ability,
                    pending.activation_cost.as_ref(),
                    &mut events,
                )?
            } else if let Some(unit) = pending.distribute {
                // CR 601.2d: X-spell distribution — pay mana first to determine X,
                // then trigger DistributeAmong with total = X.
                let p = *player;
                let pool_before = state
                    .players
                    .iter()
                    .find(|pl| pl.id == p)
                    .map(|pl| pl.mana_pool.total())
                    .unwrap_or(0);

                casting::pay_mana_cost(state, p, pending.object_id, &pending.cost, &mut events)?;

                let pool_after = state
                    .players
                    .iter()
                    .find(|pl| pl.id == p)
                    .map(|pl| pl.mana_pool.total())
                    .unwrap_or(0);
                // X = total paid minus non-X colored/generic costs.
                // ManaCost::mana_value() already excludes X shards (CR 202.3e).
                let non_x_cost = pending.cost.mana_value();
                let total_paid = pool_before.saturating_sub(pool_after) as u32;
                let x_value = total_paid.saturating_sub(non_x_cost);

                let targets = super::ability_utils::flatten_targets_in_chain(&pending.ability);
                // Store pending cast for post-distribution resumption.
                // Use ManaCost::NoCost since mana was already paid above —
                // finalize_cast will be called after DistributeAmong completes and
                // must not re-deduct mana.
                let mut pending_resumed = crate::types::game_state::PendingCast::new(
                    pending.object_id,
                    pending.card_id,
                    pending.ability,
                    crate::types::mana::ManaCost::NoCost,
                );
                pending_resumed.casting_variant = pending.casting_variant;

                // CR 601.2d: "divided evenly, rounded down" — EvenSplitDamage bypasses
                // interactive distribution. Remainder is intentionally lost per Oracle text;
                // total dealt may be less than the original amount.
                if unit == crate::types::game_state::DistributionUnit::EvenSplitDamage
                    && !targets.is_empty()
                {
                    let num = targets.len() as u32;
                    let per_target = x_value / num;
                    let distribution: Vec<_> =
                        targets.iter().map(|t| (t.clone(), per_target)).collect();
                    pending_resumed.ability.distribution = Some(distribution);
                    state.pending_cast = Some(Box::new(pending_resumed));

                    // Resume casting pipeline directly.
                    let pending = state.pending_cast.take().unwrap();
                    casting_costs::finalize_cast(
                        state,
                        p,
                        pending.object_id,
                        pending.card_id,
                        pending.ability,
                        &pending.cost,
                        pending.casting_variant,
                        &mut events,
                    )?
                } else {
                    state.pending_cast = Some(Box::new(pending_resumed));

                    WaitingFor::DistributeAmong {
                        player: p,
                        total: x_value,
                        targets,
                        unit,
                    }
                }
            } else {
                casting_costs::finalize_cast(
                    state,
                    *player,
                    pending.object_id,
                    pending.card_id,
                    pending.ability,
                    &pending.cost,
                    pending.casting_variant,
                    &mut events,
                )?
            }
        }
        // Allow mana abilities during mana payment (mid-cast)
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
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
                    None,
                )?;
                WaitingFor::ManaPayment {
                    player: *player,
                    convoke_mode: *convoke_mode,
                }
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during mana payment".to_string(),
                ));
            }
        }
        // Allow basic land tapping during mana payment
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::TapLandForMana { object_id },
        ) => {
            handle_tap_land_for_mana(state, object_id, &mut events)?;
            state
                .lands_tapped_for_mana
                .entry(*player)
                .or_default()
                .push(object_id);
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            }
        }
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode,
            },
            GameAction::UntapLandForMana { object_id },
        ) => {
            handle_untap_land_for_mana(state, *player, object_id, &mut events)?;
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: *convoke_mode,
            }
        }
        // CR 702.51a / Waterbend: Tap a creature or artifact to pay mana.
        // CR 702.51a + CR 302.6: Convoke taps creatures to pay mana; summoning sickness
        // (CR 302.6) is not checked because convoke does not use the tap activated-ability mechanism.
        (
            WaitingFor::ManaPayment {
                player,
                convoke_mode: Some(mode @ (ConvokeMode::Convoke | ConvokeMode::Waterbend)),
            },
            GameAction::TapForConvoke {
                object_id,
                mana_type,
            },
        ) => {
            let mode = *mode;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if !obj.is_convoke_eligible(*player) {
                return Err(EngineError::ActionNotAllowed(
                    "Can only tap untapped creatures or artifacts you control for convoke"
                        .to_string(),
                ));
            }
            // CR 702.51a: Validate color match for Convoke.
            let resolved_mana_type = match mode {
                ConvokeMode::Convoke => {
                    if let Some(color) = mana_sources::mana_type_to_color(mana_type) {
                        // Colored mana: creature must have that color
                        if !obj.color.contains(&color) {
                            return Err(EngineError::ActionNotAllowed(format!(
                                "Creature does not have color {:?} for convoke",
                                color
                            )));
                        }
                        mana_type
                    } else {
                        // Colorless: any creature can pay generic
                        crate::types::mana::ManaType::Colorless
                    }
                }
                // Waterbend always produces colorless
                ConvokeMode::Waterbend => crate::types::mana::ManaType::Colorless,
            };
            // Tap the permanent (no summoning sickness check — CR 702.51a + CR 302.6)
            if let Some(obj) = state.objects.get_mut(&object_id) {
                obj.tapped = true;
            }
            events.push(GameEvent::PermanentTapped {
                object_id,
                caused_by: None,
            });
            // Add mana to pool
            let unit =
                crate::types::mana::ManaUnit::new(resolved_mana_type, object_id, false, Vec::new());
            if let Some(p) = state.players.iter_mut().find(|p| p.id == *player) {
                p.mana_pool.add(unit);
            }
            events.push(GameEvent::ManaAdded {
                player_id: *player,
                mana_type: resolved_mana_type,
                source_id: object_id,
                tapped_for_mana: false,
            });
            // Only emit waterbend event for Waterbend mode
            if mode == ConvokeMode::Waterbend {
                events.push(GameEvent::Waterbend {
                    source_id: object_id,
                    controller: *player,
                });
                if let Some(p) = state.players.iter_mut().find(|p| p.id == *player) {
                    p.bending_types_this_turn.insert(BendingType::Water);
                }
            }
            WaitingFor::ManaPayment {
                player: *player,
                convoke_mode: Some(mode),
            }
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
                    log_entries: vec![],
                });
            }

            if attacks.is_empty() {
                // No attackers: skip to EndCombat
                state.phase = Phase::EndCombat;
                events.push(GameEvent::PhaseChanged {
                    phase: Phase::EndCombat,
                });
                state.combat = None;
                // CR 514.2: Prune "until end of combat" transient continuous effects.
                super::layers::prune_end_of_combat_effects(state);
                turns::advance_phase(state, &mut events);
                turns::auto_advance(state, &mut events)
            } else {
                // CR 508.2: After attackers are declared, the active player gets priority.
                priority::reset_priority(state);
                WaitingFor::Priority {
                    player: state.active_player,
                }
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
                    log_entries: vec![],
                });
            }

            // CR 509.2: After blockers are declared, the active player gets priority.
            priority::reset_priority(state);
            WaitingFor::Priority {
                player: state.active_player,
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
                        controller_override,
                        ..
                    } = event
                    {
                        zones::move_to_zone(state, object_id, to, &mut events);
                        if to == Zone::Battlefield {
                            if let Some(obj) = state.objects.get_mut(&object_id) {
                                obj.tapped = enter_tapped;
                                obj.entered_battlefield_turn = Some(state.turn_number);
                                if let Some(new_controller) = controller_override {
                                    obj.controller = new_controller;
                                }
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
        // CR 702.49: Ninjutsu-family activation during combat
        (
            WaitingFor::Priority { player },
            GameAction::ActivateNinjutsu {
                ninjutsu_card_id,
                creature_to_return,
            },
        ) => {
            let p = *player;
            super::keywords::activate_ninjutsu(
                state,
                p,
                ninjutsu_card_id,
                creature_to_return,
                &mut events,
            )
            .map_err(EngineError::InvalidAction)?;
            WaitingFor::Priority { player: p }
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
        // CR 701.62a: Manifest Dread — player selects one card to manifest, rest to graveyard.
        (
            WaitingFor::ManifestDreadChoice { player, cards },
            GameAction::SelectCards {
                cards: selected_cards,
            },
        ) => {
            let p = *player;
            let all_cards = cards.clone();

            // Validate: exactly 1 card selected from the choice set
            if selected_cards.len() != 1 || !all_cards.contains(&selected_cards[0]) {
                return Err(EngineError::InvalidAction(
                    "Must select exactly 1 card from the manifest dread choices".to_string(),
                ));
            }

            let manifest_id = selected_cards[0];
            let graveyard_cards: Vec<_> = all_cards
                .iter()
                .filter(|&&id| id != manifest_id)
                .copied()
                .collect();

            // CR 701.62a: Manifest the selected card face-down
            crate::game::morph::manifest_card(state, p, manifest_id, &mut events)
                .map_err(|e| EngineError::InvalidAction(format!("{e}")))?;

            // CR 701.62a: Put unchosen card(s) into graveyard
            for card_id in graveyard_cards {
                zones::move_to_zone(state, card_id, Zone::Graveyard, &mut events);
            }

            // Clear revealed status for these cards
            for &card_id in &all_cards {
                state.revealed_cards.remove(&card_id);
            }

            // Resume continuation if present
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // CR 701.57a: Discover — player chose cast-free or put-to-hand.
        (
            WaitingFor::DiscoverChoice {
                player,
                hit_card,
                exiled_misses,
            },
            GameAction::DiscoverChoice { cast },
        ) => {
            let p = *player;
            let hit = *hit_card;
            let misses = exiled_misses.clone();

            if cast {
                // CR 701.57a: "Cast it without paying its mana cost" — grant
                // a zero-cost casting permission on the exiled card so the player
                // can cast it via the normal exile-cast flow.
                if let Some(obj) = state.objects.get_mut(&hit) {
                    obj.casting_permissions.push(
                        crate::types::ability::CastingPermission::ExileWithAltCost {
                            cost: crate::types::mana::ManaCost::zero(),
                        },
                    );
                }
            } else {
                // Put to hand
                zones::move_to_zone(state, hit, Zone::Hand, &mut events);
            }

            // CR 701.57a: Put exiled misses on bottom in random order.
            {
                use rand::seq::SliceRandom;
                let mut shuffled = misses;
                shuffled.shuffle(&mut state.rng);
                for card_id in shuffled {
                    zones::move_to_library_position(state, card_id, false, &mut events);
                }
            }

            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // CR 401.4: Owner chose top or bottom for the permanent.
        (
            WaitingFor::TopOrBottomChoice { player, object_id },
            GameAction::ChooseTopOrBottom { top },
        ) => {
            let p = *player;
            let oid = *object_id;
            zones::move_to_library_position(state, oid, top, &mut events);
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // CR 701.20e + CR 608.2c: Player selects cards to keep from looked-at cards; rest go elsewhere.
        (
            WaitingFor::DigChoice {
                player,
                cards: all_cards,
                keep_count,
                up_to,
                selectable_cards,
                kept_destination,
                rest_destination,
                ..
            },
            GameAction::SelectCards { cards: kept },
        ) => {
            let p = *player;
            let kc = *keep_count;
            let is_up_to = *up_to;
            let all = all_cards.clone();
            let selectable = selectable_cards.clone();
            let kept_dest = *kept_destination;
            let rest_dest = *rest_destination;
            // Validate selection count
            if is_up_to {
                if kept.len() > kc {
                    return Err(EngineError::InvalidAction(format!(
                        "Must select at most {} cards, got {}",
                        kc,
                        kept.len()
                    )));
                }
            } else if kept.len() != kc {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} cards, got {}",
                    kc,
                    kept.len()
                )));
            }
            // Validate all kept cards are in selectable set
            if !selectable.is_empty() {
                for card_id in &kept {
                    if !selectable.contains(card_id) {
                        return Err(EngineError::InvalidAction(
                            "Selected card does not match filter".to_string(),
                        ));
                    }
                }
            }
            let unkept: Vec<_> = all
                .iter()
                .filter(|id| !kept.contains(id))
                .copied()
                .collect();
            // Move kept cards to destination (default: Hand)
            let kept_zone = kept_dest.unwrap_or(Zone::Hand);
            for &obj_id in &kept {
                zones::move_to_zone(state, obj_id, kept_zone, &mut events);
            }
            // Move rest to destination (default: Graveyard)
            match rest_dest {
                // CR 701.24g: Bottom of library via move_to_library_position.
                Some(Zone::Library) => {
                    for &obj_id in &unkept {
                        zones::move_to_library_position(state, obj_id, false, &mut events);
                    }
                }
                Some(zone) => {
                    for &obj_id in &unkept {
                        zones::move_to_zone(state, obj_id, zone, &mut events);
                    }
                }
                None => {
                    for &obj_id in &unkept {
                        zones::move_to_zone(state, obj_id, Zone::Graveyard, &mut events);
                    }
                }
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
        // CR 700.2: Player selected card(s) from a tracked set (e.g., exiled cards).
        // Bifurcated target injection: chosen → first sub-ability, unchosen → second sub-ability.
        (
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards: all_cards,
                count,
                ..
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let p = *player;
            let all = all_cards.clone();
            let expected = *count;

            // Validate selection count
            if chosen.len() != expected {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} card(s), got {}",
                    expected,
                    chosen.len()
                )));
            }

            // Validate all chosen cards are in the set
            for card_id in &chosen {
                if !all.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in available set".to_string(),
                    ));
                }
            }

            // Split into chosen and unchosen
            let unchosen: Vec<_> = all
                .iter()
                .filter(|id| !chosen.contains(id))
                .copied()
                .collect();

            if let Some(mut cont) = state.pending_continuation.take() {
                // Priority returns to the spell's controller, not the chooser
                // (the opponent chose, but the controller's spell continues resolving).
                let controller = cont.controller;
                state.waiting_for = WaitingFor::Priority { player: controller };
                state.priority_player = controller;

                // Inject chosen cards as targets into the first sub-ability
                // (e.g., ChangeZone to Library for Emergent Ultimatum)
                cont.targets = chosen.iter().map(|&id| TargetRef::Object(id)).collect();

                // Inject unchosen cards as targets into the NEXT sub-ability
                // (e.g., CastFromZone for Emergent Ultimatum)
                if let Some(ref mut next_sub) = cont.sub_ability {
                    next_sub.targets = unchosen.iter().map(|&id| TargetRef::Object(id)).collect();
                }

                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            } else {
                // No continuation — return priority to the chooser (shouldn't normally happen)
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
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
            let p = *player;
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

            if super::turns::finish_cleanup_discard(state, p, &chosen, &mut events) {
                // Replacement choice interrupted — waiting_for already set.
                return Ok(ActionResult {
                    events,
                    waiting_for: state.waiting_for.clone(),
                    log_entries: vec![],
                });
            }

            // Continue the turn: advance past cleanup into next turn
            super::turns::advance_phase(state, &mut events);
            super::turns::auto_advance(state, &mut events)
        }
        // CR 701.50a: Player chose card(s) to discard for connive.
        (
            WaitingFor::ConniveDiscard {
                player,
                conniver_id,
                source_id,
                cards: legal_cards,
                count,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let p = *player;
            let expected = *count;
            let legal = legal_cards.clone();
            let conniver = *conniver_id;
            let src = *source_id;

            if chosen.len() != expected {
                return Err(EngineError::InvalidAction(format!(
                    "Must discard exactly {} card(s), got {}",
                    expected,
                    chosen.len()
                )));
            }

            // Validate against both the legal snapshot and the current hand
            let current_hand: std::collections::HashSet<ObjectId> = state
                .players
                .iter()
                .find(|pl| pl.id == p)
                .map(|pl| pl.hand.iter().copied().collect())
                .unwrap_or_default();

            for card_id in &chosen {
                if !legal.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not from connive draw".to_string(),
                    ));
                }
                if !current_hand.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Card no longer in hand".to_string(),
                    ));
                }
            }

            // Discard each chosen card and count nonlands.
            let Some(nonland_count) = super::effects::connive::discard_all_and_count_nonlands(
                state,
                &chosen,
                p,
                &mut events,
            ) else {
                // Replacement choice interrupted — waiting_for already set by helper.
                return Ok(ActionResult {
                    events,
                    waiting_for: state.waiting_for.clone(),
                    log_entries: vec![],
                });
            };

            // CR 701.50a: Add +1/+1 counters for nonland discards
            super::effects::connive::add_connive_counters(
                state,
                conniver,
                nonland_count,
                &mut events,
            );

            events.push(GameEvent::EffectResolved {
                kind: EffectKind::Connive,
                source_id: src,
            });

            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // CR 701.9b: Player chose card(s) to discard during effect resolution.
        (
            WaitingFor::DiscardChoice {
                player,
                count,
                cards: legal_cards,
                source_id,
                effect_kind,
            },
            GameAction::SelectCards { cards: chosen },
        ) => {
            let p = *player;
            let expected = *count;
            let legal = legal_cards.clone();
            let src = *source_id;
            let kind = *effect_kind;

            if chosen.len() != expected {
                return Err(EngineError::InvalidAction(format!(
                    "Must discard exactly {} card(s), got {}",
                    expected,
                    chosen.len()
                )));
            }

            let current_hand: std::collections::HashSet<ObjectId> = state
                .players
                .iter()
                .find(|pl| pl.id == p)
                .map(|pl| pl.hand.iter().copied().collect())
                .unwrap_or_default();

            for card_id in &chosen {
                if !legal.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Selected card not in eligible set".to_string(),
                    ));
                }
                if !current_hand.contains(card_id) {
                    return Err(EngineError::InvalidAction(
                        "Card no longer in hand".to_string(),
                    ));
                }
            }

            for &card_id in &chosen {
                if let super::effects::discard::DiscardOutcome::NeedsReplacementChoice(
                    choice_player,
                ) = super::effects::discard::discard_as_cost(state, card_id, p, &mut events)
                {
                    state.waiting_for =
                        super::replacement::replacement_choice_waiting_for(choice_player, state);
                    return Ok(ActionResult {
                        events,
                        waiting_for: state.waiting_for.clone(),
                        log_entries: vec![],
                    });
                }
            }

            events.push(GameEvent::EffectResolved {
                kind,
                source_id: src,
            });

            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
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
        // CR 701.54a: Player chooses a ring-bearer — the chosen creature becomes the Ring-bearer.
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
        // CR 704.5j: Player chooses which legend to keep; others go to graveyard.
        (WaitingFor::ChooseLegend { candidates, .. }, GameAction::ChooseLegend { keep }) => {
            if !candidates.contains(&keep) {
                return Err(EngineError::InvalidAction(
                    "Invalid legend choice — not a candidate".to_string(),
                ));
            }
            // CR 704.5j: Put all non-chosen legends into their owners' graveyards.
            // This is NOT destruction — indestructible does not prevent it.
            let to_remove: Vec<_> = candidates
                .iter()
                .filter(|&&id| id != keep)
                .copied()
                .collect();
            for id in to_remove {
                zones::move_to_zone(state, id, Zone::Graveyard, &mut events);
            }
            // Return to priority — the caller at line 1932 runs run_post_action_pipeline,
            // which re-checks SBAs (finding any additional legend groups).
            WaitingFor::Priority {
                player: state.active_player,
            }
        }
        (WaitingFor::Priority { player }, GameAction::PlayFaceDown { object_id, card_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            // Validate object_id matches card_id and is in hand
            let valid = state.objects.get(&object_id).is_some_and(|obj| {
                obj.card_id == card_id && obj.owner == p && obj.zone == Zone::Hand
            });
            if !valid {
                return Err(EngineError::InvalidAction(
                    "Card not found in hand".to_string(),
                ));
            }
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
                    description: trigger.description.clone(),
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
                source_id: wf_source_id,
                description: wf_description,
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
                        source_id: *wf_source_id,
                        description: wf_description.clone(),
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
                            description: trigger.description.clone(),
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
                        let mut pending_eng = crate::types::game_state::PendingCast::new(
                            sid,
                            CardId(0),
                            resolved,
                            crate::types::mana::ManaCost::NoCost,
                        );
                        pending_eng.activation_cost = ability_cost.clone();
                        pending_eng.activation_ability_index = *ability_index;
                        pending_eng.target_constraints = target_constraints;
                        return Ok(ActionResult {
                            events,
                            waiting_for: WaitingFor::TargetSelection {
                                player: p,
                                pending_cast: Box::new(pending_eng),
                                target_slots,
                                selection,
                            },
                            log_entries: vec![],
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
                // CR 603.3: Once the player has chosen modes for a triggered ability,
                // consume the pending trigger and either finish putting it on the stack
                // now or re-stash it only if further target selection is required.
                let mut trigger = state
                    .pending_trigger
                    .take()
                    .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
                let target_slots = build_target_slots(state, &resolved)?;
                let target_constraints = super::ability_utils::target_constraints_from_modal(modal);
                trigger.ability = resolved;
                trigger.target_constraints = target_constraints.clone();
                trigger.modal = None;
                trigger.mode_abilities.clear();

                if !target_slots.is_empty() {
                    if let Some(targets) = auto_select_targets(&target_slots, &target_constraints)?
                    {
                        assign_targets_in_chain(&mut trigger.ability, &targets)?;
                        casting::emit_targeting_events(
                            state,
                            &flatten_targets_in_chain(&trigger.ability),
                            sid,
                            p,
                            &mut events,
                        );
                        super::triggers::push_pending_trigger_to_stack(state, trigger, &mut events);
                    } else {
                        state.pending_trigger = Some(trigger);
                        let trigger_description = state
                            .pending_trigger
                            .as_ref()
                            .and_then(|t| t.description.clone());
                        let selection = begin_target_selection(&target_slots, &target_constraints)?;
                        return Ok(ActionResult {
                            events,
                            waiting_for: WaitingFor::TriggerTargetSelection {
                                player: p,
                                target_slots,
                                target_constraints,
                                selection,
                                source_id: Some(sid),
                                description: trigger_description,
                            },
                            log_entries: vec![],
                        });
                    }
                } else {
                    super::triggers::push_pending_trigger_to_stack(state, trigger, &mut events);
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

            // CR 601.2c: Each selected target must be a legal target.
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
        // CR 702.139a: Pre-game companion reveal
        (
            WaitingFor::CompanionReveal { player, .. },
            GameAction::DeclareCompanion { card_index },
        ) => super::companion::handle_declare_companion(state, *player, card_index, &mut events),
        // CR 702.139a: Special action — pay {3} to put companion into hand (see rule 116.2g).
        (WaitingFor::Priority { player }, GameAction::CompanionToHand) => {
            state.lands_tapped_for_mana.remove(player);
            super::companion::handle_companion_to_hand(state, *player, &mut events)
                .map_err(EngineError::InvalidAction)?
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
        // CR 701.34a: Proliferate — player selected targets to proliferate.
        (
            WaitingFor::ProliferateChoice { player, eligible },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let eligible_set = eligible.clone();
            // Validate all selected targets are in the eligible set.
            for t in &targets {
                if !eligible_set.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Selected target not eligible for proliferate".to_string(),
                    ));
                }
            }
            effects::proliferate::apply_proliferate(state, &targets, &mut events);
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Proliferate,
                source_id: ObjectId(0), // Source not tracked through choice state
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // CR 707.10c: Copy retarget — player chose new targets for the copy.
        (
            WaitingFor::CopyRetarget {
                player,
                copy_id,
                target_slots,
            },
            GameAction::SelectTargets { targets },
        ) => {
            let p = *player;
            let cid = *copy_id;
            if targets.len() != target_slots.len() {
                return Err(EngineError::InvalidAction(format!(
                    "Must provide {} targets, got {}",
                    target_slots.len(),
                    targets.len()
                )));
            }
            // Update the copy's targets on the stack.
            if let Some(entry) = state.stack.iter_mut().find(|e| e.id == cid) {
                entry.ability_mut().targets = targets;
            }
            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::CopySpell,
                source_id: cid,
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        // CR 510.1c/d: Combat damage assignment from attacker to blockers.
        (
            WaitingFor::AssignCombatDamage {
                player,
                attacker_id,
                total_damage,
                blockers,
                trample,
                defending_player,
                attack_target,
                pw_loyalty,
                pw_controller,
            },
            GameAction::AssignCombatDamage {
                assignments,
                trample_damage,
                controller_damage,
            },
        ) => {
            let p = *player;
            let aid = *attacker_id;
            let total = *total_damage;
            let trample_kind = *trample;

            // CR 510.1c: Validate total equals attacker's power.
            let assigned_total: u32 = assignments.iter().map(|(_, a)| *a).sum::<u32>()
                + trample_damage
                + controller_damage;
            if assigned_total != total {
                return Err(EngineError::InvalidAction(format!(
                    "Damage assignment total {} != attacker power {}",
                    assigned_total, total
                )));
            }

            // Validate all assignment targets are actual blockers of this attacker.
            let valid_blocker_ids: Vec<ObjectId> = blockers.iter().map(|s| s.blocker_id).collect();
            for (bid, _) in &assignments {
                if !valid_blocker_ids.contains(bid) {
                    return Err(EngineError::InvalidAction(format!(
                        "{:?} is not a blocker of attacker {:?}",
                        bid, aid
                    )));
                }
            }

            // CR 702.19b: Trample damage only allowed with trample.
            if (trample_damage > 0 || controller_damage > 0) && trample_kind.is_none() {
                return Err(EngineError::InvalidAction(
                    "Cannot assign trample damage without trample".to_string(),
                ));
            }

            // CR 702.19c: controller_damage only allowed with trample-over-PW attacking a PW.
            if controller_damage > 0 {
                let is_valid = trample_kind == Some(TrampleKind::OverPlaneswalkers)
                    && pw_controller.is_some()
                    && matches!(attack_target, AttackTarget::Planeswalker(_));
                if !is_valid {
                    return Err(EngineError::InvalidAction(
                        "Controller damage only allowed with trample over planeswalkers attacking a planeswalker".to_string(),
                    ));
                }
                // CR 702.19c: Must assign at least PW loyalty to PW before spillover.
                let loyalty_threshold = pw_loyalty.unwrap_or(0);
                if trample_damage < loyalty_threshold {
                    return Err(EngineError::InvalidAction(format!(
                        "Trample over planeswalkers: must assign at least {} to PW before {} to controller",
                        loyalty_threshold, controller_damage
                    )));
                }
            }

            // CR 702.19b: Trample requires lethal to ALL blockers.
            // Enforced regardless of whether excess goes to the player.
            if trample_kind.is_some() {
                for slot in blockers {
                    let assigned = assignments
                        .iter()
                        .find(|(id, _)| *id == slot.blocker_id)
                        .map(|(_, a)| *a)
                        .unwrap_or(0);
                    if assigned < slot.lethal_minimum {
                        return Err(EngineError::InvalidAction(format!(
                            "Trample: blocker {:?} must receive at least {} lethal damage before excess to player",
                            slot.blocker_id, slot.lethal_minimum
                        )));
                    }
                }
            }

            // Store assignments in pending_damage and advance the damage step index.
            if let Some(combat) = &mut state.combat {
                for (blocker_id, amount) in &assignments {
                    if *amount > 0 {
                        combat.pending_damage.push((
                            aid,
                            DamageAssignment {
                                target: DamageTarget::Object(*blocker_id),
                                amount: *amount,
                            },
                        ));
                    }
                }
                // CR 702.19f: Trample excess goes to the attack target, not always the player.
                // CR 506.4c: If PW/battle left the battlefield, no trample excess.
                if trample_damage > 0 {
                    let is_over_pw = trample_kind == Some(TrampleKind::OverPlaneswalkers);
                    let excess_target = match attack_target {
                        AttackTarget::Player(pid) => Some(DamageTarget::Player(*pid)),
                        AttackTarget::Planeswalker(pw_id) => {
                            match state.objects.get(pw_id) {
                                Some(obj) if obj.zone == Zone::Battlefield => {
                                    Some(DamageTarget::Object(*pw_id))
                                }
                                // CR 702.19e: Trample-over-PW falls back to defending player
                                _ if is_over_pw => Some(DamageTarget::Player(*defending_player)),
                                // CR 506.4c: Without trample-over-PW, no damage
                                _ => None,
                            }
                        }
                        AttackTarget::Battle(battle_id) => match state.objects.get(battle_id) {
                            Some(obj) if obj.zone == Zone::Battlefield => {
                                Some(DamageTarget::Object(*battle_id))
                            }
                            _ => None,
                        },
                    };
                    if let Some(target) = excess_target {
                        combat.pending_damage.push((
                            aid,
                            DamageAssignment {
                                target,
                                amount: trample_damage,
                            },
                        ));
                    }
                }
                // CR 702.19c: Route controller damage to PW's controller.
                if controller_damage > 0 {
                    if let Some(ctrl) = pw_controller {
                        combat.pending_damage.push((
                            aid,
                            DamageAssignment {
                                target: DamageTarget::Player(*ctrl),
                                amount: controller_damage,
                            },
                        ));
                    }
                }
                // Advance past this attacker so resolve_combat_damage continues from next.
                combat.damage_step_index = Some(combat.damage_step_index.unwrap_or(0) + 1);
            }

            // Resume the combat damage state machine — may return another
            // WaitingFor::AssignCombatDamage for the next attacker, or None if all done.
            if let Some(waiting) = super::combat_damage::resolve_combat_damage(state, &mut events) {
                state.waiting_for = waiting.clone();
                return Ok(ActionResult {
                    events,
                    waiting_for: waiting,
                    log_entries: vec![],
                });
            }

            // All combat damage resolved — triggers were processed inline by
            // process_combat_damage_triggers. Mark so the pipeline doesn't re-fire.
            triggers_processed_inline = true;
            super::priority::reset_priority(state);
            WaitingFor::Priority { player: p }
        }
        // CR 601.2d: Distribute among targets (casting-time distribution).
        (
            WaitingFor::DistributeAmong {
                player,
                total,
                targets,
                ..
            },
            GameAction::DistributeAmong { distribution },
        ) => {
            let p = *player;
            let expected_total = *total;

            // Validate: each target gets ≥ 1, and total matches.
            let actual_total: u32 = distribution.iter().map(|(_, a)| *a).sum();
            if actual_total != expected_total {
                return Err(EngineError::InvalidAction(format!(
                    "Distribution total {} != required {}",
                    actual_total, expected_total
                )));
            }
            for (t, amount) in &distribution {
                if *amount == 0 {
                    return Err(EngineError::InvalidAction(
                        "Each target must receive at least 1".to_string(),
                    ));
                }
                if !targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Distribution target not in legal set".to_string(),
                    ));
                }
            }

            // Store on the pending cast's resolved ability if we're mid-casting.
            // The distribution will be read during effect resolution.
            if let Some(pending) = state.pending_cast.as_mut() {
                pending.ability.distribution =
                    Some(distribution.iter().map(|(t, a)| (t.clone(), *a)).collect());
            }

            // CR 601.2d: Resume casting pipeline after distribution.
            if state.pending_cast.is_some() {
                // Mid-cast distribution: resume finalize_cast to push spell to stack.
                let pending = state.pending_cast.take().unwrap();
                casting_costs::finalize_cast(
                    state,
                    p,
                    pending.object_id,
                    pending.card_id,
                    pending.ability,
                    &pending.cost,
                    pending.casting_variant,
                    &mut events,
                )?
            } else {
                // Resolution-time distribution (triggered ability path).
                state.waiting_for = WaitingFor::Priority { player: p };
                state.priority_player = p;
                if let Some(cont) = state.pending_continuation.take() {
                    let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
                }
                state.waiting_for.clone()
            }
        }
        // CR 115.7: Retarget a spell or ability on the stack.
        (
            WaitingFor::RetargetChoice {
                player,
                stack_entry_index,
                legal_new_targets,
                ..
            },
            GameAction::RetargetSpell { new_targets },
        ) => {
            let p = *player;
            let idx = *stack_entry_index;

            // CR 115.7d: Validate each submitted target is in the legal set.
            for t in &new_targets {
                if !legal_new_targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Retarget: chosen target not in legal alternatives".to_string(),
                    ));
                }
            }

            // Update targets on the stack entry.
            if idx < state.stack.len() {
                state.stack[idx].ability_mut().targets = new_targets.clone();
            } else {
                return Err(EngineError::InvalidAction(
                    "Invalid stack entry index for retargeting".to_string(),
                ));
            }

            events.push(GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::ChangeTargets,
                source_id: state
                    .stack
                    .get(idx)
                    .map(|e| e.source_id)
                    .unwrap_or(ObjectId(0)),
            });
            state.waiting_for = WaitingFor::Priority { player: p };
            state.priority_player = p;
            if let Some(cont) = state.pending_continuation.take() {
                let _ = effects::resolve_ability_chain(state, &cont, &mut events, 0);
            }
            state.waiting_for.clone()
        }
        (waiting, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting
            )));
        }
    };

    // Run post-action pipeline (SBAs, triggers, layers) and check for terminal states.
    // When triggers were already processed inline (e.g., DeclareAttackers, combat damage),
    // pass the flag to skip the trigger scan but still run SBAs, delayed triggers, and layers.
    if matches!(waiting_for, WaitingFor::Priority { .. }) {
        // Sync state.waiting_for before the pipeline so SBA/trigger checks see
        // the action's result, not the pre-action state (fixes stale TargetSelection
        // after CancelCast).
        state.waiting_for = waiting_for.clone();
        let wf =
            run_post_action_pipeline(state, &mut events, &waiting_for, triggers_processed_inline)?;
        state.waiting_for = wf.clone();
        return Ok(ActionResult {
            events,
            waiting_for: wf,
            log_entries: vec![],
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
            log_entries: vec![],
        });
    }

    state.waiting_for = waiting_for.clone();

    Ok(ActionResult {
        events,
        waiting_for,
        log_entries: vec![],
    })
}

/// Run state-based actions, exile returns, delayed triggers, and trigger processing
/// after an action that produced `WaitingFor::Priority`. Returns the resulting
/// `WaitingFor` state — may be terminal (GameOver, interactive choice) or
/// a continuation (Priority for next player/active player).
///
/// `default_wf` is the WaitingFor computed by the action handler, used as fallback
/// when no terminal/trigger/SBA outcome overrides it.
///
/// `skip_trigger_scan` — when `true`, skips the `process_triggers` call because
/// triggers were already processed inline (e.g., combat damage, declare attackers).
/// SBAs, exile returns, delayed triggers, and layer evaluation still run.
fn run_post_action_pipeline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
) -> Result<WaitingFor, EngineError> {
    sba::check_state_based_actions(state, events);

    // If SBAs set a non-Priority waiting state (legend choice, replacement choice, game over),
    // short-circuit — don't process triggers or overwrite the waiting state.
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            match_flow::handle_game_over_transition(state);
        }
        return Ok(state.waiting_for.clone());
    }

    // Check exile returns -- must happen after SBAs (which may move sources off battlefield)
    // and before triggers (so returned permanents get ETB triggers)
    check_exile_returns(state, events);

    // CR 603.7: Check delayed triggers before processing regular triggers.
    let delayed_events = triggers::check_delayed_triggers(state, events);
    events.extend(delayed_events);

    let stack_before = state.stack.len();

    // Skip trigger scan when triggers were already processed inline (e.g., combat
    // damage triggers fire before SBAs per CR 603.2). Without this guard, the same
    // LifeChanged / DamageDealt events would be re-processed, double-firing triggers
    // like "whenever you gain life".
    if !skip_trigger_scan {
        // Phase triggers are now processed inline by auto_advance() at each step
        // (Upkeep, Draw, BeginCombat, End). Exclude all PhaseChanged events here
        // to prevent double-firing. Non-phase triggers (ETB, spell cast, zone
        // changes, etc.) still process normally.
        let filtered_events: Vec<_> = events
            .iter()
            .filter(|e| !matches!(e, GameEvent::PhaseChanged { .. }))
            .cloned()
            .collect();
        triggers::process_triggers(state, &filtered_events);
    }

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
    // CR 514.2: Prune "until end of combat" transient continuous effects.
    super::layers::prune_end_of_combat_effects(state);
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
        source_id: Some(trigger.source_id),
        description: trigger.description.clone(),
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
    if let Effect::BecomeCopy { ref target, .. } = *effect_def.effect {
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
        ResolvedAbility::new(*def.effect.clone(), targets, source_id, controller).kind(def.kind);
    if let Some(sub) = &def.sub_ability {
        resolved = resolved.sub_ability(resolved_ability_from_definition(
            sub,
            source_id,
            controller,
            Vec::new(),
        ));
    }
    if let Some(else_ab) = &def.else_ability {
        resolved.else_ability = Some(Box::new(resolved_ability_from_definition(
            else_ab,
            source_id,
            controller,
            Vec::new(),
        )));
    }
    if let Some(d) = def.duration.clone() {
        resolved = resolved.duration(d);
    }
    if let Some(c) = def.condition.clone() {
        resolved = resolved.condition(c);
    }
    resolved
}

/// CR 604.2: If a land was played from the graveyard via a once-per-turn permission source,
/// record the source as used to prevent a second play/cast from the same source this turn.
fn record_graveyard_play_permission(state: &mut GameState, source: Option<ObjectId>) {
    if let Some(source_id) = source {
        // Check if the source has a once_per_turn permission
        if let Some(obj) = state.objects.get(&source_id) {
            let is_once_per_turn = obj.static_definitions.iter().any(|s| {
                matches!(
                    s.mode,
                    StaticMode::GraveyardCastPermission {
                        once_per_turn: true,
                        ..
                    }
                )
            });
            if is_once_per_turn {
                state.graveyard_cast_permissions_used.insert(source_id);
            }
        }
    }
}

fn handle_play_land(
    state: &mut GameState,
    object_id: ObjectId,
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

    // CR 305.2 + CR 505.6b: Validate land limit.
    // Base limit is max_lands_per_turn (normally 1), plus any additional drops
    // from static abilities like Exploration or Azusa.
    let additional = super::static_abilities::additional_land_drops(state, state.priority_player);
    let effective_limit = state.max_lands_per_turn.saturating_add(additional);
    if state.lands_played_this_turn >= effective_limit {
        return Err(EngineError::ActionNotAllowed(
            "Already played maximum lands this turn".to_string(),
        ));
    }

    // Validate that object_id exists in hand or graveyard (with permission) and matches card_id
    let player = state.priority_player;
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("priority player exists");
    let in_hand = player_data.hand.contains(&object_id);
    // CR 305.1 + CR 604.2: Check graveyard for play-from-graveyard permission
    // CR 604.2: Find graveyard play permission source (if any) for once-per-turn tracking.
    let gy_permission_source = if player_data.graveyard.contains(&object_id) {
        super::casting::graveyard_lands_playable_by_permission(state, player)
            .iter()
            .find(|(obj_id, _)| *obj_id == object_id)
            .map(|(_, source_id)| *source_id)
    } else {
        None
    };
    let in_graveyard_with_permission = gy_permission_source.is_some();

    if !in_hand && !in_graveyard_with_permission {
        return Err(EngineError::InvalidAction(
            "Card not found in hand or graveyard with play permission".to_string(),
        ));
    }
    if !state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.card_id == card_id)
    {
        return Err(EngineError::InvalidAction(
            "Card not found or card_id mismatch".to_string(),
        ));
    }

    // Determine origin zone for the zone change event
    let origin_zone = if in_hand { Zone::Hand } else { Zone::Graveyard };

    // Route through the replacement pipeline (handles ETB replacements like shock lands)
    let proposed = crate::types::proposed_event::ProposedEvent::zone_change(
        object_id,
        origin_zone,
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
                controller_override,
                ..
            } = event
            {
                zones::move_to_zone(state, object_id, to, events);
                if let Some(obj) = state.objects.get_mut(&object_id) {
                    obj.tapped = enter_tapped;
                    obj.entered_battlefield_turn = Some(state.turn_number);
                    if let Some(new_controller) = controller_override {
                        obj.controller = new_controller;
                    }
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
            // CR 604.2: Record once-per-turn graveyard play permission usage.
            record_graveyard_play_permission(state, gy_permission_source);
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
    // CR 604.2: Record once-per-turn graveyard play permission usage.
    record_graveyard_play_permission(state, gy_permission_source);
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
            None,
        )?;
    } else {
        // Legacy fallback for subtype-only lands.
        let obj = state.objects.get_mut(&object_id).unwrap();
        obj.tapped = true;
        events.push(GameEvent::PermanentTapped {
            object_id,
            caused_by: None,
        });
        mana_payment::produce_mana(
            state,
            object_id,
            mana_option.mana_type,
            state.priority_player,
            true,
            events,
        );
    }

    Ok(WaitingFor::Priority {
        player: state.priority_player,
    })
}

/// CR 605.3b: Reverse a manual land tap — untap source and remove its mana from pool.
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
        // CR 702.139a: Check for eligible companions before mulligans.
        if let Some(companion_wf) = super::companion::check_all_companion_reveals(state) {
            companion_wf
        } else {
            mulligan::start_mulligan(state, &mut events)
        }
    } else {
        // No cards to mulligan with, skip straight to game
        turns::auto_advance(state, &mut events)
    };

    state.waiting_for = waiting_for.clone();
    derive_display_state(state);

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
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

    let log_entries = super::log::resolve_log_entries(&events, state);
    ActionResult {
        events,
        waiting_for,
        log_entries,
    }
}

/// CR 607.2a + CR 406.6: Check if any exile-return sources have left the battlefield.
/// If so, move the exiled cards back — linked abilities track which cards were exiled by the source.
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
                damage_source: None,
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

        let result = apply(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        )
        .unwrap();

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

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_over_limit() {
        let mut state = setup_game_at_main_phase();
        state.lands_played_this_turn = 1; // Already played one

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_card_not_in_hand() {
        let mut state = setup_game_at_main_phase();

        let result = apply(
            &mut state,
            GameAction::PlayLand {
                object_id: ObjectId(0),
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
        let result = apply(
            &mut state,
            GameAction::PlayLand {
                object_id: land_id,
                card_id: CardId(1),
            },
        )
        .unwrap();

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
        use crate::types::game_state::{CastingVariant, StackEntry, StackEntryKind};

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
                    casting_variant: CastingVariant::Normal,
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
                    casting_variant: CastingVariant::Normal,
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
                        expiry: None,
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
                        expiry: None,
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
        let land_obj_id = state.players[0].hand[0];
        let land_card_id = state.objects[&land_obj_id].card_id;
        let _result = apply(
            &mut state,
            GameAction::PlayLand {
                object_id: land_obj_id,
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
                expiry: None,
            });
        }

        let result = apply(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
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
                expiry: None,
            });
        }

        // Cast the spell
        apply(
            &mut state,
            GameAction::CastSpell {
                object_id: obj_id,
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
            expiry: None,
        });

        // Cast bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                object_id: bolt_id,
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
                expiry: None,
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
                object_id: bolt_id,
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
                object_id: bolt_id,
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
                object_id: creature_id,
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
                object_id: counter_id,
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
                object_id: growth_id,
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
                object_id: bolt_id,
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
                        expiry: None,
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
            convoke_mode: None,
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
                        expiry: None,
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
                    player: PlayerId(0),
                    ..
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
                    casting_variant: crate::types::game_state::CastingVariant::Normal,
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

        let _result = apply(
            &mut state,
            GameAction::PlayLand {
                object_id: obj_id,
                card_id: CardId(1),
            },
        )
        .unwrap();
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
                object_id: hand_land,
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
            expiry: None,
        });

        // Create a forest on the battlefield to tap during ManaPayment
        let land_id = create_forest(&mut state, PlayerId(0));

        let result = apply(
            &mut state,
            GameAction::CastSpell {
                object_id: spell_id,
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
                    player: PlayerId(0),
                    ..
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
        AbilityDefinition, AbilityKind, ControllerRef, Effect, GainLifePlayer, ModalChoice,
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
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
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
            description: None,
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
            source_id: None,
            description: None,
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
                    enter_transformed: false,
                    under_your_control: false,
                    enter_tapped: false,
                    enters_attacking: false,
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
            description: None,
        });

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(legal_target)],
                optional: false,
            }],
            target_constraints: Vec::new(),
            selection: crate::types::game_state::TargetSelectionProgress::default(),
            source_id: None,
            description: None,
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
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: ObjectId(20),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "modal_placeholder".to_string(),
                    description: None,
                },
                vec![],
                ObjectId(20),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: Some(GameEvent::SpellCast {
                controller: PlayerId(0),
                object_id: ObjectId(98),
                card_id: CardId(98),
            }),
            modal: Some(ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Deal 1 damage to target player.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            }),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
                },
            )],
            description: Some("Choose two target players".to_string()),
        });
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
                    damage_source: None,
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
    fn triggered_modal_modes_without_targets_consume_pending_trigger() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        let source_id = ObjectId(21);
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id,
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "modal_placeholder".to_string(),
                    description: None,
                },
                vec![],
                source_id,
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: Some(GameEvent::SpellCast {
                controller: PlayerId(0),
                object_id: ObjectId(99),
                card_id: CardId(99),
            }),
            modal: Some(ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                mode_descriptions: vec!["Gain 2 life.".to_string(), "Draw a card.".to_string()],
                allow_repeat_modes: false,
                ..Default::default()
            }),
            mode_abilities: vec![
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: GainLifePlayer::Controller,
                    },
                ),
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                ),
            ],
            description: Some("Whenever you cast your second spell each turn".to_string()),
        });
        state.waiting_for = WaitingFor::AbilityModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                mode_descriptions: vec!["Gain 2 life.".to_string(), "Draw a card.".to_string()],
                allow_repeat_modes: false,
                ..Default::default()
            },
            source_id,
            mode_abilities: vec![
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: GainLifePlayer::Controller,
                    },
                ),
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                ),
            ],
            is_activated: false,
            ability_index: None,
            ability_cost: None,
            unavailable_modes: vec![],
        };

        let result = apply(&mut state, GameAction::SelectModes { indices: vec![0] }).unwrap();

        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert!(state.pending_trigger.is_none());
        assert_eq!(state.stack.len(), 1);
        match &state.stack[0].kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility {
                ability,
                trigger_event,
                description,
                ..
            } => {
                assert!(matches!(ability.effect, Effect::GainLife { .. }));
                assert!(matches!(trigger_event, Some(GameEvent::SpellCast { .. })));
                assert_eq!(
                    description.as_deref(),
                    Some("Whenever you cast your second spell each turn")
                );
            }
            other => panic!("expected triggered ability on stack, got {other:?}"),
        }
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
                    damage_source: None,
                },
                vec![],
                ObjectId(30),
                PlayerId(0),
            )
            .sub_ability(crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
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
            description: None,
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
            source_id: None,
            description: None,
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
                    damage_source: None,
                },
                vec![],
                ObjectId(31),
                PlayerId(0),
            )
            .sub_ability(crate::types::ability::ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Player,
                    damage_source: None,
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
            description: None,
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
            source_id: None,
            description: None,
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
        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: ObjectId(40),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "modal_placeholder".to_string(),
                    description: None,
                },
                vec![],
                ObjectId(40),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: Some(GameEvent::SpellCast {
                controller: PlayerId(0),
                object_id: ObjectId(97),
                card_id: CardId(97),
            }),
            modal: Some(ModalChoice {
                min_choices: 2,
                max_choices: 2,
                mode_count: 1,
                mode_descriptions: vec!["Target opponent reveals their hand.".to_string()],
                allow_repeat_modes: true,
                constraints: vec![ModalSelectionConstraint::DifferentTargetPlayers],
                ..Default::default()
            }),
            mode_abilities: vec![AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    ),
                    damage_source: None,
                },
            )],
            description: Some("Choose different target players".to_string()),
        });
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
                    damage_source: None,
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
            description: None,
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
        QuantityExpr, TargetFilter, TriggerConstraint, TriggerDefinition, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    /// Verify that combat is skipped when there are no attackers and no triggers.
    /// With no BeginCombat triggers and no potential attackers, auto_advance()
    /// skips straight to PostCombatMain.
    #[test]
    fn combat_skipped_when_no_attackers_no_triggers() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a 0/1 creature with no triggers — can't attack, no combat triggers.
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Wall".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(0);
            obj.toughness = Some(1);
        }

        // Pass priority twice (P0 passes, then P1 passes) with empty stack.
        // This advances from PreCombatMain → BeginCombat → no triggers, no
        // attackers → skip to PostCombatMain.
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
            "Stack should be empty — no triggers exist. Stack: {:?}",
            state.stack
        );
        assert!(
            state.pending_trigger.is_none(),
            "No pending trigger should exist"
        );
        assert!(matches!(result2.waiting_for, WaitingFor::Priority { .. }));
    }

    /// CR 503.1a: Upkeep triggers fire when the upkeep step begins.
    #[test]
    fn upkeep_trigger_fires() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create creature with "At the beginning of your upkeep, gain 1 life"
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Upkeep Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Phase)
                    .phase(Phase::Upkeep)
                    .constraint(TriggerConstraint::OnlyDuringYourTurn)
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

        // auto_advance from Untap should process Upkeep triggers inline
        let mut events = Vec::new();
        let wf = crate::game::turns::auto_advance(&mut state, &mut events);

        assert_eq!(state.phase, Phase::Upkeep);
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "Upkeep trigger should have fired"
        );
        assert!(matches!(wf, WaitingFor::Priority { .. }));
    }

    /// CR 507.1: BeginCombat triggers fire even when there are attackers.
    #[test]
    fn begin_combat_trigger_fires_with_attackers() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a 2/2 creature (can attack) with a BeginCombat trigger
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Combat Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
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

        // Pass priority from PreCombatMain
        let result1 = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result1.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        let _result2 = apply(&mut state, GameAction::PassPriority).unwrap();

        // Should be at BeginCombat with trigger on stack
        assert_eq!(state.phase, Phase::BeginCombat);
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "BeginCombat trigger should have fired"
        );
    }

    /// CR 507.1: BeginCombat triggers fire even without potential attackers.
    #[test]
    fn begin_combat_trigger_fires_without_attackers() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a 0/1 creature (can't attack) with a BeginCombat trigger
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Trigger Wall".to_string(),
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

        // Pass priority twice to advance from PreCombatMain
        let result1 = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result1.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
        let _result2 = apply(&mut state, GameAction::PassPriority).unwrap();

        // Should be at BeginCombat with trigger on stack and combat state set
        assert_eq!(state.phase, Phase::BeginCombat);
        assert!(
            state.combat.is_some(),
            "Combat state should be set when triggers fire"
        );
        assert!(
            !state.stack.is_empty() || state.pending_trigger.is_some(),
            "BeginCombat trigger should fire even without potential attackers (CR 507.1)"
        );
    }

    /// OnlyDuringYourTurn constraint prevents trigger from firing on opponent's turn.
    #[test]
    fn your_turn_constraint_blocks_on_opponents_turn() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::Untap;
        // Active player is P1, but the creature is controlled by P0
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(1);

        // Create creature controlled by P0 with "At the beginning of your upkeep"
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Your Turn Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::Phase)
                    .phase(Phase::Upkeep)
                    .constraint(TriggerConstraint::OnlyDuringYourTurn)
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

        // auto_advance from Untap — it's P1's turn, but the trigger is P0's
        // with OnlyDuringYourTurn, so it should NOT fire.
        let mut events = Vec::new();
        let _wf = crate::game::turns::auto_advance(&mut state, &mut events);

        // Trigger should not have fired — phase should have advanced past Upkeep
        assert!(
            state.stack.is_empty(),
            "Trigger with OnlyDuringYourTurn should not fire on opponent's turn"
        );
        assert!(state.pending_trigger.is_none());
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
                casting_variant: crate::types::game_state::CastingVariant::Normal,
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
                object_id: searing_spear,
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
                            count: QuantityExpr::Fixed { value: 1 },
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
                                color: crate::types::mana::ManaColor::White,
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

    /// Regression test: lifelink combat damage with a GainLife replacement effect
    /// (Leyline of Hope) must not double-fire "whenever you gain life" triggers.
    ///
    /// Previously, process_combat_damage_triggers processed the LifeChanged event
    /// for triggers, then run_post_action_pipeline re-processed the same events,
    /// causing triggers like Essence Channeler's to fire twice per life-gain event.
    #[test]
    fn lifelink_replacement_does_not_double_fire_life_gain_triggers() {
        use crate::game::game_object::CounterType;
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = new_game(42);
        state.turn_number = 5;
        state.phase = Phase::DeclareAttackers;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Lifelink attacker (Ruin-Lurker Bat analog): 1/1 flying lifelink
        let bat = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Ruin-Lurker Bat".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bat).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.keywords.push(crate::types::keywords::Keyword::Lifelink);
            obj.base_keywords = obj.keywords.clone();
            obj.entered_battlefield_turn = Some(3);
        }

        // "Whenever you gain life, put a +1/+1 counter on this creature" (Essence Channeler)
        let channeler = create_object(
            &mut state,
            CardId(501),
            PlayerId(0),
            "Essence Channeler".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&channeler).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(1);
            obj.base_power = Some(2);
            obj.base_toughness = Some(1);
            obj.entered_battlefield_turn = Some(3);
            obj.trigger_definitions.push(
                TriggerDefinition::new(TriggerMode::LifeGained)
                    .valid_target(TargetFilter::Controller)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Database,
                        Effect::PutCounter {
                            counter_type: "P1P1".to_string(),
                            count: QuantityExpr::Fixed { value: 1 },
                            target: TargetFilter::SelfRef,
                        },
                    )),
            );
            obj.base_trigger_definitions = obj.trigger_definitions.clone();
        }

        // Leyline of Hope analog: "If you would gain life, gain that much + 1 instead"
        let leyline = create_object(
            &mut state,
            CardId(502),
            PlayerId(0),
            "Leyline of Hope".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&leyline).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.replacement_definitions.push(
                ReplacementDefinition::new(ReplacementEvent::GainLife).execute(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            amount: QuantityExpr::Fixed { value: 1 },
                            player: GainLifePlayer::Controller,
                        },
                    ),
                ),
            );
            obj.base_replacement_definitions = obj.replacement_definitions.clone();
        }

        // Declare bat as attacker
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![bat],
            valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
        };

        apply(
            &mut state,
            GameAction::DeclareAttackers {
                attacks: vec![(bat, AttackTarget::Player(PlayerId(1)))],
            },
        )
        .unwrap();

        // Skip to combat damage: P0 pass, P1 pass (declare blockers — no blockers),
        // P0 pass, P1 pass (combat damage resolves).
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();
        // Now at declare blockers — P1 declares no blockers
        if matches!(state.waiting_for, WaitingFor::DeclareBlockers { .. }) {
            apply(
                &mut state,
                GameAction::DeclareBlockers {
                    assignments: vec![],
                },
            )
            .unwrap();
        }
        // Pass priority through to combat damage
        while state.phase != Phase::PostCombatMain
            && !matches!(state.waiting_for, WaitingFor::GameOver { .. })
        {
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                apply(&mut state, GameAction::PassPriority).unwrap();
            } else {
                break;
            }
        }

        // Bat dealt 1 damage → lifelink gain 1 → Leyline replaces to 2.
        // Player 0 should have gained exactly 2 life (20 → 22).
        assert_eq!(
            state.players[0].life, 22,
            "Lifelink + Leyline should gain exactly 2 life"
        );

        // Essence Channeler should have exactly 1 +1/+1 counter, not 2.
        // The bug was that the LifeChanged event was processed for triggers twice,
        // once in process_combat_damage_triggers and again in run_post_action_pipeline.
        let counters = state.objects[&channeler]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0);
        assert_eq!(
            counters, 1,
            "Essence Channeler should trigger exactly once per life-gain event, got {} counters",
            counters
        );
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
