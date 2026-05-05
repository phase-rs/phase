use crate::game::life_costs::{pay_life_as_cost, PayLifeCostResult};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::speed::{effective_speed, set_speed};
use crate::game::targeting::resolve_effect_player_ref;
use crate::game::{casting, casting_costs};
use crate::types::ability::{
    AbilityCost, Effect, EffectKind, PaymentCost, QuantityExpr, QuantityRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PayableResource, WaitingFor};
use crate::types::mana::{ManaCost, ManaCostShard};
use crate::types::player::PlayerId;

use super::{EffectError, ResolvedAbility};

/// CR 107.1c + CR 107.14: Detect a "pay any amount of X" shape — the parser
/// emits `QuantityExpr::Ref { QuantityRef::Variable { name: "X" } }` for
/// these prompts (Galvanic Discharge, etc.). A fixed or dynamic reference
/// (e.g., `Fixed { 2 }` or `EventContextSourcePower`) is paid unconditionally.
fn is_pay_any_amount(amount: &QuantityExpr) -> bool {
    matches!(
        amount,
        QuantityExpr::Ref {
            qty: QuantityRef::Variable { name },
        } if name == "X"
    )
}

/// CR 118.1: Pay a cost as part of an effect resolution.
/// CR 117.1: Mana payment uses auto-tap + pool deduction.
/// CR 119.4: Paying life IS losing life — replacement effects and the
/// CantLoseLife lock both apply, routed via `life_costs::pay_life_as_cost`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (cost, payer_filter) = match &ability.effect {
        Effect::PayCost { cost, payer } => (cost, payer),
        _ => return Err(EffectError::MissingParam("PayCost".to_string())),
    };
    let Some(payer) = resolve_effect_player_ref(state, ability, payer_filter) else {
        state.cost_payment_failed_flag = true;
        return Ok(());
    };
    let mut payment_ability = ability.clone();
    payment_ability.controller = payer;

    match cost {
        PaymentCost::Mana { cost: mana_cost } => {
            if payment_ability.chosen_x.is_none() && casting_costs::cost_has_x(mana_cost) {
                let per_x = mana_x_shard_count(mana_cost);
                let max = casting_costs::max_x_value(state, payer, mana_cost);
                let max = trigger_event_amount(state).map_or(max, |amount| max.min(amount));
                state.waiting_for = WaitingFor::PayAmountChoice {
                    player: payer,
                    resource: PayableResource::ManaGeneric { per_x },
                    min: 0,
                    max,
                    source_id: ability.source_id,
                };
                return Ok(());
            }
            // CR 117.1: Pre-check affordability on a cloned state to avoid
            // partial mutations (auto_tap_lands runs before the can_pay check
            // inside pay_mana_cost). Only commit if the player can pay.
            if !casting::can_pay_cost_after_auto_tap(state, payer, ability.source_id, mana_cost) {
                state.cost_payment_failed_flag = true;
                return Ok(());
            }
            // Payment is affordable — commit the mutation.
            let _ = casting::pay_unless_cost(state, payer, mana_cost, events);
        }
        PaymentCost::Life { amount } => {
            // CR 118.8 + CR 119.4 + CR 119.8: Paying life as an effect-embedded
            // cost routes through the single-authority helper. Per CR 119.4 this
            // IS a life-loss event, so the replacement pipeline fires and a
            // CantLoseLife lock blocks the payment (cost unpayable). The amount
            // is a `QuantityExpr` resolved here — dynamic refs like
            // `EventContextSourcePower` resolve against the triggering event.
            let amount = resolve_quantity_with_targets(state, amount, &payment_ability);
            let amount = u32::try_from(amount.max(0)).unwrap_or(0);
            match pay_life_as_cost(state, payer, amount, events) {
                PayLifeCostResult::Paid { .. } => {}
                PayLifeCostResult::InsufficientLife | PayLifeCostResult::LockedCantLoseLife => {
                    state.cost_payment_failed_flag = true;
                }
            }
        }
        PaymentCost::Speed { amount } => {
            let amount = resolve_quantity_with_targets(state, amount, &payment_ability);
            let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
            let current_speed = effective_speed(state, payer);
            if amount <= current_speed {
                set_speed(state, payer, Some(current_speed - amount), events);
            } else {
                state.cost_payment_failed_flag = true;
            }
        }
        // CR 107.14: A player can pay {E} only if they have enough energy counters.
        PaymentCost::Energy { amount } => {
            // CR 107.1c + CR 107.14: "Pay any amount of {E}" — suspend the chain
            // and surface a `PayAmountChoice` prompt. The sub-ability continuation
            // machinery in `effects::mod` stashes the remainder of the chain;
            // when the player submits the chosen amount (see
            // `engine_resolution_choices::handle_resolution_choice`), the engine
            // deducts energy, records the paid amount on `last_effect_count`
            // (the fallback for `QuantityRef::EventContextAmount`), and drains
            // the continuation so the subsequent "that much damage" effect
            // reads the player's chosen value.
            if is_pay_any_amount(amount) {
                let max = state
                    .players
                    .iter()
                    .find(|p| p.id == payer)
                    .map(|p| p.energy)
                    .unwrap_or(0);
                state.waiting_for = WaitingFor::PayAmountChoice {
                    player: payer,
                    resource: PayableResource::Energy,
                    min: 0,
                    max,
                    source_id: ability.source_id,
                };
                return Ok(());
            }
            let amount = resolve_quantity_with_targets(state, amount, &payment_ability);
            let amount = u32::try_from(amount.max(0)).unwrap_or(0);
            let can_pay = state
                .players
                .iter()
                .find(|p| p.id == payer)
                .is_some_and(|p| p.energy >= amount);
            if can_pay {
                if let Some(p) = state.players.iter_mut().find(|p| p.id == payer) {
                    p.energy -= amount;
                    events.push(GameEvent::EnergyChanged {
                        player: payer,
                        delta: -(amount as i32),
                    });
                }
                // CR 107.1c: Record the paid amount for downstream chain steps
                // that reference `QuantityRef::EventContextAmount` (e.g.
                // "that much damage"). Uses the same fallback slot populated
                // for "pay any amount of X" so fixed and variable pays are
                // observationally uniform downstream.
                state.last_effect_count = Some(amount as i32);
            } else {
                state.cost_payment_failed_flag = true;
            }
        }
        PaymentCost::AbilityCost { cost } => {
            resolve_ability_cost_payment(state, &payment_ability, payer, cost, events)?;
        }
    }
    Ok(())
}

