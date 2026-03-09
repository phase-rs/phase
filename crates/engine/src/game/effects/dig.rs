use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Dig/Reveal effect: reveal top N cards, put ChangeNum in hand, rest to bottom of library.
/// Reads `DigNum` or `NumCards` param for how many to reveal.
/// Reads `ChangeNum` param for how many to keep (put in hand).
// TODO: Full implementation needs WaitingFor::DigChoice for player to select which cards to keep
pub fn resolve(
    _state: &mut GameState,
    _ability: &ResolvedAbility,
    _events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    Err(EffectError::Unregistered("Dig".to_string()))
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

    fn make_dig_ability(dig_num: u32, change_num: u32) -> ResolvedAbility {
        ResolvedAbility {
            api_type: "Dig".to_string(),
            params: HashMap::from([
                ("DigNum".to_string(), dig_num.to_string()),
                ("ChangeNum".to_string(), change_num.to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn dig_3_keep_1_moves_1_to_hand_rest_to_bottom() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let original_top_3: Vec<_> = state.players[0].library[..3].to_vec();

        let ability = make_dig_ability(3, 1);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // 1 card moved to hand
        assert_eq!(state.players[0].hand.len(), 1);
        // The card in hand should be one of the original top 3
        assert!(original_top_3.contains(&state.players[0].hand[0]));
        // Library should still have 4 cards (5 - 1 to hand, 2 to bottom)
        assert_eq!(state.players[0].library.len(), 4);
    }

    #[test]
    fn dig_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_dig_ability(3, 1);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(state.players[0].hand.is_empty());
    }
}
