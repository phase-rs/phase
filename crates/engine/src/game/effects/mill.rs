use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Mill target player: move top N cards from their library to their graveyard.
/// Reads `NumCards` param (default 1).
/// Target is resolved from ability.targets (first TargetRef::Player), or defaults to opponent of controller.
pub fn resolve(
    _state: &mut GameState,
    _ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    todo!("Mill effect not yet implemented")
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
        ResolvedAbility {
            api_type: "Mill".to_string(),
            params: HashMap::from([("NumCards".to_string(), num_cards.to_string())]),
            targets,
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn mill_3_moves_top_3_from_library_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(&mut state, CardId(i + 1), PlayerId(1), format!("Card {}", i), Zone::Library);
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
            create_object(&mut state, CardId(i + 1), PlayerId(1), format!("Card {}", i), Zone::Library);
        }

        let ability = make_mill_ability(5, vec![TargetRef::Player(PlayerId(1))]);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[1].library.is_empty());
        assert_eq!(state.players[1].graveyard.len(), 2);
    }
}
