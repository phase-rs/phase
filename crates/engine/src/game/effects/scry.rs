use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Scry N: controller looks at top N cards of their library.
/// Simplified implementation: moves all scryed cards to the bottom of the library.
/// TODO: A proper implementation needs WaitingFor::ScryChoice for the player to choose
/// which cards go to top vs bottom and their ordering.
/// This simplified version is still useful for AI opponents and triggers scry-related events.
///
/// Reads `ScryNum` param (default 1).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let scry_num: usize = ability
        .params
        .get("ScryNum")
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    let count = scry_num.min(player.library.len());
    if count == 0 {
        events.push(GameEvent::EffectResolved {
            api_type: ability.api_type.clone(),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Collect the top N card IDs to move to bottom
    let cards_to_scry: Vec<_> = player.library[..count].to_vec();

    // Move each card to bottom of library using move_to_library_position
    for obj_id in cards_to_scry {
        zones::move_to_library_position(state, obj_id, false, events);
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
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_scry_ability(scry_num: u32) -> ResolvedAbility {
        ResolvedAbility {
            api_type: "Scry".to_string(),
            params: HashMap::from([("ScryNum".to_string(), scry_num.to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn scry_2_moves_top_2_to_bottom() {
        let mut state = GameState::new_two_player(42);
        // Create library: [Card0(top), Card1, Card2, Card3, Card4(bottom)]
        for i in 0..5 {
            create_object(&mut state, CardId(i + 1), PlayerId(0), format!("Card {}", i), Zone::Library);
        }
        let original_top_2: Vec<_> = state.players[0].library[..2].to_vec();
        let original_bottom_3: Vec<_> = state.players[0].library[2..].to_vec();

        let ability = make_scry_ability(2);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Library should still have 5 cards
        assert_eq!(state.players[0].library.len(), 5);
        // The original bottom 3 should now be at the top
        assert_eq!(&state.players[0].library[..3], &original_bottom_3[..]);
        // The original top 2 should now be at the bottom
        for id in &original_top_2 {
            assert!(state.players[0].library[3..].contains(id));
        }
    }

    #[test]
    fn scry_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_scry_ability(2);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
    }
}