fn resolve_ability_cost_payment(
    state: &mut GameState,
    ability: &ResolvedAbility,
    payer: PlayerId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    match cost {
        AbilityCost::Discard {
            count,
            filter,
            random: false,
            self_ref: false,
        } => {
            let count = resolve_quantity_with_targets(state, count, ability).max(0) as usize;
            let eligible = casting::find_eligible_discard_targets(
                state,
                payer,
                ability.source_id,
                filter.as_ref(),
            );
            if eligible.len() < count {
                state.cost_payment_failed_flag = true;
                return Ok(());
            }
            if count == 0 {
                state.last_effect_count = Some(0);
                return Ok(());
            }
            if eligible.len() == count {
                for card_id in eligible {
                    if let super::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
                        super::discard::discard_as_cost(state, card_id, payer, events)
                    {
                        state.waiting_for =
                            crate::game::replacement::replacement_choice_waiting_for(
                                choice_player,
                                state,
                            );
                        return Ok(());
                    }
                }
                state.last_effect_count = Some(count as i32);
            } else {
                state.waiting_for = WaitingFor::DiscardChoice {
                    player: payer,
                    count,
                    cards: eligible,
                    source_id: ability.source_id,
                    effect_kind: EffectKind::PayCost,
                    up_to: false,
                    unless_filter: None,
                };
            }
        }
        unsupported => {
            return Err(EffectError::InvalidParam(format!(
                "unsupported resolution-time AbilityCost payment: {unsupported:?}"
            )));
        }
    }
    Ok(())
}

fn mana_x_shard_count(cost: &ManaCost) -> u32 {
    match cost {
        ManaCost::Cost { shards, .. } => shards
            .iter()
            .filter(|shard| matches!(shard, ManaCostShard::X))
            .count() as u32,
        ManaCost::NoCost | ManaCost::SelfManaCost => 0,
    }
}

