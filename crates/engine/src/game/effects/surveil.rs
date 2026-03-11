use crate::types::ability::{effect_variant_name, Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// Surveil N: controller looks at top N cards of their library.
/// Sets WaitingFor::SurveilChoice so the player can choose which cards
/// go to graveyard vs stay on top of library.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let surveil_num: usize = match &ability.effect {
        Effect::Surveil { count } => *count as usize,
        _ => 1,
    };

    let player = state
        .players
        .iter()
        .find(|p| p.id == ability.controller)
        .ok_or(EffectError::PlayerNotFound)?;

    let count = surveil_num.min(player.library.len());
    if count == 0 {
        events.push(GameEvent::EffectResolved {
            api_type: effect_variant_name(&ability.effect).to_string(),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let cards: Vec<_> = player.library[..count].to_vec();

    state.waiting_for = WaitingFor::SurveilChoice {
        player: ability.controller,
        cards,
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

    fn make_surveil_ability(surveil_num: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Surveil { count: surveil_num },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_surveil_2_sets_waiting_for_surveil_choice() {
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

        let ability = make_surveil_ability(2);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::SurveilChoice { player, cards } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 2);
                assert_eq!(*cards, top_2);
            }
            other => panic!("Expected SurveilChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_surveil_with_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_surveil_ability(2);
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
