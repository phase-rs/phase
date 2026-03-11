use crate::types::ability::{effect_variant_name, Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// Dig/Reveal effect: reveal top N cards, player selects ChangeNum to keep (hand),
/// rest go to graveyard.
/// Sets WaitingFor::DigChoice so the player can select which cards to keep.
///
/// Reads `ChangeNum` param for how many to keep (put in hand).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let dig_num: usize = match &ability.effect {
        Effect::Dig { count, .. } => *count as usize,
        _ => ability
            .params
            .get("DigNum")
            .or_else(|| ability.params.get("NumCards"))
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
    };

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

    let cards: Vec<_> = player.library[..count].to_vec();
    let keep_count = change_num.min(cards.len());

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        cards,
        keep_count,
    };

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
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
        ResolvedAbility::from_raw(
            "Dig",
            HashMap::from([
                ("DigNum".to_string(), dig_num.to_string()),
                ("ChangeNum".to_string(), change_num.to_string()),
            ]),
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_dig_5_keep_2_sets_waiting_for_dig_choice() {
        let mut state = GameState::new_two_player(42);
        for i in 0..7 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_5: Vec<_> = state.players[0].library[..5].to_vec();

        let ability = make_dig_ability(5, 2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 5);
                assert_eq!(*cards, top_5);
                assert_eq!(*keep_count, 2);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_dig_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_dig_ability(3, 1);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
