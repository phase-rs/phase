use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// Scry N: controller looks at top N cards of their library.
/// Sets WaitingFor::ScryChoice so the player can choose which cards
/// go to top vs bottom. The engine processes the SelectCards response
/// in engine.rs to reorder the library.
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

    // Collect the top N card IDs for the player to choose from
    let cards: Vec<_> = player.library[..count].to_vec();

    state.waiting_for = WaitingFor::ScryChoice {
        player: ability.controller,
        cards,
    };

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
        ResolvedAbility::from_raw(
            "Scry",
            HashMap::from([("ScryNum".to_string(), scry_num.to_string())]),
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_scry_2_sets_waiting_for_scry_choice() {
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
        let top_2: Vec<_> = state.players[0].library[..2].to_vec();

        let ability = make_scry_ability(2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ScryChoice { player, cards } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 2);
                assert_eq!(*cards, top_2);
            }
            other => panic!("Expected ScryChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_scry_1_single_card_still_requires_choice() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card 0".to_string(),
            Zone::Library,
        );

        let ability = make_scry_ability(1);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ScryChoice { player, cards } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 1);
            }
            other => panic!("Expected ScryChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_scry_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_scry_ability(2);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        // Should NOT set ScryChoice when library is empty
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
