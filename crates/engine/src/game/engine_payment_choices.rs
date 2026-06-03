use crate::game::filter;
use crate::types::ability::{
    AbilityCondition, AbilityCost, Effect, EffectKind, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActionResult, AutoMayChoice, GameState, PendingContinuation, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::zones::Zone;

use super::casting;
use super::effects;
use super::engine::{
    check_exile_returns, handle_tap_land_for_mana, handle_untap_land_for_mana,
    resume_pending_continuation_if_priority, EngineError,
};
use super::life_costs::{pay_life_as_cost, PayLifeCostResult};
use super::mana_abilities;
use super::restrictions;
use super::zones;

pub(super) fn handle_optional_effect_choice(
    state: &mut GameState,
    accept: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    state.cost_payment_failed_flag = false;
    set_active_priority(state);

    if let Some(ability) = state.pending_optional_effect.take() {
        let choice = if accept {
            AutoMayChoice::Accept
        } else {
            AutoMayChoice::Decline
        };
        // CR 608.2: an ability's resolution is a single process; a triggered
        // ability suspended for its optional ("may") decision retains its
        // triggering event context. Restore it for the resumed resolution so
        // `TriggeringPlayer` and other event-context refs resolve correctly.
        let pending_event = state.pending_optional_trigger_event.take();
        let previous_trigger_event = state.current_trigger_event.clone();
        state.current_trigger_event = pending_event;
        // CR 603.2c + CR 608.2: mirror restoration of the batched-trigger
        // subject count so a `QuantityRef::EventContextAmount` resolved during
        // the resumed sub-ability reads the same "that many" the pre-pause
        // resolution would have observed.
        let pending_count = state.pending_optional_trigger_match_count.take();
        let previous_trigger_match_count = state.current_trigger_match_count;
        state.current_trigger_match_count = pending_count;
        let result = effects::resolve_optional_effect_decision(state, *ability, choice, events, 1);
        state.current_trigger_event = previous_trigger_event;
        state.current_trigger_match_count = previous_trigger_match_count;
        result.map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
    }

    resume_pending_continuation_if_priority(state, events)?;
    if state.resolving_begin_game_abilities
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        return Ok(super::mulligan::resume_begin_game_abilities(state, events));
    }
    Ok(state.waiting_for.clone())
}

pub(super) fn handle_optional_effect_choice_and_remember(
    state: &mut GameState,
    waiting_for: WaitingFor,
    choice: AutoMayChoice,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::OptionalEffectChoice {
        may_trigger_key: Some(key),
        ..
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Optional effect cannot be remembered".to_string(),
        ));
    };
    state.set_may_trigger_auto_choice(key, choice);
    handle_optional_effect_choice(state, matches!(choice, AutoMayChoice::Accept), events)
}

pub(super) fn handle_opponent_may_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    accept: bool,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let WaitingFor::OpponentMayChoice {
        player: promptee,
        remaining,
        source_id,
        description,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for opponent-may choice".to_string(),
        ));
    };

    state.cost_payment_failed_flag = false;

    if accept {
        if let Some(mut ability) = state.pending_optional_effect.take() {
            ability.optional = false;
            ability.optional_for = None;
            ability.context.optional_effect_performed = true;
            ability.context.accepting_player = Some(promptee);

            let target_selection = match &ability.effect {
                Effect::Sacrifice { target, .. } | Effect::Tap { target } => {
                    let require_untapped = matches!(ability.effect, Effect::Tap { .. });
                    let legal: Vec<ObjectId> = state
                        .objects
                        .iter()
                        .filter(|(_, obj)| {
                            obj.zone == Zone::Battlefield
                                && obj.controller == promptee
                                && (!require_untapped || !obj.tapped)
                                && filter::matches_target_filter(
                                    state,
                                    obj.id,
                                    target,
                                    &filter::FilterContext::from_source_with_controller(
                                        ability.source_id,
                                        promptee,
                                    ),
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
                    if let Some(sub) = ability.sub_ability.take() {
                        state.pending_continuation = Some(PendingContinuation::new(sub));
                    }
                    state.waiting_for = WaitingFor::MultiTargetSelection {
                        player: promptee,
                        legal_targets: legal,
                        min_targets: 1,
                        max_targets: 1,
                        pending_ability: ability,
                    };
                    return Ok(action_result(events, state.waiting_for.clone()));
                }

                set_active_priority(state);
                effects::resolve_ability_chain(state, &ability, events, 1)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            } else {
                if matches!(ability.effect, Effect::DealDamage { .. }) {
                    ability.targets = vec![TargetRef::Player(promptee)];
                }
                set_active_priority(state);
                effects::resolve_ability_chain(state, &ability, events, 1)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
        }
    } else if !remaining.is_empty() {
        let next = remaining[0];
        let rest = remaining[1..].to_vec();
        state.waiting_for = WaitingFor::OpponentMayChoice {
            player: next,
            source_id,
            description,
            remaining: rest,
        };
        return Ok(action_result(events, state.waiting_for.clone()));
    } else {
        set_active_priority(state);
        if let Some(ability) = state.pending_optional_effect.take() {
            if let Some(ref sub) = ability.sub_ability {
                if sub
                    .condition
                    .as_ref()
                    .is_some_and(AbilityCondition::is_optional_effect_performed)
                {
                    if let Some(ref else_branch) = sub.else_ability {
                        let mut else_resolved = else_branch.as_ref().clone();
                        else_resolved.context = ability.context.clone();
                        effects::resolve_ability_chain(state, &else_resolved, events, 1)
                            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                    }
                }
            }
        }
    }

    resume_pending_continuation_if_priority(state, events)?;
    Ok(action_result(events, state.waiting_for.clone()))
}

/// CR 702.104a: Resolve the chosen opponent's pay/decline decision for a Tribute
/// creature. On accept, add N +1/+1 counters to the source and persist
/// `TributeOutcome::Paid`. On decline, persist `TributeOutcome::Declined`. Either
/// way, the companion "if tribute wasn't paid" trigger (CR 702.104b) can read the
/// recorded outcome.
pub(super) fn handle_tribute_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    accept: bool,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let WaitingFor::TributeChoice {
        player,
        source_id,
        count,
        ..
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for tribute choice".to_string(),
        ));
    };

    if accept {
        effects::tribute::apply_paid(state, player, source_id, count, events);
    } else {
        effects::tribute::apply_declined(state, source_id);
    }

    // Return priority to the active player so the ETB triggered ability can see
    // the persisted TributeOutcome when its intervening-if condition is checked.
    set_active_priority(state);
    resume_pending_continuation_if_priority(state, events)?;
    Ok(action_result(events, state.waiting_for.clone()))
}

