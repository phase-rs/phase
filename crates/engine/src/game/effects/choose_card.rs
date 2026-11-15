use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 608.2d: Choose — player makes a choice from available options.
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
            .map(|p| state.graveyard_of(p.id).iter().copied().collect())
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
            .map(|p| state.library_of(p.id).iter().copied().collect())
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
            subject: None,
        });
        return Ok(());
    }

    let keep_count = change_num.min(cards.len());

    state.waiting_for = WaitingFor::DigChoice {
        player: ability.controller,
        library_owner: ability.controller,
        selectable_cards: cards.clone(),
        cards,
        keep_count,
        up_to: false,
        kept_destination: None,
        rest_destination: None,
        // CR 701.20e + CR 608.2c: choosing a card names a card and moves nothing
        // (`Effect::ChooseCard` has no zone move, and neither side of this choice
        // names a destination), so the cards that were not chosen stay exactly
        // where they were read from. `Route`'s `None` → graveyard default would
        // turn a card choice over the library, hand, exile or battlefield into a
        // mass mill/discard of every other candidate.
        rest_remainder: crate::types::ability::DigRestRemainder::StayInLibrary,
        rest_split_top_count: None,
        rest_order: crate::types::ability::DigRestOrder::Preserve,
        source_id: Some(ability.source_id),
        enter_tapped: false,
        enters_attacking: false,
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::actions::GameAction;
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

    /// CR 701.20e + CR 608.2c: `Effect::ChooseCard` names a card from a list and
    /// moves nothing. The candidates that were not chosen must therefore never be
    /// routed anywhere — under the `Route` graveyard default a bare card choice
    /// over the library, hand, exile or battlefield would mill/discard every other
    /// candidate.
    ///
    /// The selection is submitted through the production resolution-choice
    /// reducer, because the choice state is only half the claim: the FAILURE this
    /// test prevents happens when the answer is applied, and a prompt-only
    /// assertion would pass even if the unchosen candidate were milled.
    #[test]
    fn test_choose_card_never_routes_the_unselected_cards() {
        let mut state = GameState::new_two_player(42);
        let chosen = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );
        let unchosen = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Card B".to_string(),
            Zone::Library,
        );
        assert_eq!(
            state.players[0].library.iter().copied().collect::<Vec<_>>(),
            vec![chosen, unchosen]
        );

        let ability = ResolvedAbility::new(
            Effect::ChooseCard {
                choices: vec!["Library".to_string()],
                target: TargetFilter::Any,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let waiting_for = match &state.waiting_for {
            WaitingFor::DigChoice {
                cards,
                kept_destination,
                rest_destination,
                rest_remainder,
                ..
            } => {
                assert_eq!(*cards, vec![chosen, unchosen]);
                assert_eq!(*kept_destination, None);
                assert_eq!(*rest_destination, None);
                assert_eq!(
                    *rest_remainder,
                    crate::types::ability::DigRestRemainder::StayInLibrary,
                    "CR 701.20e: a card choice moves nothing, so the rest of the \
                     candidates must stay where they are"
                );
                state.waiting_for.clone()
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        };

        crate::game::engine_resolution_choices::handle_resolution_choice(
            &mut state,
            waiting_for,
            GameAction::SelectCards {
                cards: vec![chosen],
            },
            &mut events,
        )
        .expect("naming one of the offered cards must be accepted");

        // CR 701.20e: neither candidate changed zone. The chosen card is only
        // NAMED; the unchosen one must not be routed by the graveyard default.
        assert_eq!(state.objects[&chosen].zone, Zone::Library);
        assert_eq!(state.objects[&unchosen].zone, Zone::Library);
        assert!(
            state.players[0].library.iter().any(|id| *id == chosen),
            "the chosen card stays in the library"
        );
        assert!(
            state.players[0].library.iter().any(|id| *id == unchosen),
            "the unchosen card must stay in the library, got {:?}",
            state.players[0].library
        );
        assert!(
            state.players[0].graveyard.is_empty(),
            "a card choice moves nothing, got {:?}",
            state.players[0].graveyard
        );
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

#[cfg(test)]
mod dandan_read_sweep_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetFilter;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// CR 400.1: a "choose a card from your <zone>" prompt offers the shared
    /// pile's cards to the non-canonical seat.
    #[test]
    fn offers_the_shared_pile_to_the_non_canonical_seat() {
        for (zone, name) in [(Zone::Graveyard, "Graveyard"), (Zone::Library, "Library")] {
            let mut state = GameState::new(FormatConfig::dandan(), 2, 42);
            let card = create_object(&mut state, CardId(1), PlayerId(1), "Card".into(), zone);
            let ability = ResolvedAbility::new(
                Effect::ChooseCard {
                    choices: vec![name.to_string()],
                    target: TargetFilter::Any,
                },
                vec![],
                ObjectId(100),
                PlayerId(1),
            );

            resolve(&mut state, &ability, &mut Vec::new()).unwrap();

            let WaitingFor::DigChoice { cards, .. } = &state.waiting_for else {
                panic!("{name}: expected DigChoice, got {:?}", state.waiting_for);
            };
            assert_eq!(cards, &vec![card], "{name}");
        }
    }
}
