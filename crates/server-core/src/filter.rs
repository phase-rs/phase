use engine::game::players;
use engine::types::game_state::GameState;
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

    // Hide library contents for ALL players (no one should see card details in libraries)
    let all_library_ids: Vec<ObjectId> = filtered
        .players
        .iter()
        .flat_map(|p| p.library.iter().copied())
        .collect();
    for obj_id in all_library_ids {
        hide_card(&mut filtered, obj_id);
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
                casting_restrictions: vec![],
                casting_options: vec![],
                solve_condition: None,
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
}
