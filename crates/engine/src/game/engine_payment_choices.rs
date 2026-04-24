use crate::game::filter;
use crate::types::ability::{AbilityCondition, Effect, EffectKind, TargetRef, UnlessCost};
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, PendingContinuation, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

use super::casting;
use super::effects;
use super::engine::{
    handle_tap_land_for_mana, handle_untap_land_for_mana, resume_pending_continuation_if_priority,
    EngineError,
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

    if let Some(mut ability) = state.pending_optional_effect.take() {
        ability.optional = false;
        if accept {
            ability.context.optional_effect_performed = true;
            effects::resolve_ability_chain(state, &ability, events, 0)
                .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
        } else if let Some(ref sub) = ability.sub_ability {
            if matches!(sub.condition, Some(AbilityCondition::IfYouDo)) {
                if let Some(ref else_branch) = sub.else_ability {
                    let mut else_resolved = else_branch.as_ref().clone();
                    else_resolved.context = ability.context.clone();
                    effects::resolve_ability_chain(state, &else_resolved, events, 0)
                        .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
                }
            }
        }
    }

    resume_pending_continuation_if_priority(state, events)?;
    Ok(state.waiting_for.clone())
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
                effects::resolve_ability_chain(state, &ability, events, 0)
                    .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
            } else {
                if matches!(ability.effect, Effect::DealDamage { .. }) {
                    ability.targets = vec![TargetRef::Player(promptee)];
                }
                set_active_priority(state);
                effects::resolve_ability_chain(state, &ability, events, 0)
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
                if matches!(sub.condition, Some(AbilityCondition::IfAPlayerDoes)) {
                    if let Some(ref else_branch) = sub.else_ability {
                        let mut else_resolved = else_branch.as_ref().clone();
                        else_resolved.context = ability.context.clone();
                        effects::resolve_ability_chain(state, &else_resolved, events, 0)
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
        source_id, count, ..
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for tribute choice".to_string(),
        ));
    };

    if accept {
        effects::tribute::apply_paid(state, source_id, count, events);
    } else {
        effects::tribute::apply_declined(state, source_id);
    }

    // Return priority to the active player so the ETB triggered ability can see
    // the persisted TributeOutcome when its intervening-if condition is checked.
    set_active_priority(state);
    resume_pending_continuation_if_priority(state, events)?;
    Ok(action_result(events, state.waiting_for.clone()))
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
        ..
    } = waiting_for
    else {
        return Err(EngineError::InvalidAction(
            "Not waiting for unless payment".to_string(),
        ));
    };

    let mut payment_failed = !pay;
    if pay {
        match cost {
            UnlessCost::Fixed { cost: mana_cost } => {
                casting::pay_unless_cost(state, player, &mana_cost, events)?;
            }
            UnlessCost::DynamicGeneric { .. } => {
                unreachable!("DynamicGeneric should be resolved before payment");
            }
            UnlessCost::PayLife { amount } => {
                // CR 118.12 + CR 118.3 + CR 119.4 + CR 119.8: Unless-pay life
                // routes through the single-authority helper. An unpayable cost
                // (insufficient life, or CantLoseLife lock) causes the "unless"
                // branch to fall through to the effect still happening.
                let life_amount = u32::try_from(amount.max(0)).unwrap_or(0);
                match pay_life_as_cost(state, player, life_amount, events) {
                    PayLifeCostResult::Paid { .. } => {}
                    PayLifeCostResult::InsufficientLife | PayLifeCostResult::LockedCantLoseLife => {
                        payment_failed = true;
                    }
                }
            }
            UnlessCost::DiscardCard => {
                let hand_cards: Vec<ObjectId> = state
                    .players
                    .iter()
                    .find(|p| p.id == player)
                    .map(|p| p.hand.iter().copied().collect::<Vec<_>>())
                    .unwrap_or_default();
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
            UnlessCost::Sacrifice { count, ref filter } => {
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
        }

        if !payment_failed {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&pending_effect.effect),
                source_id: pending_effect.source_id,
            });
        }
    }

    if !pay || payment_failed {
        let mut ability = pending_effect.as_ref().clone();
        if let Effect::Counter {
            ref mut unless_payment,
            ..
        } = ability.effect
        {
            *unless_payment = None;
        }
        effects::resolve_ability_chain(state, &ability, events, 0)
            .map_err(|e| EngineError::InvalidAction(format!("{e:?}")))?;
    }

    if matches!(state.waiting_for, WaitingFor::UnlessPayment { .. }) {
        set_active_priority(state);
    }
    resume_pending_continuation_if_priority(state, events)?;
    Ok(action_result(events, state.waiting_for.clone()))
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
        effect_description,
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
        effect_description,
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
        effect_description,
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
        effect_description,
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
        effect_description,
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
            cost,
            pending_effect,
            effect_description,
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