/// CR 118.12a: Resolve the player's choice between sub-costs of a disjunctive
/// unless-cost. `UnlessCostBranch::Pay { index }` re-enters
/// `handle_unless_payment` with the chosen single cost as `pay: true`;
/// `UnlessCostBranch::Decline` declines all branches (effect happens),
/// mirroring `PayUnlessCost { pay: false }`.
pub(super) fn handle_unless_payment_choose_cost(
    state: &mut GameState,
    waiting_for: WaitingFor,
    choice: crate::types::actions::UnlessCostBranch,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    use crate::types::actions::UnlessCostBranch;
    let WaitingFor::UnlessPaymentChooseCost {
        player,
        costs,
        pending_effect,
        trigger_event,
        effect_description,
        mut remaining_choices,
        mut chosen,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for unless-payment cost branch".to_string(),
        ));
    };

    match choice {
        UnlessCostBranch::Pay { index } => {
            let picked = costs.get(index).cloned().ok_or_else(|| {
                EngineError::InvalidAction(format!(
                    "ChooseUnlessCostBranch index {index} out of range \
                     (have {} sub-costs)",
                    costs.len()
                ))
            })?;
            // CR 702.24a + CR 118.12: If more disjunctive prompts remain
            // (cumulative-upkeep `OneOf × N` expansion), accumulate this pick
            // and surface the next prompt without paying anything yet.
            // "Each choice is made separately for each age counter, then
            // either the entire set of costs is paid, or none of them is
            // paid."
            chosen.push(picked);
            if !remaining_choices.is_empty() {
                let next_costs = remaining_choices.remove(0);
                state.waiting_for = WaitingFor::UnlessPaymentChooseCost {
                    player,
                    costs: next_costs,
                    pending_effect,
                    trigger_event,
                    effect_description,
                    remaining_choices,
                    chosen,
                };
                return Ok(action_result(events, state.waiting_for.clone()));
            }
            // All choices made — collapse into a single cost and re-enter
            // the standard single-cost path with `pay: true`. The
            // pending_effect already has `unless_pay = None` (cleared by
            // `surface_unless_payment`).
            let final_cost = if chosen.len() == 1 {
                chosen.into_iter().next().unwrap()
            } else {
                AbilityCost::Composite { costs: chosen }
            };
            let next = WaitingFor::UnlessPayment {
                player,
                cost: final_cost,
                pending_effect,
                trigger_event,
                effect_description,
                // Disjunctive (`OneOf`) unless-costs are single-payer — the
                // "any player" poll never co-occurs with a sub-cost choice.
                remaining: Vec::new(),
            };
            handle_unless_payment(state, next, true, events)
        }
        UnlessCostBranch::Decline => {
            // CR 118.12 + CR 702.24a: Declining any prompt in the sequence
            // declines the whole disjunctive unless-cost — falls through to
            // the effect happening, equivalent to `PayUnlessCost { pay:
            // false }` on the single-cost path. Re-enter
            // `handle_unless_payment` with `pay: false` and any
            // representative cost (the cost is unused on the decline path:
            // `handle_unless_payment` routes straight to
            // `resolve_ability_chain` on the `!pay || payment_failed`
            // branch, never reading `cost`). Use the first sub-cost as a
            // stand-in so the WaitingFor shape is valid even though the
            // cost itself is not consulted.
            let stand_in_cost = costs.into_iter().next().unwrap_or(AbilityCost::Mana {
                cost: ManaCost::zero(),
            });
            let next = WaitingFor::UnlessPayment {
                player,
                cost: stand_in_cost,
                pending_effect,
                trigger_event,
                effect_description,
                remaining: Vec::new(),
            };
            handle_unless_payment(state, next, false, events)
        }
    }
}

