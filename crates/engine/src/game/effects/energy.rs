use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 122.1: Gain energy counters. Increments the controller's energy pool.
pub fn resolve_gain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount = match &ability.effect {
        Effect::GainEnergy { amount } => resolve_quantity_with_targets(state, amount, ability),
        _ => return Err(EffectError::MissingParam("amount".to_string())),
    };
    let amount = amount.max(0) as u32;

    // CR 122.1: Energy counters are a kind of counter that a player may have.
    let player = &mut state.players[ability.controller.0 as usize];
    player.energy += amount;

    // CR 122.1 + CR 107.14: Energy counters are counters placed on a player.
    events.push(GameEvent::EnergyChanged {
        player: ability.controller,
        delta: amount as i32,
    });
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GainEnergy,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::types::ability::{
        ControllerRef, QuantityExpr, QuantityRef, TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn gain_energy_ability(amount: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainEnergy { amount },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn gain_energy_resolves_fixed_amount() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = gain_energy_ability(QuantityExpr::Fixed { value: 2 });

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 2);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EnergyChanged {
                player: PlayerId(0),
                delta: 2,
            }
        )));
    }

    #[test]
    fn gain_energy_resolves_dynamic_object_count() {
        let mut state = GameState::new_two_player(42);
        for idx in 0..2 {
            let id = zones::create_object(
                &mut state,
                CardId(idx),
                PlayerId(0),
                "Creature".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        let opponent_id = zones::create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = gain_energy_ability(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        });
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 2);
    }
}
