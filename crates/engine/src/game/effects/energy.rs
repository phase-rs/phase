use std::collections::HashSet;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterAddition, PendingEffectResolved};
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CounterPlacement, ProposedEvent};

pub(crate) fn add_energy_with_replacement(
    state: &mut GameState,
    actor: PlayerId,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    if amount == 0 {
        return true;
    }

    // CR 122.1 + CR 107.14 + CR 614.17: Energy is a counter a player has, so
    // gaining energy passes through the counter-placement pipeline before the
    // dedicated player energy field is mutated.
    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Energy { actor, player_id },
        count: amount,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(ProposedEvent::AddCounter {
            placement: CounterPlacement::Energy { player_id, .. },
            count,
            ..
        }) => {
            apply_energy_addition(state, player_id, count, events);
            true
        }
        ReplacementResult::Execute(_) | ReplacementResult::Prevented => true,
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            false
        }
    }
}

pub(crate) fn apply_energy_addition(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) {
    if amount == 0 {
        return;
    }

    let player = &mut state.players[player_id.0 as usize];
    player.energy += amount;

    // CR 122.1 + CR 107.14: Energy counters are counters placed on a player.
    events.push(GameEvent::EnergyChanged {
        player: player_id,
        delta: amount as i32,
    });
}

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

    if !add_energy_with_replacement(
        state,
        ability.controller,
        ability.controller,
        amount,
        events,
    ) {
        super::counters::stash_pending_counter_additions(
            state,
            Vec::<PendingCounterAddition>::new(),
            PendingEffectResolved::new(EffectKind::GainEnergy, ability.source_id),
        );
        return Ok(());
    }
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
        ControllerRef, QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition,
        ReplacementPlayerScope, TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
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

    #[test]
    fn gain_energy_is_prevented_by_player_counter_prohibition() {
        let mut state = GameState::new_two_player(42);
        let source = zones::create_object(
            &mut state,
            CardId(88),
            PlayerId(1),
            "Solemnity".to_string(),
            Zone::Battlefield,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        replacement.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        let ability = gain_energy_ability(QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::EnergyChanged { .. })),
            "prevented energy-counter additions must not emit energy-change events"
        );
    }

    #[test]
    fn gain_energy_count_is_increased_by_controller_scoped_replacement() {
        let mut state = GameState::new_two_player(42);
        let source = zones::create_object(
            &mut state,
            CardId(88),
            PlayerId(0),
            "Izzet Generatorium".to_string(),
            Zone::Battlefield,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Plus { value: 1 });
        replacement.valid_player = Some(ReplacementPlayerScope::You);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        let ability = gain_energy_ability(QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 3);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EnergyChanged {
                player: PlayerId(0),
                delta: 3,
            }
        )));
    }
}