pub(super) fn handle_unless_payment(
    state: &mut GameState,
    waiting_for: WaitingFor,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<ActionResult, EngineError> {
    let WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for unless payment".to_string(),
        ));
    };

    // CR 118.12a: Preserved for the "unless any player pays" poll re-emit —
    // `cost` itself is moved by the `match cost` below on the pay path.
    let poll_cost = cost.clone();

    let mut payment_failed = !pay;
    if pay {
        match cost {
            // CR 118.12: Pay the static mana component of the unless cost.
            AbilityCost::Mana { cost: mana_cost } => {
                casting::pay_unless_cost(state, player, &mana_cost, events)?;
            }
            // CR 118.4 + CR 107.3c: A dynamic generic cost should have been
            // resolved into a fixed `Mana { cost }` upstream (in the
            // `unless_pay` interceptor in `effects::mod`). Reaching this arm
            // means the resolution was skipped — that's an engine invariant
            // bug, not a runtime condition.
            AbilityCost::ManaDynamic { .. } => {
                unreachable!("ManaDynamic should be resolved before payment");
            }
            // CR 118.12 + CR 118.3 + CR 119.4 + CR 119.8: Unless-pay life
            // routes through the single-authority helper. An unpayable cost
            // (insufficient life, or CantLoseLife lock) causes the "unless"
            // branch to fall through to the effect still happening.
            AbilityCost::PayLife { amount } => {
                // CR 107.3c: Resolve the `QuantityExpr` against game state so
                // dynamic life amounts (e.g., "pay X life where X is your
                // opponents' life total") read the chosen X at payment time.
                let life_amount = crate::game::quantity::resolve_quantity_with_targets(
                    state,
                    &amount,
                    pending_effect.as_ref(),
                );
                let life_amount = u32::try_from(life_amount.max(0)).unwrap_or(0);
                match pay_life_as_cost(state, player, life_amount, events) {
                    PayLifeCostResult::Paid { .. } => {}
                    PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited => {
                        payment_failed = true;
                    }
                }
            }
            // CR 118.12 + CR 118.12a: "[Effect] unless [player] pays [cost]"
            // — the player chose to pay; deduct the cost and skip the effect.
            // CR 107.14: Paying {E} removes one energy counter from the
            // paying player per `{E}` symbol in the cost. Energy counters
            // are tracked on `Player.energy` (no zone), so the deduction is
            // a direct counter-state mutation.
            AbilityCost::PayEnergy { amount } => {
                // CR 107.3c: Resolve the `QuantityExpr` against game state
                // before the mutable borrow below so dynamic amounts (e.g.
                // "an amount of {E} equal to its mana value") read the parent
                // target at payment time.
                let energy_amount = crate::game::quantity::resolve_quantity_with_targets(
                    state,
                    &amount,
                    pending_effect.as_ref(),
                );
                let energy_amount = u32::try_from(energy_amount.max(0)).unwrap_or(0);
                let Some(player_state) = state.players.iter_mut().find(|p| p.id == player) else {
                    return Err(EngineError::InvalidAction(
                        "Unless payment player not found".to_string(),
                    ));
                };
                if player_state.energy < energy_amount {
                    payment_failed = true;
                } else {
                    player_state.energy -= energy_amount;
                    events.push(GameEvent::EnergyChanged {
                        player,
                        delta: -(energy_amount as i32),
                    });
                }
            }
            // CR 118.12 + CR 701.9: Unless-discard. Defers to the unified
            // `WardDiscardChoice` waiting state (the name predates the fold
            // and now covers both ward and counter unless-discard cases).
            // `count`/`random`/`self_ref` axes from the unified `Discard`
            // shape are not yet consumed at this site — extending them is
            // future work tracked alongside the `Balduvian Horde` random-
            // discard fidelity gap.
            AbilityCost::Discard {
                count: _,
                filter,
                random: _,
                self_ref: _,
            } => {
                let hand_cards = crate::game::casting::find_eligible_discard_targets(
                    state,
                    player,
                    pending_effect.source_id,
                    filter.as_ref(),
                );
                if hand_cards.is_empty() {
                    payment_failed = true;
                } else {
                    state.waiting_for = WaitingFor::WardDiscardChoice {
                        player,
                        cards: hand_cards,
                        pending_effect: pending_effect.clone(),
                    };
                    return Ok(action_result(events, state.waiting_for.clone()));
                }
            }
            // CR 118.12 + CR 701.21: Unless-sacrifice — collect eligible
            // permanents and surface the choice via `WardSacrificeChoice`.
            AbilityCost::Sacrifice {
                count,
                target: ref filter,
            } => {
                let sac_source = pending_effect.source_id;
                let ctx = crate::game::filter::FilterContext::from_source_with_controller(
                    sac_source, player,
                );
                let eligible: Vec<ObjectId> = state
                    .battlefield
                    .iter()
                    .filter(|id| {
                        state
                            .objects
                            .get(id)
                            .map(|obj| {
                                obj.controller == player
                                    && !obj.is_emblem
                                    && crate::game::filter::matches_target_filter(
                                        state, **id, filter, &ctx,
                                    )
                            })
                            .unwrap_or(false)
                    })
                    .copied()
                    .collect();
                if eligible.len() < count as usize {
                    payment_failed = true;
                } else {
                    state.waiting_for = WaitingFor::WardSacrificeChoice {
                        player,
                        permanents: eligible,
                        pending_effect: pending_effect.clone(),
                        remaining: count,
                    };
                    return Ok(action_result(events, state.waiting_for.clone()));
                }
            }
            // CR 118.12: Return-to-hand unless cost. `from_zone` defaults to
            // battlefield (the standard shape); `Some(Zone::Graveyard)` is
            // used by Harvest Wurm and similar.
            AbilityCost::ReturnToHand {
                count,
                ref filter,
                ref from_zone,
            } => {
                let source = pending_effect.source_id;
                let ctx =
                    crate::game::filter::FilterContext::from_source_with_controller(source, player);
                let zone_objects: Vec<ObjectId> = match from_zone {
                    Some(Zone::Graveyard) => state
                        .players
                        .iter()
                        .find(|p| p.id == player)
                        .map(|p| p.graveyard.iter().copied().collect())
                        .unwrap_or_default(),
                    _ => state.battlefield.iter().copied().collect(),
                };
                let filter_ref = filter.as_ref();
                let eligible: Vec<ObjectId> = zone_objects
                    .iter()
                    .filter(|id| {
                        state
                            .objects
                            .get(id)
                            .map(|obj| {
                                obj.controller == player
                                    && !obj.is_emblem
                                    && filter_ref.is_none_or(|f| {
                                        crate::game::filter::matches_target_filter(
                                            state, **id, f, &ctx,
                                        )
                                    })
                            })
                            .unwrap_or(false)
                    })
                    .copied()
                    .collect();
                if eligible.len() < count as usize {
                    payment_failed = true;
                } else {
                    state.waiting_for = WaitingFor::UnlessBounceChoice {
                        player,
                        permanents: eligible,
                        pending_effect: pending_effect.clone(),
                        remaining: count,
                    };
                    return Ok(action_result(events, state.waiting_for.clone()));
                }
            }
            // CR 118.12: AbilityCost variants below are not currently emitted
            // as unless-pay costs by the parser. If a card surfaces one, the
            // unless branch fails and the effect happens unconditionally.
            // Listed exhaustively (no wildcard) so future cost additions
            // force a deliberate decision here.
            // CR 118.12a: `OneOf` — surface a sub-cost choice. Once the
            // player picks an index, the resolver re-enters `handle_unless_payment`
            // with the chosen single cost via
            // `handle_unless_payment_choose_cost`. Reaching this arm means
            // the choice was not made yet — that is an engine invariant
            // bug, not a runtime condition. The choice transition happens
            // in `surface_unless_payment` (effects/mod.rs) before this
            // function is ever called with a `OneOf` cost.
            AbilityCost::OneOf { .. } => {
                unreachable!(
                    "OneOf unless-cost should have been resolved to a single \
                     AbilityCost by handle_unless_payment_choose_cost before \
                     reaching handle_unless_payment"
                );
            }
            // CR 702.24a: `PerCounter` is expanded against game state at the
            // unless-payment entry point in `effects/mod.rs` — the expanded
            // base (Mana / Composite / OneOf / PayLife / Sacrifice), not the
            // `PerCounter` wrapper, reaches this match. Listed here so the
            // exhaustive match documents the invariant.
            AbilityCost::PerCounter { .. } => {
                unreachable!(
                    "PerCounter unless-cost should have been expanded against \
                     game state at the unless-payment entry point before \
                     reaching handle_unless_payment"
                );
            }
            // CR 702.24a + CR 118.12: `Composite` of `Mana` sub-costs is the
            // shape produced when `handle_unless_payment_choose_cost`
            // accumulates per-counter disjunctive picks for an `OneOf × N`
            // cumulative-upkeep expansion (e.g., Jötun Owl Keeper at N age
            // counters chooses `{W}` or `{U}` for each, yielding `Composite[
            // Mana{...}, Mana{...}, ...]`). Sum the inner mana costs via
            // `ManaCost::plus` and pay as a single combined mana cost — the
            // same aggregation used by combat-tax `scaled()` payment.
            // "Then either the entire set of costs is paid, or none of them
            // is paid. Partial payments aren't allowed."
            //
            // Mixed `Composite` (e.g., `Composite[Mana, PayLife]`) is
            // **explicitly out of scope** here — no current MTG card
            // produces a mixed unless-payment composite. Extend with a
            // sequenced sub-cost payer when one ships.
            AbilityCost::Composite { costs }
                if costs.iter().all(|c| matches!(c, AbilityCost::Mana { .. })) =>
            {
                let combined = costs.iter().fold(ManaCost::zero(), |acc, c| match c {
                    AbilityCost::Mana { cost } => acc.plus(cost),
                    _ => unreachable!("guard ensures all Mana"),
                });
                casting::pay_unless_cost(state, player, &combined, events)?;
            }
            AbilityCost::Composite { .. } => {
                // CR 702.24a + CR 118.12: A non-all-Mana `Composite`
                // unless-cost is not yet supported. No current MTG card
                // produces this shape (cumulative upkeep with mixed
                // disjunctive sub-costs is empirically Mana-only). Falling
                // through to `payment_failed = true` makes the unless-effect
                // happen, which is the rules-correct fallback for an
                // unpayable cost (CR 118.12: declining is equivalent).
                payment_failed = true;
            }
            AbilityCost::Tap
            | AbilityCost::Untap
            | AbilityCost::Unattach
            | AbilityCost::Loyalty { .. }
            | AbilityCost::PaySpeed { .. }
            | AbilityCost::Exile { .. }
            | AbilityCost::ExileMaterials { .. }
            | AbilityCost::CollectEvidence { .. }
            | AbilityCost::TapCreatures { .. }
            | AbilityCost::RemoveCounter { .. }
            | AbilityCost::Mill { .. }
            | AbilityCost::Exert
            | AbilityCost::Blight { .. }
            | AbilityCost::Reveal { .. }
            | AbilityCost::Behold { .. }
            | AbilityCost::Waterbend { .. }
            | AbilityCost::NinjutsuFamily { .. }
            | AbilityCost::EffectCost { .. }
            | AbilityCost::Unimplemented { .. } => {
                payment_failed = true;
            }
        }

        if !payment_failed {
            clear_echo_due_for_echo_payment(state, &pending_effect);
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&pending_effect.effect),
                source_id: pending_effect.source_id,
            });

            // CR 118.12 + CR 118.12a: "[Effect] unless [player] pays [cost].
            // If they do, [alternative]." When the payment succeeds, the
            // primary effect is suppressed (above) and the `IfAPlayerDoes`
            // sub_ability runs as the alternative outcome. Mirrors the
            // `OpponentMayChoice` accept path (`engine_payment_choices.rs`
            // L88-L94) which sets `optional_effect_performed=true` on the
            // accepted ability so `evaluate_condition` honors `IfAPlayerDoes`
            // (`effects/mod.rs` L2156-L2158). Cards: Rhystic Lightning,
            // Don't Make a Sound, Divert Disaster, Assimilate Essence.
            if let Some(sub) = pending_effect.sub_ability.as_ref().filter(|sub| {
                sub.condition
                    .as_ref()
                    .is_some_and(AbilityCondition::is_optional_effect_performed)
            }) {
                // Abandon Attachments #81 parallel: a stale
                // `cost_payment_failed_flag` from a previous resolution would
                // make `evaluate_condition` reject the IfAPlayerDoes condition
                // (`effects/mod.rs` L2156-L2158: `&& !cost_payment_failed_flag`).
                // Clear it on the success path the same way
                // `handle_optional_effect_choice` (L29) and
                // `handle_opponent_may_choice` (L88) do for their accept paths.
                state.cost_payment_failed_flag = false;
                let mut sub_resolved = sub.as_ref().clone();
                if sub_resolved.targets.is_empty() {
                    sub_resolved.targets = pending_effect.targets.clone();
                }
                sub_resolved.context = pending_effect.context.clone();
                sub_resolved.context.optional_effect_performed = true;
                let previous_trigger_event = state.current_trigger_event.clone();
                state.current_trigger_event = trigger_event.clone();
                let result = effects::resolve_ability_chain(state, &sub_resolved, events, 0);
                state.current_trigger_event = previous_trigger_event;
                result.map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            }
        }
    }

    if !pay || payment_failed {
        // CR 118.12a: "[Effect] unless any player pays ..." poll — when the
        // current player declines (or cannot pay) and more players remain,
        // prompt the next player in APNAP order rather than resolving the
        // effect. The first player to pay prevents the effect; only when every
        // polled player has declined does the effect resolve (once). Mirrors
        // the `OpponentMayChoice` decline-branch poll re-emit.
        if let Some((&next, rest)) = remaining.split_first() {
            state.waiting_for = WaitingFor::UnlessPayment {
                player: next,
                cost: poll_cost,
                pending_effect: pending_effect.clone(),
                trigger_event: trigger_event.clone(),
                effect_description: effect_description.clone(),
                remaining: rest.to_vec(),
            };
            return Ok(action_result(events, state.waiting_for.clone()));
        }

        let ability = pending_effect.as_ref().clone();
        clear_echo_due_for_echo_payment(state, &ability);
        // Post-fold: `unless_pay` was already cleared on `pending_effect`
        // when the unless prompt was first surfaced (`effects::mod` strips
        // it before sending the pending effect into `WaitingFor`), so no
        // further stripping is needed here.
        let previous_trigger_event = state.current_trigger_event.clone();
        state.current_trigger_event = trigger_event.clone();
        let result = effects::resolve_ability_chain(state, &ability, events, 0);
        state.current_trigger_event = previous_trigger_event;
        result.map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;

        // CR 610.3 + #783: The unless-payment resume bypasses
        // `run_post_action_pipeline` (see the inline-scan note in
        // engine.rs), so the standard exile-return scan never runs. When the
        // resolved effect makes a source leave the battlefield — e.g. Static
        // Prison sacrificing itself via "sacrifice unless you pay {E}" — the
        // permanent it exiled "until it leaves the battlefield" must return.
        // Idempotent and event-scoped: a no-op when no source left this way.
        check_exile_returns(state, events);
    }

    if matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }) {
        set_active_priority(state);
    }
    resume_pending_continuation_if_priority(state, events)?;
    Ok(action_result(events, state.waiting_for.clone()))
}

