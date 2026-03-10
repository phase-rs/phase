use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Mill target player: move top N cards from their library to their graveyard.
/// Reads `NumCards` param (default 1).
/// Target is resolved from ability.targets (first TargetRef::Player), or defaults to opponent of controller.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_cards: usize = ability
        .params
        .get("NumCards")
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    // Find target player: first TargetRef::Player, or opponent of controller
    let target_player = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Player(pid) = t {
                Some(*pid)
            } else {
                None
            }
        })
        .unwrap_or({
            // Default to opponent: if controller is 0, target is 1, and vice versa
            PlayerId(if ability.controller.0 == 0 { 1 } else { 0 })
        });

    let player = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .ok_or(EffectError::PlayerNotFound)?;

    // Collect the top N card IDs (or fewer if library is smaller)
    let count = num_cards.min(player.library.len());
    let cards_to_mill: Vec<_> = player.library[..count].to_vec();

    // Move each card from library to graveyard
    for obj_id in cards_to_mill {
        zones::move_to_zone(state, obj_id, Zone::Graveyard, events);
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type.clone(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_mill_ability(num_cards: u32, targets: Vec<TargetRef>) -> ResolvedAbility {
        ResolvedAbility::from_raw(
            "Mill",
            HashMap::from([("NumCards".to_string(), num_cards.to_string())]),
            targets,
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn mill_3_moves_top_3_from_library_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_3: Vec<_> = state.players[1].library[..3].to_vec();

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[1].library.len(), 2);
        assert_eq!(state.players[1].graveyard.len(), 3);
        for id in &top_3 {
            assert!(state.players[1].graveyard.contains(id));
        }
    }

    #[test]
    fn mill_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[1].library.is_empty());

        let ability = make_mill_ability(3, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.players[1].graveyard.is_empty());
    }

    #[test]
    fn mill_with_fewer_cards_than_requested_mills_available() {
        let mut state = GameState::new_two_player(42);
        for i in 0..2 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(1),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let ability = make_mill_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].library.is_empty());
        assert_eq!(state.players[1].graveyard.len(), 2);
    }
}
