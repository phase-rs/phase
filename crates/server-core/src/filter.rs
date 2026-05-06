use engine::game::filter_state_for_viewer;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

/// Returns a filtered copy of the game state for the given player.
/// Hides ALL opponents' hand contents and ALL players' library contents.
pub fn filter_state_for_player(state: &GameState, viewer: PlayerId) -> GameState {
    filter_state_for_viewer(state, viewer)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::game::deck_loading::DeckEntry;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;
    use proptest::prelude::*;

    fn setup_state() -> GameState {
        let mut state = GameState::new_two_player(42);

        // Add cards to player 0's hand
        let id0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&id0).unwrap().abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )]);

        // Add cards to player 1's hand
        let id1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&id1).unwrap().abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            },
        )]);

        // Add cards to libraries
        create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Island".to_string(),
            Zone::Library,
        );

        state
    }

    #[test]
    fn own_hand_is_fully_visible() {
        let state = setup_state();
        let filtered = filter_state_for_player(&state, PlayerId(0));

        let hand = &filtered.players[0].hand;
        assert_eq!(hand.len(), 1);
        let obj = filtered.objects.get(&hand[0]).unwrap();
        assert_eq!(obj.name, "Lightning Bolt");
        assert!(!obj.face_down);
    }

    #[test]
    fn opponent_hand_cards_are_hidden() {
        let state = setup_state();
        let filtered = filter_state_for_player(&state, PlayerId(0));

        let opp_hand = &filtered.players[1].hand;
        assert_eq!(opp_hand.len(), 1, "hand size preserved");
        let obj = filtered.objects.get(&opp_hand[0]).unwrap();
        assert_eq!(obj.name, "Hidden Card");
        assert!(obj.face_down);
        assert!(obj.abilities.is_empty());
    }

    #[test]
    fn library_contents_hidden_for_both() {
        let state = setup_state();
        let filtered = filter_state_for_player(&state, PlayerId(0));

        // Own library hidden
        let own_lib = &filtered.players[0].library;
        assert_eq!(own_lib.len(), 1);
        let obj = filtered.objects.get(&own_lib[0]).unwrap();
        assert_eq!(obj.name, "Hidden Card");

        // Opponent library hidden
        let opp_lib = &filtered.players[1].library;
        assert_eq!(opp_lib.len(), 1);
        let obj = filtered.objects.get(&opp_lib[0]).unwrap();
        assert_eq!(obj.name, "Hidden Card");
    }

    #[test]
    fn filter_preserves_hand_size() {
        let state = setup_state();
        let original_opp_hand_size = state.players[1].hand.len();
        let filtered = filter_state_for_player(&state, PlayerId(0));
        assert_eq!(filtered.players[1].hand.len(), original_opp_hand_size);
    }

    #[test]
    fn revealed_cards_remain_visible_in_opponent_hand() {
        let mut state = setup_state();
        let opp_hand = &state.players[1].hand;
        let revealed_id = opp_hand[0];

        // Mark the card as revealed
        state.revealed_cards.insert(revealed_id);

        let filtered = filter_state_for_player(&state, PlayerId(0));

        let obj = filtered.objects.get(&revealed_id).unwrap();
        assert_ne!(
            obj.name, "Hidden Card",
            "Revealed card should not be hidden"
        );
        assert!(!obj.face_down, "Revealed card should not be face_down");
    }

    #[test]
    fn redacts_opponent_deck_pool_details() {
        let mut state = setup_state();
        let entry = DeckEntry {
            card: CardFace {
                name: "Forest".to_string(),
                mana_cost: ManaCost::NoCost,
                card_type: CardType {
                    supertypes: vec![],
                    core_types: vec![engine::types::card_type::CoreType::Land],
                    subtypes: vec!["Forest".to_string()],
                },
                power: None,
                toughness: None,
                loyalty: None,
                defense: None,
                oracle_text: None,
                non_ability_text: None,
                flavor_name: None,
                keywords: vec![],
                abilities: vec![],
                triggers: vec![],
                static_abilities: vec![],
                replacements: vec![],
                color_override: None,
                color_identity: vec![],
                scryfall_oracle_id: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                solve_condition: None,
                parse_warnings: vec![],
                brawl_commander: false,
                metadata: Default::default(),
                rarities: Default::default(),
            },
            count: 4,
        };
        state.deck_pools = vec![
            engine::types::game_state::PlayerDeckPool {
                player: PlayerId(0),
                registered_main: Arc::new(vec![entry.clone()]),
                registered_sideboard: Arc::new(vec![entry.clone()]),
                current_main: Arc::new(vec![entry.clone()]),
                current_sideboard: Arc::new(vec![entry.clone()]),
                ..Default::default()
            },
            engine::types::game_state::PlayerDeckPool {
                player: PlayerId(1),
                registered_main: Arc::new(vec![entry.clone()]),
                registered_sideboard: Arc::new(vec![entry.clone()]),
                current_main: Arc::new(vec![entry.clone()]),
                current_sideboard: Arc::new(vec![entry]),
                ..Default::default()
            },
        ];

        let filtered = filter_state_for_player(&state, PlayerId(0));
        let own = filtered
            .deck_pools
            .iter()
            .find(|pool| pool.player == PlayerId(0))
            .unwrap();
        let opp = filtered
            .deck_pools
            .iter()
            .find(|pool| pool.player == PlayerId(1))
            .unwrap();
        assert!(!own.registered_main.is_empty());
        assert!(opp.registered_main.is_empty());
        assert!(opp.registered_sideboard.is_empty());
        assert!(opp.current_main.is_empty());
        assert!(opp.current_sideboard.is_empty());
    }

    #[test]
    fn manifest_dread_hides_card_ids_from_opponent() {
        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);
        // Add 2 cards to library
        let card_a = create_object(
            &mut state,
            CardId(10),
            p0,
            "Creature A".to_string(),
            Zone::Library,
        );
        let card_b = create_object(
            &mut state,
            CardId(11),
            p0,
            "Creature B".to_string(),
            Zone::Library,
        );

        // Set up ManifestDreadChoice state
        state.waiting_for = WaitingFor::ManifestDreadChoice {
            player: p0,
            cards: vec![card_a, card_b],
        };
        state.revealed_cards.insert(card_a);
        state.revealed_cards.insert(card_b);

        // Player 0 (manifesting player) should see the cards
        let filtered_p0 = filter_state_for_player(&state, p0);
        match &filtered_p0.waiting_for {
            WaitingFor::ManifestDreadChoice { cards, .. } => {
                assert_eq!(cards.len(), 2);
                assert_eq!(cards[0], card_a);
                assert_eq!(cards[1], card_b);
            }
            other => panic!("Expected ManifestDreadChoice, got {:?}", other),
        }
        // Cards should not be hidden for the manifesting player
        let obj_a = &filtered_p0.objects[&card_a];
        assert_eq!(obj_a.name, "Creature A");

        // Player 1 (opponent) should see redacted card IDs
        let filtered_p1 = filter_state_for_player(&state, PlayerId(1));
        match &filtered_p1.waiting_for {
            WaitingFor::ManifestDreadChoice { cards, .. } => {
                assert_eq!(cards.len(), 2);
                // Card IDs should be zeroed out for opponents
                assert_eq!(cards[0], engine::types::identifiers::ObjectId(0));
                assert_eq!(cards[1], engine::types::identifiers::ObjectId(0));
            }
            other => panic!("Expected ManifestDreadChoice, got {:?}", other),
        }
        // Library cards should be hidden for opponent
        let obj_a_opp = &filtered_p1.objects[&card_a];
        assert_eq!(obj_a_opp.name, "Hidden Card");
    }

    #[test]
    fn effect_zone_choice_from_hand_redacts_cards_for_opponent() {
        let mut state = GameState::new_two_player(42);
        let card_a = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        let card_b = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Island".to_string(),
            Zone::Hand,
        );

        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![card_a, card_b],
            count: 1,
            up_to: true,
            source_id: ObjectId(100),
            effect_kind: engine::types::ability::EffectKind::ChangeZone,
            zone: Zone::Hand,
            destination: Some(Zone::Battlefield),
            enter_tapped: false,
            enter_transformed: false,
            under_your_control: false,
            enters_attacking: false,
            owner_library: false,
        };

        let filtered = filter_state_for_player(&state, PlayerId(1));

        match filtered.waiting_for {
            WaitingFor::EffectZoneChoice { cards, .. } => {
                assert_eq!(cards, vec![ObjectId(0), ObjectId(0)]);
            }
            other => panic!("Expected EffectZoneChoice, got {:?}", other),
        }

        assert_eq!(filtered.objects[&card_a].name, "Hidden Card");
        assert_eq!(filtered.objects[&card_b].name, "Hidden Card");
    }

    proptest! {
        #![proptest_config(ProptestConfig {
            cases: 16,
            .. ProptestConfig::default()
        })]

        #[test]
        fn property_filter_hides_opponent_hidden_zones(
            opp_hand_count in 1usize..5,
            own_library_count in 1usize..5,
            opp_library_count in 1usize..5,
        ) {
            let mut state = GameState::new_two_player(42);

            for idx in 0..opp_hand_count {
                create_object(
                    &mut state,
                    CardId((100 + idx) as u64),
                    PlayerId(1),
                    format!("Opp Hand {idx}"),
                    Zone::Hand,
                );
            }

            for idx in 0..own_library_count {
                create_object(
                    &mut state,
                    CardId((200 + idx) as u64),
                    PlayerId(0),
                    format!("Own Library {idx}"),
                    Zone::Library,
                );
            }

            for idx in 0..opp_library_count {
                create_object(
                    &mut state,
                    CardId((300 + idx) as u64),
                    PlayerId(1),
                    format!("Opp Library {idx}"),
                    Zone::Library,
                );
            }

            let filtered = filter_state_for_player(&state, PlayerId(0));

            prop_assert_eq!(filtered.players[1].hand.len(), opp_hand_count);
            for obj_id in &filtered.players[1].hand {
                let obj = filtered.objects.get(obj_id).expect("hand object must exist");
                prop_assert!(obj.face_down);
                prop_assert_eq!(&obj.name, "Hidden Card");
                prop_assert!(obj.abilities.is_empty());
            }

            prop_assert_eq!(filtered.players[0].library.len(), own_library_count);
            for obj_id in &filtered.players[0].library {
                let obj = filtered.objects.get(obj_id).expect("own library object must exist");
                prop_assert_eq!(&obj.name, "Hidden Card");
                prop_assert!(obj.face_down);
            }

            prop_assert_eq!(filtered.players[1].library.len(), opp_library_count);
            for obj_id in &filtered.players[1].library {
                let obj = filtered.objects.get(obj_id).expect("opponent library object must exist");
                prop_assert_eq!(&obj.name, "Hidden Card");
                prop_assert!(obj.face_down);
            }
        }
    }
}
