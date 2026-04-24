use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.57a: Discover N — exile cards from the top of your library until
/// you exile a nonland card with mana value N or less. Cast it without paying
/// its mana cost or put it into your hand. Put the rest on the bottom of your
/// library in a random order.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let limit = match &ability.effect {
        Effect::Discover { mana_value_limit } => *mana_value_limit,
        _ => return Err(EffectError::InvalidParam("Expected Discover".to_string())),
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    // Collect library IDs (top to bottom)
    let library: Vec<ObjectId> = player.library.iter().copied().collect();
    let mut exiled_misses: Vec<ObjectId> = Vec::new();
    let mut hit_card: Option<ObjectId> = None;

    // CR 701.57a: Exile one at a time until hit or library exhausted
    for &card_id in &library {
        // Move to exile
        zones::move_to_zone(state, card_id, Zone::Exile, events);

        // Check if this is a nonland card with MV ≤ limit
        let is_hit = state.objects.get(&card_id).is_some_and(|obj| {
            let is_land = obj.card_types.core_types.contains(&CoreType::Land);
            let mv = obj.mana_cost.mana_value();
            !is_land && mv <= limit
        });

        if is_hit {
            hit_card = Some(card_id);
            break;
        } else {
            exiled_misses.push(card_id);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    match hit_card {
        Some(hit) => {
            // Player chooses: cast without paying or put to hand
            state.waiting_for = WaitingFor::DiscoverChoice {
                player: ability.controller,
                hit_card: hit,
                exiled_misses,
            };
        }
        None => {
            // CR 701.57a: No hit — put all exiled misses on bottom in random order
            shuffle_to_bottom(state, &exiled_misses, ability.controller, events);
        }
    }

    Ok(())
}

/// Put cards on the bottom of the player's library in random order.
fn shuffle_to_bottom(
    state: &mut GameState,
    cards: &[ObjectId],
    _player_id: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) {
    use rand::seq::SliceRandom;

    let mut shuffled = cards.to_vec();
    shuffled.shuffle(&mut state.rng);

    for &card_id in &shuffled {
        zones::move_to_library_position(state, card_id, false, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;
    use crate::types::mana::ManaCost;
    use crate::types::player::PlayerId;

    #[test]
    fn test_discover_finds_nonland_card() {
        let mut state = GameState::new_two_player(42);
        // Create library: land, land, nonland (MV 2)
        let land1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let land2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Mountain".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&creature).unwrap().mana_cost = ManaCost::generic(2);

        let ability = ResolvedAbility::new(
            Effect::Discover {
                mana_value_limit: 3,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should find the creature and set DiscoverChoice
        match &state.waiting_for {
            WaitingFor::DiscoverChoice {
                hit_card,
                exiled_misses,
                ..
            } => {
                assert_eq!(*hit_card, creature);
                assert_eq!(exiled_misses.len(), 2, "Should have 2 land misses");
            }
            other => panic!("Expected DiscoverChoice, got {:?}", other),
        }
    }
}
