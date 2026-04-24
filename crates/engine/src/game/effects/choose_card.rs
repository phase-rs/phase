use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 700.2: Choose — player makes a choice from available options.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Extract origin and change_num from choices or default
    let (origin_zone, change_num) = match &ability.effect {
        Effect::ChooseCard { choices, .. } => {
            // Choices can encode origin and count, e.g. ["Graveyard", "2"]
            let origin = choices.first().map(|s| s.as_str()).unwrap_or("Graveyard");
            let count: usize = choices.get(1).and_then(|v| v.parse().ok()).unwrap_or(1);
            (origin, count)
        }
        _ => ("Graveyard", 1),
    };

    // Collect cards from the specified zone belonging to the controller.
    let cards: Vec<_> = match origin_zone {
        "Graveyard" => state
            .players
            .iter()
            .find(|p| p.id == ability.controller)
            .map(|p| p.graveyard.iter().copied().collect())
            .unwrap_or_default(),
        "Hand" => state
            .players
            .iter()
            .find(|p| p.id == ability.controller)
            .map(|p| p.hand.iter().copied().collect())
            .unwrap_or_default(),
        "Library" => state
            .players
            .iter()
            .find(|p| p.id == ability.controller)
            .map(|p| p.library.iter().copied().collect())
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
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    let keep_count = change_num.min(cards.len());

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        selectable_cards: cards.clone(),
        cards,
        keep_count,
        up_to: false,
        kept_destination: None,
        rest_destination: None,
        source_id: Some(ability.source_id),
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

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

        let ability = ResolvedAbility::new(
            Effect::ChooseCard {
                choices: vec!["Graveyard".to_string()],
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
                ..
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

        let ability = ResolvedAbility::new(
            Effect::ChooseCard {
                choices: vec!["Graveyard".to_string()],
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
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

        let ability = ResolvedAbility::new(
            Effect::ChooseCard {
                choices: vec!["Graveyard".to_string(), "2".to_string()],
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
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