fn trigger_event_amount(state: &GameState) -> Option<u32> {
    state
        .current_trigger_event
        .as_ref()
        .and_then(crate::game::targeting::extract_amount_from_event)
        .and_then(|amount| u32::try_from(amount.max(0)).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TargetFilter;
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::{ManaCost, ManaType, ManaUnit};
    use crate::types::player::PlayerId;

    fn make_ability(effect: Effect) -> ResolvedAbility {
        ResolvedAbility::new(effect, vec![], ObjectId(1), PlayerId(0))
    }

    #[test]
    fn mana_payment_deducts_from_pool() {
        let mut state = GameState::new_two_player(42);
        // Give player 0 three colorless mana
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                snow: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Mana { cost },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(!state.cost_payment_failed_flag);
    }

    #[test]
    fn mana_payment_fails_when_insufficient() {
        let mut state = GameState::new_two_player(42);
        // Player 0 has empty mana pool (default)
        let cost = ManaCost::Cost {
            shards: vec![],
            generic: 2,
        };
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Mana { cost },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
    }

    #[test]
    fn life_payment_deducts_life() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 20;
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Life {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 17);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::LifeChanged { player_id, amount }
                if *player_id == PlayerId(0) && *amount == -3
        )));
    }

    #[test]
    fn life_payment_fails_when_insufficient() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 2;
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Life {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 2); // No change
    }

    #[test]
    fn energy_payment_deducts_energy() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 3;
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Energy {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 2 },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(!state.cost_payment_failed_flag);
        assert_eq!(state.players[0].energy, 1);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EnergyChanged { player, delta }
                if *player == PlayerId(0) && *delta == -2
        )));
    }

    #[test]
    fn energy_payment_fails_when_insufficient() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 1;
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Energy {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 2 },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].energy, 1); // No change
    }

    #[test]
    fn ability_cost_discard_payment_enters_discard_choice() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let first = create_object(&mut state, CardId(10), PlayerId(0), "A".into(), Zone::Hand);
        let second = create_object(&mut state, CardId(11), PlayerId(0), "B".into(), Zone::Hand);
        let ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: PaymentCost::AbilityCost {
                    cost: AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: None,
                        random: false,
                        self_ref: false,
                    },
                },
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DiscardChoice {
                player,
                count,
                cards,
                effect_kind,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert_eq!(*effect_kind, crate::types::ability::EffectKind::PayCost);
                assert!(cards.contains(&first));
                assert!(cards.contains(&second));
            }
            other => panic!("expected DiscardChoice, got {other:?}"),
        }
    }

    #[test]
    fn ability_cost_discard_choice_drains_continuation() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let first = create_object(&mut state, CardId(10), PlayerId(0), "A".into(), Zone::Hand);
        create_object(&mut state, CardId(11), PlayerId(0), "B".into(), Zone::Hand);
        state.players[0].life = 20;

        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: crate::types::ability::GainLifePlayer::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        let mut pay_ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: PaymentCost::AbilityCost {
                    cost: AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: None,
                        random: false,
                        self_ref: false,
                    },
                },
                payer: TargetFilter::Controller,
            },
            vec![],
            ObjectId(500),
            PlayerId(0),
        );
        pay_ability.sub_ability = Some(Box::new(gain_life));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay_ability, &mut events, 0).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::DiscardChoice { .. }
        ));

        let waiting_for = state.waiting_for.clone();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SelectCards { cards: vec![first] },
            &mut events,
        )
        .unwrap();

        assert!(matches!(
            outcome,
            ResolutionChoiceOutcome::WaitingFor(_) | ResolutionChoiceOutcome::ActionResult(_)
        ));
        assert_eq!(state.players[0].life, 23);
        assert_eq!(state.last_effect_count, Some(1));
    }

    /// CR 107.14: A fixed-amount energy payment stamps `last_effect_count`
    /// so downstream chain steps like "deals that much damage" can read the
    /// paid value through `QuantityRef::EventContextAmount`.
    #[test]
    fn energy_payment_stamps_last_effect_count() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 5;
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Energy {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(state.last_effect_count, Some(3));
    }

    /// CR 107.1c + CR 107.14: "Pay any amount of {E}" — the resolver pauses
    /// on a `PayAmountChoice` prompt with `max` bounded by the player's
    /// current energy. No energy is deducted until `SubmitPayAmount` fires.
    #[test]
    fn pay_any_amount_of_energy_pauses_for_choice() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 3;
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Energy {
                amount: crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice {
                player,
                resource,
                min,
                max,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*resource, PayableResource::Energy);
                assert_eq!(*min, 0);
                assert_eq!(*max, 3);
            }
            other => panic!("expected PayAmountChoice, got {:?}", other),
        }
        assert_eq!(
            state.players[0].energy, 3,
            "energy must not be deducted yet"
        );
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::EnergyChanged { .. })),
            "no EnergyChanged event until the player commits an amount"
        );
    }

    /// CR 107.1c + CR 107.14 + CR 603.7c: Galvanic Discharge chain shape —
    /// GainEnergy(3) → PayCost{Energy, Variable X} → DealDamage{EventContextAmount}.
    /// The player picks 2 out of 3 energy; damage equals the chosen amount.
    #[test]
    fn pay_any_amount_then_deal_that_much_damage_flow() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::game::zones::create_object;
        use crate::types::ability::{QuantityExpr, QuantityRef, TargetFilter, TargetRef};
        use crate::types::actions::GameAction;
        use crate::types::identifiers::CardId;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        // Target creature owned by player 1.
        let target_id = create_object(
            &mut state,
            CardId(777),
            PlayerId(1),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&target_id).unwrap().toughness = Some(2);
        state.objects.get_mut(&target_id).unwrap().power = Some(2);

        // Player 0 starts with 3 energy (after a prior GainEnergy step in the chain).
        state.players[0].energy = 3;

        // PayCost { Energy, Variable("X") } followed by DealDamage { EventContextAmount, target }.
        let damage = ResolvedAbility::new(
            Effect::DealDamage {
                damage_source: None,
                target: TargetFilter::Any,
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(500),
            PlayerId(0),
        );
        let mut pay_ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: PaymentCost::Energy {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                },
                payer: TargetFilter::Controller,
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(500),
            PlayerId(0),
        );
        pay_ability.sub_ability = Some(Box::new(damage));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay_ability, &mut events, 0).unwrap();

        // Chain paused on PayAmountChoice.
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { max, .. } => assert_eq!(*max, 3),
            other => panic!("expected PayAmountChoice, got {:?}", other),
        }

        // Player commits 2.
        let wf = state.waiting_for.clone();
        let outcome = handle_resolution_choice(
            &mut state,
            wf,
            GameAction::SubmitPayAmount { amount: 2 },
            &mut events,
        )
        .unwrap();
        match outcome {
            ResolutionChoiceOutcome::WaitingFor(_) => {}
            ResolutionChoiceOutcome::ActionResult(_) => {}
        }

        assert_eq!(state.players[0].energy, 1, "two energy consumed");
        assert_eq!(
            state.objects.get(&target_id).map(|o| o.damage_marked),
            Some(2),
            "Galvanic Discharge dealt 2 damage (the chosen amount)"
        );
    }

    #[test]
    fn pay_x_mana_from_life_gain_trigger_draws_chosen_x_cards() {
        use crate::game::effects::resolve_ability_chain;
        use crate::game::engine_resolution_choices::{
            handle_resolution_choice, ResolutionChoiceOutcome,
        };
        use crate::game::zones::create_object;
        use crate::types::actions::GameAction;
        use crate::types::events::GameEvent;
        use crate::types::identifiers::CardId;
        use crate::types::mana::ManaCostShard;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Well of Lost Dreams".to_string(),
            Zone::Battlefield,
        );
        for n in 0..5 {
            create_object(
                &mut state,
                CardId(100 + n),
                PlayerId(0),
                format!("Card {n}"),
                Zone::Library,
            );
        }
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                snow: false,
                restrictions: vec![],
                grants: vec![],
                expiry: None,
            });
        }
        state.current_trigger_event = Some(GameEvent::LifeChanged {
            player_id: PlayerId(0),
            amount: 3,
        });

        let draw = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        let mut pay = ResolvedAbility::new(
            Effect::PayCost {
                cost: PaymentCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![ManaCostShard::X],
                        generic: 0,
                    },
                },
                payer: TargetFilter::Controller,
            },
            vec![],
            source_id,
            PlayerId(0),
        );
        pay.sub_ability = Some(Box::new(draw));

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &pay, &mut events, 0).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { resource, max, .. } => {
                assert_eq!(*resource, PayableResource::ManaGeneric { per_x: 1 });
                assert_eq!(*max, 3);
            }
            other => panic!("expected PayAmountChoice, got {other:?}"),
        }

        let waiting_for = state.waiting_for.clone();
        let outcome = handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SubmitPayAmount { amount: 2 },
            &mut events,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            ResolutionChoiceOutcome::WaitingFor(_) | ResolutionChoiceOutcome::ActionResult(_)
        ));
        assert_eq!(state.players[0].hand.len(), 2);
        assert_eq!(state.players[0].mana_pool.mana.len(), 1);
    }

    /// CR 107.1c: "Pay any amount" with zero energy still pauses with
    /// `max = 0` — the player can only pick 0 (the "may" branch).
    #[test]
    fn pay_any_amount_with_zero_energy_max_is_zero() {
        let mut state = GameState::new_two_player(42);
        state.players[0].energy = 0;
        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Energy {
                amount: crate::types::ability::QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::PayAmountChoice { max, .. } => assert_eq!(*max, 0),
            other => panic!("expected PayAmountChoice, got {:?}", other),
        }
    }

    /// CR 119.8: An `Effect::PayCost { Life }` under CantLoseLife is unpayable —
    /// `cost_payment_failed_flag` is set and life total does not change.
    #[test]
    fn life_payment_blocked_by_cant_lose_life() {
        use crate::game::zones::create_object;
        use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
        use crate::types::identifiers::CardId;
        use crate::types::statics::StaticMode;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );

        let ability = make_ability(Effect::PayCost {
            cost: PaymentCost::Life {
                amount: crate::types::ability::QuantityExpr::Fixed { value: 3 },
            },
            payer: TargetFilter::Controller,
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);

        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].life, 20, "life total must not change");
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::LifeChanged { .. })),
            "no LifeChanged event should be emitted"
        );
    }
}