fn clear_echo_due_for_echo_payment(
    state: &mut GameState,
    pending_effect: &crate::types::ability::ResolvedAbility,
) {
    let is_echo_sacrifice = matches!(
        &pending_effect.effect,
        Effect::Sacrifice {
            target: TargetFilter::SelfRef,
            ..
        }
    );
    if !is_echo_sacrifice {
        return;
    }

    if let Some(obj) = state.objects.get_mut(&pending_effect.source_id) {
        if obj.echo_due && obj.keywords.iter().any(|kw| matches!(kw, Keyword::Echo(_))) {
            obj.echo_due = false;
        }
    }
}

pub(super) fn handle_unless_payment_tap_land_for_mana(
    state: &mut GameState,
    waiting_for: WaitingFor,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for unless payment".to_string(),
        ));
    };

    handle_tap_land_for_mana(state, object_id, events)?;
    state
        .lands_tapped_for_mana
        .entry(player)
        .or_default()
        .push(object_id);

    Ok(WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    })
}

pub(super) fn handle_unless_payment_untap_land_for_mana(
    state: &mut GameState,
    waiting_for: WaitingFor,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for unless payment".to_string(),
        ));
    };

    handle_untap_land_for_mana(state, player, object_id, events)?;
    Ok(WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    })
}

pub(super) fn handle_unless_payment_activate_ability(
    state: &mut GameState,
    waiting_for: WaitingFor,
    source_id: ObjectId,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::UnlessPayment {
        player,
        cost,
        pending_effect,
        trigger_event,
        effect_description,
        remaining,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for unless payment".to_string(),
        ));
    };

    let object = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    if ability_index >= object.abilities.len()
        || !mana_abilities::is_mana_ability(&object.abilities[ability_index])
    {
        return Err(EngineError::ActionNotAllowed(
            "Only mana abilities can be activated during unless payment".to_string(),
        ));
    }

    let ability_def = object.abilities[ability_index].clone();
    mana_abilities::activate_mana_ability(
        state,
        source_id,
        player,
        ability_index,
        &ability_def,
        events,
        crate::types::game_state::ManaAbilityResume::UnlessPayment {
            cost: Box::new(cost),
            pending_effect,
            trigger_event,
            effect_description,
            remaining,
        },
        None,
    )?;
    Ok(state.waiting_for.clone())
}

pub(super) fn handle_ward_discard_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    chosen: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::WardDiscardChoice {
        player,
        cards: legal_cards,
        pending_effect,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for ward discard choice".to_string(),
        ));
    };

    if chosen.len() != 1 || !legal_cards.contains(&chosen[0]) {
        return Err(EngineError::InvalidAction(
            "Must select exactly one card to discard".to_string(),
        ));
    }

    zones::move_to_zone(state, chosen[0], Zone::Graveyard, events);
    restrictions::record_discard(state, player);
    events.push(GameEvent::Discarded {
        player_id: player,
        object_id: chosen[0],
    });
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&pending_effect.effect),
        source_id: pending_effect.source_id,
    });

    set_active_priority(state);
    resume_pending_continuation_if_priority(state, events)?;
    Ok(state.waiting_for.clone())
}

pub(super) fn handle_ward_sacrifice_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    chosen: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::WardSacrificeChoice {
        player,
        permanents,
        pending_effect,
        remaining,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for ward sacrifice choice".to_string(),
        ));
    };

    if chosen.len() != 1 || !permanents.contains(&chosen[0]) {
        return Err(EngineError::InvalidAction(
            "Must select exactly one permanent to sacrifice".to_string(),
        ));
    }

    // CR 603.10a + CR 118.8: NOTE — sequential Ward multi-sacrifice is a separate
    // co-departed gap. Each Ward sacrifice is taken in its own action's `events`
    // (one permanent per round-trip, re-prompting for `remaining - 1`), so the
    // permanents paying one Ward cost are never stamped as a simultaneous departure
    // group; the `handle_sacrifice_for_cost` co-departed stamp does not apply here.
    // A co-departing observer therefore under-observes. Closing this would batch all
    // Ward sacrifices into one action (like `handle_sacrifice_for_cost`) — out of scope.
    crate::game::sacrifice::sacrifice_permanent(state, chosen[0], player, events)?;

    // If more sacrifices remain, re-prompt with updated eligible permanents
    if remaining > 1 {
        let eligible: Vec<ObjectId> = permanents
            .into_iter()
            .filter(|&id| id != chosen[0] && state.objects.contains_key(&id))
            .collect();
        state.waiting_for = WaitingFor::WardSacrificeChoice {
            player,
            permanents: eligible,
            pending_effect,
            remaining: remaining - 1,
        };
        return Ok(state.waiting_for.clone());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&pending_effect.effect),
        source_id: pending_effect.source_id,
    });

    set_active_priority(state);
    resume_pending_continuation_if_priority(state, events)?;
    Ok(state.waiting_for.clone())
}

