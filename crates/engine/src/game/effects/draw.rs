use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// Draw cards for the ability's controller.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let num_cards = match &ability.effect {
        Effect::Draw { count } => *count,
        _ => ability
            .params
            .get("NumCards")
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
    };

    let proposed = ProposedEvent::Draw {
        player_id: ability.controller,
        count: num_cards,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if let ProposedEvent::Draw {
                player_id, count, ..
            } = event
            {
                let player = state
                    .players
                    .iter()
                    .find(|p| p.id == player_id)
                    .ok_or(EffectError::PlayerNotFound)?;

                let cards_to_draw: Vec<_> = player
                    .library
                    .iter()
                    .take(count as usize)
                    .copied()
                    .collect();

                for obj_id in cards_to_draw {
                    zones::move_to_zone(state, obj_id, Zone::Hand, events);
                    events.push(GameEvent::CardDrawn {
                        player_id,
                        object_id: obj_id,
                    });
                }
            }
        }
        ReplacementResult::Prevented => {
            // Draw was prevented, skip
        }
        ReplacementResult::NeedsChoice(player) => {
            let candidate_count = state
                .pending_replacement
                .as_ref()
                .map(|p| p.candidates.len())
                .unwrap_or(0);
            state.waiting_for = crate::types::game_state::WaitingFor::ReplacementChoice {
                player,
                candidate_count,
            };
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: ability.api_type().to_string(),
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
    use std::collections::HashMap;

    fn make_ability(num_cards: u32) -> ResolvedAbility {
        ResolvedAbility::from_raw(
            "Draw",
            HashMap::from([("NumCards".to_string(), num_cards.to_string())]),
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn draw_moves_top_card_to_hand() {
        let mut state = GameState::new_two_player(42);
        let card_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(1);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&card_id));
        assert!(!state.players[0].library.contains(&card_id));
    }

    #[test]
    fn draw_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let c1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let c2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        let ability = make_ability(2);
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.players[0].hand.contains(&c1));
        assert!(state.players[0].hand.contains(&c2));
    }

    #[test]
    fn draw_emits_card_drawn_and_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Library,
        );
        let mut events = Vec::new();

        resolve(&mut state, &make_ability(1), &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::CardDrawn { .. })));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::EffectResolved { api_type, .. } if api_type == "Draw")
        ));
    }
}
