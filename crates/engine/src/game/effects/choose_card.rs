use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// ChooseCard: generic "choose a card" effect that presents cards from a zone
/// for the player to select from.
///
/// Reads `Origin` param for the zone to choose from (default: Graveyard).
/// Reads `ChangeNum` param for how many cards to choose (default: 1).
/// Sets WaitingFor::DigChoice for the player to make their selection.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let origin = ability
        .params
        .get("Origin")
        .map(|s| s.as_str())
        .unwrap_or("Graveyard");

    let change_num: usize = ability
        .params
        .get("ChangeNum")
        .map(|v| v.parse().unwrap_or(1))
        .unwrap_or(1);

    // Collect cards from the specified zone belonging to the controller
    let cards: Vec<_> = match origin {
        "Graveyard" => state
            .players
            .iter()
            .find(|p| p.id == ability.controller)
            .map(|p| p.graveyard.clone())
            .unwrap_or_default(),
        "Hand" => state
            .players
            .iter()
            .find(|p| p.id == ability.controller)
            .map(|p| p.hand.clone())
            .unwrap_or_default(),
        "Library" => state
            .players
            .iter()
            .find(|p| p.id == ability.controller)
            .map(|p| p.library.clone())
            .unwrap_or_default(),
        "Exile" => state
            .exile
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|obj| obj.owner == ability.controller)
                    .unwrap_or(false)
            })
            .copied()
            .collect(),
        "Battlefield" => state
            .battlefield
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|obj| obj.controller == ability.controller)
                    .unwrap_or(false)
            })
            .copied()
            .collect(),
        _ => Vec::new(),
    };

    if cards.is_empty() {
        events.push(GameEvent::EffectResolved {
            api_type: ability.api_type.clone(),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let keep_count = change_num.min(cards.len());

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        cards,
        keep_count,
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

    #[test]
    fn test_choose_card_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let card1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Graveyard,
        );
        let card2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility {
            api_type: "ChooseCard".to_string(),
            params: HashMap::from([("Origin".to_string(), "Graveyard".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 2);
                assert!(cards.contains(&card1));
                assert!(cards.contains(&card2));
                assert_eq!(*keep_count, 1);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_choose_card_empty_zone_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].graveyard.is_empty());

        let ability = ResolvedAbility {
            api_type: "ChooseCard".to_string(),
            params: HashMap::from([("Origin".to_string(), "Graveyard".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should not set DigChoice with empty zone
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }

    #[test]
    fn test_choose_card_with_change_num() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Graveyard,
            );
        }

        let ability = ResolvedAbility {
            api_type: "ChooseCard".to_string(),
            params: HashMap::from([
                ("Origin".to_string(), "Graveyard".to_string()),
                ("ChangeNum".to_string(), "2".to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice { keep_count, .. } => {
                assert_eq!(*keep_count, 2);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }
}