/// CR 118.12: Handle player's selection of a permanent to return to hand as unless cost.
pub(super) fn handle_unless_bounce_choice(
    state: &mut GameState,
    waiting_for: WaitingFor,
    chosen: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::UnlessBounceChoice {
        player,
        permanents,
        pending_effect,
        remaining,
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for unless bounce choice".to_string(),
        ));
    };

    if chosen.len() != 1 || !permanents.contains(&chosen[0]) {
        return Err(EngineError::InvalidAction(
            "Must select exactly one permanent to return to hand".to_string(),
        ));
    }

    zones::move_to_zone(state, chosen[0], Zone::Hand, events);

    if remaining > 1 {
        let eligible: Vec<ObjectId> = permanents
            .into_iter()
            .filter(|&id| id != chosen[0] && state.objects.contains_key(&id))
            .collect();
        state.waiting_for = WaitingFor::UnlessBounceChoice {
            player,
            permanents: eligible,
            pending_effect,
            remaining: remaining - 1,
        };
        return Ok(state.waiting_for.clone());
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&pending_effect.effect),
        source_id: pending_effect.source_id,
    });

    set_active_priority(state);
    resume_pending_continuation_if_priority(state, events)?;
    Ok(state.waiting_for.clone())
}

fn set_active_priority(state: &mut GameState) {
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    state.priority_player = state.active_player;
}

