use crate::game::zones;
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// Dig/Reveal effect: reveal top N cards, put ChangeNum in hand, rest to bottom of library.
/// Reads `DigNum` or `NumCards` param for how many to reveal.
/// Reads `ChangeNum` param for how many to keep (put in hand).
// TODO: Full implementation needs WaitingFor::DigChoice for player to select which cards to keep
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let dig_num: usize = ability
        .params
        .get("DigNum")
        .or_else(|| ability.params.get("NumCards"))
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    let change_num: usize = ability
        .params
        .get("ChangeNum")
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    let count = dig_num.min(player.library.len());
    if count == 0 {
        return Ok(());
    }

    let revealed: Vec<_> = player.library[..count].to_vec();

    // Simplified: first ChangeNum cards go to hand, rest to bottom of library
    let keep_count = change_num.min(revealed.len());
    let to_hand = &revealed[..keep_count];
    let to_bottom = &revealed[keep_count..];

    for &obj_id in to_hand {
        zones::move_to_zone(state, obj_id, Zone::Hand, events);
    }

    // Move rest to bottom of library (they're already removed by move_to_zone above
    // if they were moved; the remaining ones are still in library at original positions).
    // We need to move them to the bottom: remove then re-add at end.
    for &obj_id in to_bottom {
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == ability.controller)
            .unwrap();
        if let Some(pos) = player.library.iter().position(|&id| id == obj_id) {
            player.library.remove(pos);
            player.library.push(obj_id);
        }
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
