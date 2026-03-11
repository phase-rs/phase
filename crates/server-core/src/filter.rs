use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

/// Returns a filtered copy of the game state for the given player.
/// Hides opponent hand contents and library order/contents for both players.
pub fn filter_state_for_player(state: &GameState, player: PlayerId) -> GameState {
    let mut filtered = state.clone();
    let opponent = PlayerId(1 - player.0);

    // Hide opponent's hand card details
    let opponent_hand: Vec<_> = filtered.players[opponent.0 as usize].hand.clone();
    for &obj_id in &opponent_hand {
        if let Some(obj) = filtered.objects.get_mut(&obj_id) {
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

    // Hide library contents for both players (opponent should never see card details)
    for p in &filtered.players {
        let lib: Vec<_> = p.library.clone();
        for &obj_id in &lib {
            if let Some(obj) = filtered.objects.get_mut(&obj_id) {
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
    }

    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, DamageAmount, Effect, TargetFilter,
    };
    use engine::types::identifiers::CardId;
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
        state.objects.get_mut(&id0).unwrap().abilities = vec![AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        }];

        // Add cards to player 1's hand
        let id1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&id1).unwrap().abilities = vec![AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::Counter {
                target: TargetFilter::Any,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        }];

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
}
