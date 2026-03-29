use engine::game::players;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

/// Returns a filtered copy of the game state for the given player.
/// Hides ALL opponents' hand contents and ALL players' library contents.
pub fn filter_state_for_player(state: &GameState, viewer: PlayerId) -> GameState {
    let mut filtered = state.clone();

    // Hide hand card details for ALL opponents (not just one)
    let opponents = players::opponents(state, viewer);
    let opp_hand_ids: Vec<ObjectId> = opponents
        .iter()
        .flat_map(|&opp| filtered.players[opp.0 as usize].hand.iter().copied())
        .collect();
    for obj_id in opp_hand_ids {
        if !state.revealed_cards.contains(&obj_id) {
            hide_card(&mut filtered, obj_id);
        }
    }

    // CR 701.62a: During ManifestDreadChoice, the manifesting player can see the revealed
    // library cards. Opponents cannot see any library card identities.
    let (manifest_dread_visible, manifest_dread_cards): (
        std::collections::HashSet<ObjectId>,
        std::collections::HashSet<ObjectId>,
    ) = if let WaitingFor::ManifestDreadChoice { player, ref cards } = filtered.waiting_for {
        let all_cards: std::collections::HashSet<ObjectId> = cards.iter().copied().collect();
        if player == viewer {
            (all_cards.clone(), all_cards)
        } else {
            // Opponent: hide the card IDs in the WaitingFor itself
            (std::collections::HashSet::new(), all_cards)
        }
    } else {
        (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        )
    };

    // CR 701.20e: During DigChoice, the looking player can see the looked-at
    // library cards. Opponents cannot see any library card identities.
    let (dig_visible, dig_cards): (
        std::collections::HashSet<ObjectId>,
        std::collections::HashSet<ObjectId>,
    ) = if let WaitingFor::DigChoice {
        player, ref cards, ..
    } = filtered.waiting_for
    {
        let all_cards: std::collections::HashSet<ObjectId> = cards.iter().copied().collect();
        if player == viewer {
            (all_cards.clone(), all_cards)
        } else {
            (std::collections::HashSet::new(), all_cards)
        }
    } else {
        (
            std::collections::HashSet::new(),
            std::collections::HashSet::new(),
        )
    };

    // Hide library contents for ALL players (no one should see card details in libraries)
    let all_library_ids: Vec<ObjectId> = filtered
        .players
        .iter()
        .flat_map(|p| p.library.iter().copied())
        .collect();
    for obj_id in all_library_ids {
        // CR 701.62a: Don't hide cards visible to the manifesting player
        // CR 701.20e: Don't hide cards visible to the looking player.
        // CR 701.20b: Don't hide cards currently revealed (e.g. Goblin Guide trigger),
        // but ManifestDread/Dig cards use their own visibility path — not generic revealed_cards
        let visible = manifest_dread_visible.contains(&obj_id)
            || dig_visible.contains(&obj_id)
            || (state.revealed_cards.contains(&obj_id)
                && !manifest_dread_cards.contains(&obj_id)
                && !dig_cards.contains(&obj_id));
        if !visible {
            hide_card(&mut filtered, obj_id);
        }
    }

    // CR 701.62a: For opponents during ManifestDreadChoice, redact the card list
    // so they can't see which specific cards were revealed.
    if let WaitingFor::ManifestDreadChoice { player, ref cards } = state.waiting_for {
        if player != viewer {
            filtered.waiting_for = WaitingFor::ManifestDreadChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
            };
        }
    }

    // CR 701.20e: For opponents during DigChoice, redact the card list
    // so they can't see which specific cards were looked at.
    if let WaitingFor::DigChoice {
        player,
        ref cards,
        keep_count,
        up_to,
        ref selectable_cards,
        kept_destination,
        rest_destination,
        source_id,
    } = state.waiting_for
    {
        if player != viewer {
            filtered.waiting_for = WaitingFor::DigChoice {
                player,
                cards: cards.iter().map(|_| ObjectId(0)).collect(),
                keep_count,
                up_to,
                selectable_cards: selectable_cards.iter().map(|_| ObjectId(0)).collect(),
                kept_destination,
                rest_destination,
                source_id,
            };
        }
    }

    // CR 701.48a: During LearnChoice, opponents shouldn't see which cards are in hand.
    if let WaitingFor::LearnChoice {
        player,
        ref hand_cards,
    } = state.waiting_for
    {
        if player != viewer {
            filtered.waiting_for = WaitingFor::LearnChoice {
                player,
                hand_cards: hand_cards.iter().map(|_| ObjectId(0)).collect(),
            };
        }
    }

    // Only show the viewer's own auto-pass flag
    filtered.auto_pass.retain(|pid, _| *pid == viewer);

    // Only show the viewer's own manual mana-tap tracking
    filtered
        .lands_tapped_for_mana
        .retain(|pid, _| *pid == viewer);

    // Hide pending cast info from opponents (contains full spell data)
    if filtered.pending_cast.is_some() && filtered.waiting_for.acting_player() != Some(viewer) {
        filtered.pending_cast = None;
    }

    for pool in &mut filtered.deck_pools {
        if pool.player != viewer {
            pool.registered_main.clear();
            pool.registered_sideboard.clear();
            pool.current_main.clear();
            pool.current_sideboard.clear();
        }
    }

    filtered
}

/// Zero out all identifying information on a card object.
fn hide_card(state: &mut GameState, obj_id: ObjectId) {
    if let Some(obj) = state.objects.get_mut(&obj_id) {
        obj.face_down = true;
        obj.name = "Hidden Card".to_string();
        obj.abilities.clear();
        obj.keywords.clear();
        obj.base_keywords.clear();
        obj.power = None;
        obj.toughness = None;
        obj.loyalty = None;
        obj.color.clear();
        obj.base_color.clear();
        obj.trigger_definitions.clear();
        obj.replacement_definitions.clear();
        obj.static_definitions.clear();
        obj.casting_permissions.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::deck_loading::DeckEntry;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::CardType;
    use engine::types::identifiers::CardId;
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
        state.objects.get_mut(&id0).unwrap().abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )];

        // Add cards to player 1's hand
        let id1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&id1).unwrap().abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Counter {
                target: TargetFilter::Any,
                source_static: None,
                unless_payment: None,
            },
        )];

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
                scryfall_oracle_id: None,
                modal: None,
                additional_cost: None,
                strive_cost: None,
                casting_restrictions: vec![],
                casting_options: vec![],
                solve_condition: None,
                brawl_commander: false,
                metadata: Default::default(),
            },
            count: 4,
        };
        state.deck_pools = vec![
            engine::types::game_state::PlayerDeckPool {
                player: PlayerId(0),
                registered_main: vec![entry.clone()],
                registered_sideboard: vec![entry.clone()],
                current_main: vec![entry.clone()],
                current_sideboard: vec![entry.clone()],
            },
            engine::types::game_state::PlayerDeckPool {
                player: PlayerId(1),
                registered_main: vec![entry.clone()],
                registered_sideboard: vec![entry.clone()],
                current_main: vec![entry.clone()],
                current_sideboard: vec![entry],
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
