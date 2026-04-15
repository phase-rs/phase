use crate::game::casting;
use crate::game::life_costs::{pay_life_as_cost, PayLifeCostResult};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::speed::{effective_speed, set_speed};
use crate::types::ability::{Effect, PaymentCost};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

use super::{EffectError, ResolvedAbility};

/// CR 118.1: Pay a cost as part of an effect resolution.
/// CR 118.2: Paying life is not loss of life — replacement effects do not apply.
/// CR 117.1: Mana payment uses auto-tap + pool deduction.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let cost = match &ability.effect {
        Effect::PayCost { cost } => cost,
        _ => return Err(EffectError::MissingParam("PayCost".to_string())),
    };

    match cost {
        PaymentCost::Mana { cost: mana_cost } => {
            // CR 117.1: Pre-check affordability on a cloned state to avoid
            // partial mutations (auto_tap_lands runs before the can_pay check
            // inside pay_mana_cost). Only commit if the player can pay.
            if !casting::can_pay_cost_after_auto_tap(
                state,
                ability.controller,
                ability.source_id,
                mana_cost,
            ) {
                state.cost_payment_failed_flag = true;
                return Ok(());
            }
            // Payment is affordable — commit the mutation.
            let _ = casting::pay_unless_cost(state, ability.controller, mana_cost, events);
        }
        PaymentCost::Life { amount } => {
            // CR 118.3b + CR 119.4 + CR 119.8: Paying life as an effect-embedded
            // cost routes through the single-authority helper. Per CR 119.4 this
            // IS a life-loss event, so the replacement pipeline fires and a
            // CantLoseLife lock blocks the payment (cost unpayable).
            match pay_life_as_cost(state, ability.controller, *amount, events) {
                PayLifeCostResult::Paid { .. } => {}
                PayLifeCostResult::InsufficientLife | PayLifeCostResult::LockedCantLoseLife => {
                    state.cost_payment_failed_flag = true;
                }
            }
        }
        PaymentCost::Speed { amount } => {
            let amount = resolve_quantity_with_targets(state, amount, ability);
            let amount = u8::try_from(amount.max(0)).unwrap_or(u8::MAX);
            let current_speed = effective_speed(state, ability.controller);
            if amount <= current_speed {
                set_speed(
                    state,
                    ability.controller,
                    Some(current_speed - amount),
                    events,
                );
            } else {
                state.cost_payment_failed_flag = true;
            }
        }
        // CR 107.14: A player can pay {E} only if they have enough energy counters.
        PaymentCost::Energy { amount } => {
            let amount = resolve_quantity_with_targets(state, amount, ability);
            let amount = u32::try_from(amount.max(0)).unwrap_or(0);
            let can_pay = state
                .players
                .iter()
                .find(|p| p.id == ability.controller)
                .is_some_and(|p| p.energy >= amount);
            if can_pay {
                if let Some(p) = state
                    .players
                    .iter_mut()
                    .find(|p| p.id == ability.controller)
                {
                    p.energy -= amount;
                    events.push(GameEvent::EnergyChanged {
                        player: ability.controller,
                        delta: -(amount as i32),
                    });
                }
            } else {
                state.cost_payment_failed_flag = true;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
            cost: PaymentCost::Life { amount: 3 },
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
            cost: PaymentCost::Life { amount: 3 },
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
        });
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.cost_payment_failed_flag);
        assert_eq!(state.players[0].energy, 1); // No change
    }

    /// CR 119.8: An `Effect::PayCost { Life }` under CantLoseLife is unpayable —
    /// `cost_payment_failed_flag` is set and life total does not change.
    #[test]
    fn life_payment_blocked_by_cant_lose_life() {
        use crate::game::zones::create_object;
        use crate::types::ability::{
            ControllerRef, StaticDefinition, TargetFilter, TypedFilter,
        };
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
            cost: PaymentCost::Life { amount: 3 },
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
