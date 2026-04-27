use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

/// CR 701.62a: Manifest dread — look at top 2 cards of library, manifest one,
/// put the rest into graveyard.
///
/// Sets WaitingFor::ManifestDreadChoice so the player can select which card to manifest.
/// If fewer than 2 cards are available, uses what's there. If 0 cards, does nothing.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let player = ability.controller;

    let player_state = state
        .players
        .iter()
        .find(|p| p.id == player)
        .ok_or(EffectError::PlayerNotFound)?;

    let count = player_state.library.len().min(2);
    if count == 0 {
        // CR 701.62a: Nothing to manifest if library is empty
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // CR 701.62a: Look at top 2 (or fewer) cards
    let cards: Vec<_> = player_state
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    if count == 1 {
        // Only one card — must manifest it (no choice needed)
        let card_id = cards[0];
        crate::game::morph::manifest_card(state, player, card_id, events)
            .map_err(|e| EffectError::MissingParam(format!("{e}")))?;
    } else {
        // CR 701.62a: Player chooses which card to manifest
        // Mark these cards as revealed to the controller only
        for &card_id in &cards {
            state.revealed_cards.insert(card_id);
        }

        state.waiting_for = WaitingFor::ManifestDreadChoice { player, cards };
    }

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
    use crate::types::ability::Effect;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_manifest_dread_ability() -> ResolvedAbility {
        ResolvedAbility::new(Effect::ManifestDread, vec![], ObjectId(100), PlayerId(0))
    }

    #[test]
    fn manifest_dread_sets_waiting_for_with_top_2() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(i + 1),
                player,
                format!("Card {}", i),
                Zone::Library,
            );
        }
        let top_2: Vec<_> = state.players[0]
            .library
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>();

        let ability = make_manifest_dread_ability();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ManifestDreadChoice { player: p, cards } => {
                assert_eq!(*p, player);
                assert_eq!(cards.len(), 2);
                assert_eq!(*cards, top_2);
            }
            other => panic!("Expected ManifestDreadChoice, got {:?}", other),
        }
        // Cards should be revealed to controller
        for &card_id in &top_2 {
            assert!(state.revealed_cards.contains(&card_id));
        }
    }

    #[test]
    fn manifest_dread_single_card_auto_manifests() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let id = create_object(
            &mut state,
            CardId(1),
            player,
            "Solo Card".to_string(),
            Zone::Library,
        );

        let ability = make_manifest_dread_ability();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should auto-manifest without waiting (only 1 card, no choice)
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority, got {:?}",
            state.waiting_for
        );
        let obj = &state.objects[&id];
        assert!(obj.face_down);
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
    }

    #[test]
    fn manifest_dread_engine_roundtrip_manifests_selected_graves_other() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        let card_a = create_object(
            &mut state,
            CardId(1),
            player,
            "Card A".to_string(),
            Zone::Library,
        );
        let card_b = create_object(
            &mut state,
            CardId(2),
            player,
            "Card B".to_string(),
            Zone::Library,
        );
        let top_2: Vec<_> = state.players[0]
            .library
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>();
        let selected = top_2[0]; // manifest the first
        let graved = top_2[1]; // graveyard the second

        // Resolve to set WaitingFor
        let ability = make_manifest_dread_ability();
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Submit selection via engine
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;
        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![selected],
            },
        );
        assert!(result.is_ok(), "apply failed: {:?}", result.err());

        // Verify selected card is manifested face-down on battlefield
        let obj_selected = &state.objects[&selected];
        assert!(obj_selected.face_down);
        assert_eq!(obj_selected.zone, Zone::Battlefield);
        assert_eq!(obj_selected.power, Some(2));
        assert_eq!(obj_selected.toughness, Some(2));

        // Verify other card went to graveyard
        let obj_graved = &state.objects[&graved];
        assert_eq!(obj_graved.zone, Zone::Graveyard);

        // Revealed cards should be cleared
        assert!(!state.revealed_cards.contains(&selected));
        assert!(!state.revealed_cards.contains(&graved));

        let _ = (card_a, card_b);
    }

    #[test]
    fn manifest_dread_continuation_executes_after_choice() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);
        // Create 2 cards in library
        create_object(
            &mut state,
            CardId(1),
            player,
            "Card A".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(2),
            player,
            "Card B".to_string(),
            Zone::Library,
        );
        let top_2: Vec<_> = state.players[0]
            .library
            .iter()
            .take(2)
            .copied()
            .collect::<Vec<_>>();
        let selected = top_2[0];

        // Create ability with sub_ability (draw a card)
        let mut ability = make_manifest_dread_ability();
        let draw_sub = ResolvedAbility::new(
            Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Controller,
                up_to: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(draw_sub));

        // Add a card to library for the draw to find
        create_object(
            &mut state,
            CardId(3),
            player,
            "Draw Target".to_string(),
            Zone::Library,
        );

        let hand_before = state.players[0].hand.len();

        // Resolve chain — should pause at ManifestDreadChoice with continuation saved
        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();
        assert!(
            matches!(state.waiting_for, WaitingFor::ManifestDreadChoice { .. }),
            "Expected ManifestDreadChoice, got {:?}",
            state.waiting_for
        );
        assert!(state.pending_continuation.is_some());

        // Submit selection
        use crate::game::engine::apply_as_current;
        use crate::types::actions::GameAction;
        let result = apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![selected],
            },
        );
        assert!(result.is_ok(), "apply failed: {:?}", result.err());

        // Verify the continuation (draw) executed
        let hand_after = state.players[0].hand.len();
        assert_eq!(
            hand_after,
            hand_before + 1,
            "Continuation draw should have added 1 card to hand"
        );
    }

    #[test]
    fn manifest_dread_empty_library_does_nothing() {
        let mut state = GameState::new_two_player(42);
        assert!(state.players[0].library.is_empty());

        let ability = make_manifest_dread_ability();
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