fn action_result(events: &mut Vec<GameEvent>, waiting_for: WaitingFor) -> ActionResult {
    ActionResult {
        events: std::mem::take(events),
        waiting_for,
        log_entries: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, ControllerRef, QuantityExpr, ResolvedAbility, SubAbilityLink, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{AutoMayChoice, MayTriggerAutoChoiceKey, MayTriggerOrigin};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn gain_life(value: i32) -> Effect {
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value },
            player: TargetFilter::Controller,
        }
    }

    #[test]
    fn declining_optional_effect_resolves_not_if_you_do_subability() {
        let mut state = GameState::new_two_player(42);
        let mut optional = ResolvedAbility::new(gain_life(1), vec![], ObjectId(100), PlayerId(0));
        optional.optional = true;
        let mut decline_branch =
            ResolvedAbility::new(gain_life(3), vec![], ObjectId(100), PlayerId(0));
        decline_branch.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        });
        optional.sub_ability = Some(Box::new(decline_branch));
        state.pending_optional_effect = Some(Box::new(optional));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };

        let mut events = Vec::new();
        handle_optional_effect_choice(&mut state, false, &mut events)
            .expect("decline branch should resolve");

        assert_eq!(state.players[0].life, 23);
    }

    #[test]
    fn declining_optional_effect_resolves_not_if_a_player_does_subability() {
        let mut state = GameState::new_two_player(42);
        let mut optional = ResolvedAbility::new(gain_life(1), vec![], ObjectId(100), PlayerId(0));
        optional.optional = true;
        let mut decline_branch =
            ResolvedAbility::new(gain_life(3), vec![], ObjectId(100), PlayerId(0));
        decline_branch.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        });
        optional.sub_ability = Some(Box::new(decline_branch));
        state.pending_optional_effect = Some(Box::new(optional));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };

        let mut events = Vec::new();
        handle_optional_effect_choice(&mut state, false, &mut events)
            .expect("decline branch should resolve");

        assert_eq!(state.players[0].life, 23);
    }

    #[test]
    fn declining_optional_effect_prefers_else_ability() {
        let mut state = GameState::new_two_player(42);
        let mut optional = ResolvedAbility::new(gain_life(1), vec![], ObjectId(100), PlayerId(0));
        optional.optional = true;
        optional.else_ability = Some(Box::new(ResolvedAbility::new(
            gain_life(2),
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));
        let mut decline_branch =
            ResolvedAbility::new(gain_life(3), vec![], ObjectId(100), PlayerId(0));
        decline_branch.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        });
        optional.sub_ability = Some(Box::new(decline_branch));
        state.pending_optional_effect = Some(Box::new(optional));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };

        let mut events = Vec::new();
        handle_optional_effect_choice(&mut state, false, &mut events)
            .expect("else branch should resolve");

        assert_eq!(state.players[0].life, 22);
    }

    #[test]
    fn declining_optional_effect_resolves_if_you_do_subability_else_branch() {
        let mut state = GameState::new_two_player(42);
        let mut optional = ResolvedAbility::new(gain_life(1), vec![], ObjectId(100), PlayerId(0));
        optional.optional = true;
        let mut if_you_do = ResolvedAbility::new(gain_life(2), vec![], ObjectId(100), PlayerId(0));
        if_you_do.condition = Some(AbilityCondition::effect_performed());
        if_you_do.else_ability = Some(Box::new(ResolvedAbility::new(
            gain_life(4),
            vec![],
            ObjectId(100),
            PlayerId(0),
        )));
        optional.sub_ability = Some(Box::new(if_you_do));
        state.pending_optional_effect = Some(Box::new(optional));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };

        let mut events = Vec::new();
        handle_optional_effect_choice(&mut state, false, &mut events)
            .expect("sub-ability else branch should resolve");

        assert_eq!(state.players[0].life, 24);
    }

    #[test]
    fn declining_optional_effect_skips_ordinary_continuation() {
        let mut state = GameState::new_two_player(42);
        let mut optional = ResolvedAbility::new(gain_life(1), vec![], ObjectId(100), PlayerId(0));
        optional.optional = true;
        let mut continuation_sub =
            ResolvedAbility::new(gain_life(3), vec![], ObjectId(100), PlayerId(0));
        // CR 608.2c: This sub is a within-clause continuation step of the
        // declined action — declining the optional must skip it. Made explicit
        // so the case under test is unambiguous to a future reader.
        continuation_sub.sub_link = SubAbilityLink::ContinuationStep;
        optional.sub_ability = Some(Box::new(continuation_sub));
        state.pending_optional_effect = Some(Box::new(optional));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };

        let mut events = Vec::new();
        handle_optional_effect_choice(&mut state, false, &mut events)
            .expect("declining ordinary optional effect should resolve");

        assert_eq!(state.players[0].life, 20);
    }

    #[test]
    fn accepting_optional_effect_skips_not_if_you_do_subability() {
        let mut state = GameState::new_two_player(42);
        let mut optional = ResolvedAbility::new(gain_life(1), vec![], ObjectId(100), PlayerId(0));
        optional.optional = true;
        let mut decline_branch =
            ResolvedAbility::new(gain_life(3), vec![], ObjectId(100), PlayerId(0));
        decline_branch.condition = Some(AbilityCondition::Not {
            condition: Box::new(AbilityCondition::effect_performed()),
        });
        optional.sub_ability = Some(Box::new(decline_branch));
        state.pending_optional_effect = Some(Box::new(optional));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(100),
            description: None,
            may_trigger_key: None,
        };

        let mut events = Vec::new();
        handle_optional_effect_choice(&mut state, true, &mut events)
            .expect("accepted optional effect should resolve");

        assert_eq!(state.players[0].life, 21);
    }

    #[test]
    fn remember_optional_effect_records_key_and_resolves_choice() {
        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(100);
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id,
            origin: MayTriggerOrigin::Printed { trigger_index: 0 },
        };
        let mut optional = ResolvedAbility::new(gain_life(2), vec![], source_id, PlayerId(0));
        optional.optional = true;
        state.pending_optional_effect = Some(Box::new(optional));
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id,
            description: None,
            may_trigger_key: Some(key),
        };

        let mut events = Vec::new();
        handle_optional_effect_choice_and_remember(
            &mut state,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(0),
                source_id,
                description: None,
                may_trigger_key: Some(key),
            },
            AutoMayChoice::Accept,
            &mut events,
        )
        .expect("remembered optional choice should resolve");

        assert_eq!(
            state.may_trigger_auto_choice(&key),
            Some(AutoMayChoice::Accept)
        );
        assert_eq!(state.players[0].life, 22);
    }

    #[test]
    fn remember_optional_effect_rejects_unkeyed_prompt() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let result = handle_optional_effect_choice_and_remember(
            &mut state,
            WaitingFor::OptionalEffectChoice {
                player: PlayerId(0),
                source_id: ObjectId(100),
                description: None,
                may_trigger_key: None,
            },
            AutoMayChoice::Accept,
            &mut events,
        );

        assert!(result.is_err());
    }

    /// CR 118.12 + CR 119.4 + CR 107.3c (M1 fold): An unless-pay-life cost
    /// with a `QuantityExpr` amount evaluates the quantity at unless-time.
    /// Pre-fold the cost was an `i32`; post-fold it carries the same widened
    /// `QuantityExpr` shape as `AbilityCost::PayLife`. Using a fixed expr
    /// here exercises the resolution path without needing a dynamic ref.
    #[test]
    fn unless_pay_life_widened_to_quantity_expr_resolves_at_payment() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let pending = ResolvedAbility::new(gain_life(5), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let mut events = Vec::new();
        let waiting_for = state.waiting_for.clone();
        handle_unless_payment(&mut state, waiting_for, true, &mut events)
            .expect("unless-pay-life should resolve");
        // Player paid 3 life — life total drops by 3, gain-life effect skipped.
        assert_eq!(state.players[0].life, 17);
    }

    /// CR 118.12a + CR 701.21: Unless-sacrifice costs are payer-relative.
    /// A parser-emitted `ControllerRef::You` filter must resolve against the
    /// player paying the cost, not against the ability controller or a chosen
    /// target player.
    #[test]
    fn unless_sacrifice_cost_uses_payer_relative_filter() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Payer Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let pending = ResolvedAbility::new(gain_life(4), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(1),
            cost: AbilityCost::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                count: 1,
            },
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let mut events = Vec::new();
        let waiting_for = state.waiting_for.clone();
        handle_unless_payment(&mut state, waiting_for, true, &mut events)
            .expect("unless-sacrifice should surface choice");
        match &state.waiting_for {
            WaitingFor::WardSacrificeChoice {
                player, permanents, ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(permanents, &vec![creature]);
            }
            other => panic!("expected WardSacrificeChoice, got {other:?}"),
        }
    }

    /// CR 118.12a: "unless any player pays" poll — when the prompted player
    /// declines and `remaining` is non-empty, the next player is prompted and
    /// the pending effect is NOT yet resolved. When the last player declines,
    /// the effect resolves exactly once.
    #[test]
    fn unless_pay_any_player_poll_advances_then_resolves_once() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[1].life = 20;
        let pending = ResolvedAbility::new(gain_life(4), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining: vec![PlayerId(1)],
        };

        // P0 declines → P1 is prompted, poll list drained, effect not resolved.
        let mut events = Vec::new();
        let wf = state.waiting_for.clone();
        handle_unless_payment(&mut state, wf, false, &mut events).expect("poll advance");
        match &state.waiting_for {
            WaitingFor::UnlessPayment {
                player, remaining, ..
            } => {
                assert_eq!(*player, PlayerId(1));
                assert!(remaining.is_empty());
            }
            other => panic!("expected UnlessPayment for P1, got {other:?}"),
        }
        assert_eq!(
            state.players[0].life, 20,
            "effect must not resolve mid-poll"
        );

        // P1 (last) declines → the effect resolves exactly once.
        let mut events = Vec::new();
        let wf = state.waiting_for.clone();
        handle_unless_payment(&mut state, wf, false, &mut events).expect("poll resolve");
        assert_eq!(
            state.players[0].life, 24,
            "GainLife(4) resolves exactly once"
        );
    }

    /// CR 118.12a: "unless any player pays" poll — a later player paying
    /// prevents the effect; earlier decliners do not stop the poll.
    #[test]
    fn unless_pay_any_player_poll_pay_prevents_effect() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        state.players[1].life = 20;
        let pending = ResolvedAbility::new(gain_life(4), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(1),
            cost: AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        // P1 pays 1 life → effect prevented; P0's life unchanged.
        let mut events = Vec::new();
        let wf = state.waiting_for.clone();
        handle_unless_payment(&mut state, wf, true, &mut events).expect("poll pay");
        assert_eq!(state.players[1].life, 19, "payer paid 1 life");
        assert_eq!(state.players[0].life, 20, "effect prevented by payment");
    }

    /// CR 118.12 + CR 107.14: Unless-PayEnergy stamps an `EnergyChanged`
    /// event and skips the pending effect when the payment succeeds.
    #[test]
    fn unless_pay_energy_deducts_and_skips_effect() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 5;
        let pending = ResolvedAbility::new(gain_life(2), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let mut events = Vec::new();
        let waiting_for = state.waiting_for.clone();
        handle_unless_payment(&mut state, waiting_for, true, &mut events)
            .expect("unless-pay-energy should resolve");
        assert_eq!(state.players[0].energy, 3);
        // Pending GainLife was skipped because payment succeeded — life unchanged.
        assert_eq!(state.players[0].life, 20);
    }

    /// CR 118.12a: **Runtime test** — choosing the PayLife branch of a
    /// disjunctive unless-cost re-enters the standard `handle_unless_payment`
    /// path, deducts life, and suppresses the pending effect. Drives the
    /// inner handler directly (not via `apply_action`); see the
    /// `unless_payment_choose_cost_via_apply_action_*` tests below for the
    /// public-surface contract.
    #[test]
    fn unless_payment_choose_cost_branch_zero_routes_to_chosen_cost() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let pending = ResolvedAbility::new(gain_life(7), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPaymentChooseCost {
            player: PlayerId(0),
            costs: vec![
                AbilityCost::PayLife {
                    amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                },
                AbilityCost::Discard {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    filter: None,
                    random: false,
                    self_ref: false,
                },
            ],
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining_choices: vec![],
            chosen: vec![],
        };

        let mut events = Vec::new();
        let waiting_for = state.waiting_for.clone();
        handle_unless_payment_choose_cost(
            &mut state,
            waiting_for,
            crate::types::actions::UnlessCostBranch::Pay { index: 0 },
            &mut events,
        )
        .expect("choose-cost dispatch should resolve");
        // PayLife branch was chosen and paid — life drops by 3, pending GainLife
        // was suppressed (post-fold the pending_effect's unless_pay is cleared
        // by surface_unless_payment, and the success path skips the effect).
        assert_eq!(state.players[0].life, 17);
    }

    /// CR 118.12a: **Runtime test** — declining all branches of a
    /// disjunctive unless-cost falls through to the effect happening,
    /// equivalent to `PayUnlessCost { pay: false }` on the single-cost
    /// path. Drives the inner handler directly; see the
    /// `unless_payment_choose_cost_via_apply_action_*` tests below for the
    /// public-surface contract.
    #[test]
    fn unless_payment_choose_cost_decline_runs_pending_effect() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let pending = ResolvedAbility::new(gain_life(7), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPaymentChooseCost {
            player: PlayerId(0),
            costs: vec![AbilityCost::PayLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            }],
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining_choices: vec![],
            chosen: vec![],
        };

        let mut events = Vec::new();
        let waiting_for = state.waiting_for.clone();
        handle_unless_payment_choose_cost(
            &mut state,
            waiting_for,
            crate::types::actions::UnlessCostBranch::Decline,
            &mut events,
        )
        .expect("choose-cost decline should resolve");
        // Effect happens: gain 7 life from 20 → 27.
        assert_eq!(state.players[0].life, 27);
    }

    /// CR 118.12a: **Public-surface test** — drives the choose-cost
    /// transition through `engine::apply` with a real `GameAction`. Exercises
    /// the dispatcher in `engine.rs` (the contract that actually ships) end-
    /// to-end, not just the inner handler.
    #[test]
    fn unless_payment_choose_cost_via_apply_action_pay_branch() {
        use crate::types::actions::{GameAction, UnlessCostBranch};
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let pending = ResolvedAbility::new(gain_life(7), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPaymentChooseCost {
            player: PlayerId(0),
            costs: vec![AbilityCost::PayLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            }],
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining_choices: vec![],
            chosen: vec![],
        };

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Pay { index: 0 },
            },
        )
        .expect("apply_action should resolve the choose-cost prompt");
        // PayLife branch paid → life 20 − 3 = 17, pending GainLife suppressed.
        assert_eq!(state.players[0].life, 17);
    }

    /// CR 118.12a: **Public-surface test** — declining via `engine::apply`
    /// runs the pending effect.
    #[test]
    fn unless_payment_choose_cost_via_apply_action_decline() {
        use crate::types::actions::{GameAction, UnlessCostBranch};
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let pending = ResolvedAbility::new(gain_life(7), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPaymentChooseCost {
            player: PlayerId(0),
            costs: vec![AbilityCost::PayLife {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            }],
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining_choices: vec![],
            chosen: vec![],
        };

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::ChooseUnlessCostBranch {
                choice: UnlessCostBranch::Decline,
            },
        )
        .expect("apply_action should resolve the decline");
        // Effect happens: 20 + 7 = 27.
        assert_eq!(state.players[0].life, 27);
    }

    /// CR 702.24a + CR 118.12: A `Composite`-of-`OneOf`s unless-cost (the
    /// shape `expand_per_counter` produces from a `OneOf` base at N ≥ 2 — e.g.
    /// Jötun Owl Keeper's `{W} or {U}` cumulative upkeep with 2 age counters)
    /// drives sequential disjunctive choices: each prompt resolves
    /// independently and picks accumulate into `chosen`. After the last
    /// prompt, the accumulated picks collapse into a `Composite` cost and the
    /// state transitions to `UnlessPayment` for the single combined payment.
    /// "Each choice is made separately for each age counter, then either the
    /// entire set of costs is paid, or none of them is paid."
    ///
    /// This test exercises **only the multi-choice routing** through
    /// `handle_unless_payment_choose_cost`. The single-cost
    /// `handle_unless_payment` handler's response to a `Composite` cost is
    /// out of scope here (covered by subsequent tasks); we cut the run
    /// short before that handler runs by inspecting `state.waiting_for`
    /// between the choose-cost handler and the unless-payment handler
    /// transition.
    #[test]
    fn unless_payment_composite_of_one_ofs_routes_through_sequential_choose() {
        // Two-prompt sequence: first prompt offers PayLife{3}/PayLife{1};
        // second prompt offers PayLife{2}/PayLife{5}. Distinct values per
        // prompt so the accumulated `chosen` list is unambiguous.
        let first_costs = vec![
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 },
            },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 1 },
            },
        ];
        let second_costs = vec![
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 5 },
            },
        ];

        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let pending = ResolvedAbility::new(gain_life(7), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPaymentChooseCost {
            player: PlayerId(0),
            costs: first_costs,
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining_choices: vec![second_costs.clone()],
            chosen: vec![],
        };

        // First pick: index 0 (PayLife{3}). Expected post-state: still in
        // UnlessPaymentChooseCost, now showing the second prompt;
        // remaining_choices drained to empty; chosen carries [PayLife{3}].
        let mut events = Vec::new();
        let wf = state.waiting_for.clone();
        handle_unless_payment_choose_cost(
            &mut state,
            wf,
            crate::types::actions::UnlessCostBranch::Pay { index: 0 },
            &mut events,
        )
        .expect("first choose-cost prompt should accumulate, not pay");
        match &state.waiting_for {
            WaitingFor::UnlessPaymentChooseCost {
                costs,
                remaining_choices,
                chosen,
                ..
            } => {
                assert_eq!(
                    costs, &second_costs,
                    "second prompt's costs are surfaced verbatim"
                );
                assert!(
                    remaining_choices.is_empty(),
                    "after popping the only queued prompt, remaining_choices is empty"
                );
                assert_eq!(chosen.len(), 1, "first pick accumulated into `chosen`");
                assert!(
                    matches!(
                        &chosen[0],
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 3 }
                        }
                    ),
                    "first pick is PayLife{{3}} as selected by index 0"
                );
            }
            other => panic!("expected second UnlessPaymentChooseCost prompt, got {other:?}"),
        }
        assert_eq!(
            state.players[0].life, 20,
            "no payment yet — picks accumulate until the final prompt"
        );

        // Second pick: index 1 (PayLife{5}). Expected post-state: the
        // multi-choice routing collapses into a `Composite` cost and
        // re-enters `handle_unless_payment` for the combined payment. The
        // single-cost handler's behavior with a Composite cost is out of
        // scope for this routing test; what matters is that the accumulated
        // picks formed the expected Composite before `handle_unless_payment`
        // was called.
        //
        // Drive the routing by hand (rather than via
        // `handle_unless_payment_choose_cost` which then re-enters
        // `handle_unless_payment`): pull out the final picks and assert the
        // shape of the would-be `UnlessPayment::cost`.
        let WaitingFor::UnlessPaymentChooseCost {
            costs,
            mut chosen,
            remaining_choices,
            ..
        } = state.waiting_for.clone()
        else {
            panic!("expected UnlessPaymentChooseCost before final pick");
        };
        assert!(remaining_choices.is_empty(), "queue is drained");
        chosen.push(costs[1].clone());
        let final_cost = if chosen.len() == 1 {
            chosen.into_iter().next().unwrap()
        } else {
            AbilityCost::Composite { costs: chosen }
        };
        match final_cost {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2, "two picks → 2-element Composite");
                assert!(matches!(
                    &costs[0],
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 3 }
                    }
                ));
                assert!(matches!(
                    &costs[1],
                    AbilityCost::PayLife {
                        amount: QuantityExpr::Fixed { value: 5 }
                    }
                ));
            }
            other => panic!("expected Composite[PayLife{{3}}, PayLife{{5}}], got {other:?}"),
        }
    }

    /// CR 702.24a + CR 118.12: End-to-end OneOf × N flow — driving both
    /// disjunctive picks through `handle_unless_payment_choose_cost`,
    /// collapsing the accumulated picks into a `Composite` of `Mana` costs,
    /// and paying the combined mana cost in a single `handle_unless_payment`
    /// step. Mirrors Jötun Owl Keeper's "{W} or {U}" cumulative-upkeep cost
    /// at N=2 age counters: 2 prompts, each picking `{W}` or `{U}`, summed
    /// into a single `{W}{U}` (or `{W}{W}`, etc.) payment. "Then either the
    /// entire set of costs is paid, or none of them is paid."
    ///
    /// Verifies the full Task 14 contract: pick → pick → pay succeeds, the
    /// unless-effect (would-be `GainLife`) does NOT happen (life unchanged),
    /// and the combined mana cost is deducted from the player's pool.
    #[test]
    fn unless_payment_composite_of_one_ofs_pays_combined_mana_e2e() {
        use crate::types::mana::{ManaType, ManaUnit};
        // Two-prompt sequence mirroring `OneOf{[Mana{W}, Mana{U}]}` expanded
        // to N=2 (the shape Jötun Owl Keeper produces at 2 age counters).
        let oneof_wu = vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![crate::types::mana::ManaCostShard::White],
                    generic: 0,
                },
            },
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![crate::types::mana::ManaCostShard::Blue],
                    generic: 0,
                },
            },
        ];

        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        // Provision {W}{W} so the payer can pay either combination of two
        // {W}-or-{U} picks if they choose {W} twice (the cheaper scenario
        // here just verifies the routing flow — picking {W} twice is the
        // simplest mana-pool model).
        for _ in 0..2 {
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::White,
                ObjectId(0),
                false,
                vec![],
            ));
        }

        let pending = ResolvedAbility::new(gain_life(7), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPaymentChooseCost {
            player: PlayerId(0),
            costs: oneof_wu.clone(),
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining_choices: vec![oneof_wu.clone()],
            chosen: vec![],
        };

        // First pick: {W} (index 0). State remains UnlessPaymentChooseCost.
        let mut events = Vec::new();
        let wf = state.waiting_for.clone();
        handle_unless_payment_choose_cost(
            &mut state,
            wf,
            crate::types::actions::UnlessCostBranch::Pay { index: 0 },
            &mut events,
        )
        .expect("first choose-cost prompt should accumulate, not pay");
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::UnlessPaymentChooseCost { .. }
            ),
            "intermediate state must remain UnlessPaymentChooseCost"
        );

        // Second pick: {W} (index 0) again. State transitions through
        // UnlessPayment{Composite[Mana{W}, Mana{W}]} → combined Mana{W}{W}
        // payment → success. Pending GainLife is suppressed.
        let mut events = Vec::new();
        let wf = state.waiting_for.clone();
        handle_unless_payment_choose_cost(
            &mut state,
            wf,
            crate::types::actions::UnlessCostBranch::Pay { index: 0 },
            &mut events,
        )
        .expect("final choose-cost prompt should pay the combined Composite-of-Mana");

        // Combined Mana{W}{W} drained the mana pool.
        let p0 = state.players.iter().find(|p| p.id == PlayerId(0)).unwrap();
        assert_eq!(
            p0.mana_pool.total(),
            0,
            "combined {{W}}{{W}} cost drains the two {{W}} units from the mana pool"
        );
        // Pending GainLife suppressed (CR 118.12: paying the unless-cost
        // means the effect does NOT happen).
        assert_eq!(
            p0.life, 20,
            "GainLife(7) suppressed because the combined unless-cost was paid"
        );
    }

    /// CR 118.12 (M1 fold + Harvest Wurm shape): An unless ReturnToHand cost
    /// with `from_zone: Some(Zone::Graveyard)` collects eligible cards from
    /// the graveyard zone (not battlefield).
    #[test]
    fn unless_return_to_hand_from_graveyard_collects_graveyard_cards() {
        use crate::game::zones::create_object;
        use crate::types::card_type::CardType;
        use crate::types::card_type::CoreType;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;
        let mut state = GameState::new_two_player(42);
        // Place a Land card in player 0's graveyard.
        let land_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Graveyard,
        );
        let land_types = CardType {
            core_types: vec![CoreType::Land],
            ..Default::default()
        };
        state.objects.get_mut(&land_id).unwrap().card_types = land_types;
        let pending = ResolvedAbility::new(gain_life(2), vec![], ObjectId(100), PlayerId(0));
        state.waiting_for = WaitingFor::UnlessPayment {
            player: PlayerId(0),
            cost: AbilityCost::ReturnToHand {
                count: 1,
                filter: None, // any card in the graveyard
                from_zone: Some(Zone::Graveyard),
            },
            pending_effect: Box::new(pending),
            trigger_event: None,
            effect_description: None,
            remaining: Vec::new(),
        };

        let mut events = Vec::new();
        let waiting_for = state.waiting_for.clone();
        handle_unless_payment(&mut state, waiting_for, true, &mut events)
            .expect("unless-return-to-hand should surface choice");
        match &state.waiting_for {
            WaitingFor::UnlessBounceChoice { permanents, .. } => {
                assert!(
                    permanents.contains(&land_id),
                    "graveyard card should be eligible, got {:?}",
                    permanents
                );
            }
            other => panic!("expected UnlessBounceChoice, got {:?}", other),
        }
    }

    /// CR 118.12 (M1 backward compat): An old `UnlessCost::PayLife { amount: 2 }`
    /// JSON shape deserializes as the new
    /// `AbilityCost::PayLife { amount: QuantityExpr::Fixed { value: 2 } }`.
    #[test]
    fn legacy_unless_cost_pay_life_deserializes_to_ability_cost() {
        use crate::types::ability::{deserialize_ability_cost_compat, AbilityCost, QuantityExpr};
        let json = r#"{"type":"PayLife","amount":2}"#;
        let mut de = serde_json::Deserializer::from_str(json);
        let cost: AbilityCost =
            deserialize_ability_cost_compat(&mut de).expect("legacy deserialize");
        assert_eq!(
            cost,
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 }
            }
        );
    }

    /// CR 118.12 (M1 backward compat): Legacy `UnlessCost::Fixed { cost: ... }`
    /// folds to `AbilityCost::Mana { cost: ... }`.
    #[test]
    fn legacy_unless_cost_fixed_deserializes_to_ability_cost_mana() {
        use crate::types::ability::{deserialize_ability_cost_compat, AbilityCost};
        use crate::types::mana::ManaCost;
        let json = r#"{"type":"Fixed","cost":{"type":"Cost","shards":[],"generic":3}}"#;
        let mut de = serde_json::Deserializer::from_str(json);
        let cost: AbilityCost =
            deserialize_ability_cost_compat(&mut de).expect("legacy Fixed deserialize");
        assert_eq!(
            cost,
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 3,
                }
            }
        );
    }

    /// CR 118.12 (M1 backward compat): Legacy `UnlessCost::Sacrifice` renames
    /// `filter` → `target` to match `AbilityCost::Sacrifice` shape.
    #[test]
    fn legacy_unless_cost_sacrifice_renames_filter_to_target() {
        use crate::types::ability::{deserialize_ability_cost_compat, AbilityCost, TargetFilter};
        let json = r#"{"type":"Sacrifice","count":2,"filter":{"type":"Any"}}"#;
        let mut de = serde_json::Deserializer::from_str(json);
        let cost: AbilityCost =
            deserialize_ability_cost_compat(&mut de).expect("legacy Sacrifice deserialize");
        assert_eq!(
            cost,
            AbilityCost::Sacrifice {
                target: TargetFilter::Any,
                count: 2,
            }
        );
    }
}
