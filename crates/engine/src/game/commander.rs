use std::collections::HashMap;

use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// CR 903.8: Commander tax — {2} additional cost per previous cast from command zone.
pub fn commander_tax(state: &GameState, commander_id: ObjectId) -> u32 {
    state
        .commander_cast_count
        .get(&commander_id)
        .copied()
        .unwrap_or(0)
        * 2
}

/// CR 408.3 + CR 903.8: Record that a commander was cast from the command zone, incrementing its cast count.
pub fn record_commander_cast(state: &mut GameState, commander_id: ObjectId) {
    *state.commander_cast_count.entry(commander_id).or_insert(0) += 1;
}

/// CR 903.9a + CR 408.1: Commander owner may put it into the command zone instead of graveyard or exile.
/// CR 408.1: The command zone is reserved for specialized objects that have an overarching effect on the game.
///
/// Returns true if an object is a commander and its destination is Graveyard or Exile,
/// meaning it should be redirected to the command zone instead.
pub fn should_redirect_to_command_zone(
    state: &GameState,
    object_id: ObjectId,
    destination: Zone,
) -> bool {
    // Only redirect commanders
    let obj = match state.objects.get(&object_id) {
        Some(obj) => obj,
        None => return false,
    };

    if !obj.is_commander {
        return false;
    }

    // Only redirect when going to graveyard or exile
    matches!(destination, Zone::Graveyard | Zone::Exile)
}

/// CR 903.4: Each card must be within the commander's color identity.
///
/// Color identity includes colors from mana cost symbols (CR 903.4) plus the card's
/// color indicator / color-defining ability. Rules-text mana symbols (e.g., Alesha's
/// {W/B} activated ability) are not yet parsed into structured data — that is a
/// separate, larger undertaking (CR 903.4d).
///
/// Returns true if the cast is legal under color identity rules.
pub fn can_cast_in_color_identity(
    state: &GameState,
    card_colors: &[ManaColor],
    card_mana_cost: &ManaCost,
    player: PlayerId,
) -> bool {
    use super::printed_cards::derive_colors_from_mana_cost;

    // CR 903.4: Commander's color identity = color + mana cost colors.
    let mut commander_identity: Vec<ManaColor> = Vec::new();
    for obj in state
        .objects
        .values()
        .filter(|obj| obj.is_commander && obj.owner == player)
    {
        for &c in &obj.color {
            if !commander_identity.contains(&c) {
                commander_identity.push(c);
            }
        }
        for c in derive_colors_from_mana_cost(&obj.mana_cost) {
            if !commander_identity.contains(&c) {
                commander_identity.push(c);
            }
        }
    }

    // If no commander found (non-Commander format), allow everything
    if commander_identity.is_empty() {
        return true;
    }

    // CR 903.4: Card's color identity = color + mana cost colors.
    let card_identity_from_cost = derive_colors_from_mana_cost(card_mana_cost);

    // Every color in the card's identity must be in the commander's identity
    card_colors
        .iter()
        .chain(card_identity_from_cost.iter())
        .all(|c| commander_identity.contains(c))
}

/// CR 903.5a: Commander deck must have exactly 100 cards. CR 903.5b: Singleton except basic lands.
/// CR 408.3: In Commander, the commander card starts the game in the command zone.
///
/// Validate a Commander deck: 100 cards, singleton (except basics), all cards within
/// commander's color identity.
///
/// CR 903.4: A card's color identity includes colors from both its mana cost and
/// color indicator. Both `card_color_map` (color indicators/overrides) and
/// `card_mana_cost_map` (mana cost colors) are checked.
pub fn validate_commander_deck(
    deck_colors: &[ManaColor],
    card_names: &[String],
    card_color_map: &HashMap<String, Vec<ManaColor>>,
    card_mana_cost_map: &HashMap<String, ManaCost>,
    expected_size: u16,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    // Check deck size
    if card_names.len() != expected_size as usize {
        errors.push(format!(
            "Commander deck must have exactly {} cards, found {}",
            expected_size,
            card_names.len()
        ));
    }

    // Check singleton rule (basic lands are exempt)
    let basic_lands = ["Plains", "Island", "Swamp", "Mountain", "Forest"];
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for name in card_names {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    for (name, count) in &counts {
        if *count > 1 && !basic_lands.contains(name) {
            errors.push(format!(
                "Commander deck is singleton: '{}' appears {} times",
                name, count
            ));
        }
    }

    // CR 903.4: Check color identity from color indicators/overrides.
    for (name, colors) in card_color_map {
        for color in colors {
            if !deck_colors.contains(color) {
                errors.push(format!(
                    "'{}' has color {:?} outside commander's color identity",
                    name, color
                ));
                break;
            }
        }
    }

    // CR 903.4: Check color identity from mana cost shards.
    for (name, mana_cost) in card_mana_cost_map {
        if let ManaCost::Cost { shards, .. } = mana_cost {
            for shard in shards {
                let mut violation_found = false;
                for color in ManaColor::ALL {
                    if shard.contributes_to(color) && !deck_colors.contains(&color) {
                        errors.push(format!(
                            "'{}' has color {:?} in mana cost outside commander's color identity",
                            name, color
                        ));
                        violation_found = true;
                        break;
                    }
                }
                if violation_found {
                    break;
                }
            }
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card_type::CoreType;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn setup_commander_game() -> GameState {
        GameState::new(FormatConfig::commander(), 4, 42)
    }

    fn create_commander_in_command_zone(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        colors: Vec<ManaColor>,
    ) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Command,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.is_commander = true;
        obj.color = colors.clone();
        obj.base_color = colors;
        obj_id
    }

    // --- Commander Tax Tests ---

    #[test]
    fn commander_tax_zero_on_first_cast() {
        let state = setup_commander_game();
        let commander_id = ObjectId(99);
        assert_eq!(commander_tax(&state, commander_id), 0);
    }

    #[test]
    fn commander_tax_increments_correctly() {
        let mut state = setup_commander_game();
        let commander_id = ObjectId(99);

        record_commander_cast(&mut state, commander_id);
        assert_eq!(commander_tax(&state, commander_id), 2);

        record_commander_cast(&mut state, commander_id);
        assert_eq!(commander_tax(&state, commander_id), 4);

        record_commander_cast(&mut state, commander_id);
        assert_eq!(commander_tax(&state, commander_id), 6);
    }

    #[test]
    fn commander_tax_tracks_per_commander_for_partners() {
        let mut state = setup_commander_game();
        let commander_a = ObjectId(10);
        let commander_b = ObjectId(20);

        record_commander_cast(&mut state, commander_a);
        record_commander_cast(&mut state, commander_a);
        record_commander_cast(&mut state, commander_b);

        assert_eq!(commander_tax(&state, commander_a), 4);
        assert_eq!(commander_tax(&state, commander_b), 2);
    }

    // --- Zone Redirection Tests ---

    #[test]
    fn redirect_commander_from_graveyard() {
        let mut state = setup_commander_game();
        let cmd_id = create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

        assert!(should_redirect_to_command_zone(
            &state,
            cmd_id,
            Zone::Graveyard
        ));
    }

    #[test]
    fn redirect_commander_from_exile() {
        let mut state = setup_commander_game();
        let cmd_id = create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

        assert!(should_redirect_to_command_zone(&state, cmd_id, Zone::Exile));
    }

    #[test]
    fn no_redirect_to_hand() {
        let mut state = setup_commander_game();
        let cmd_id = create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

        assert!(!should_redirect_to_command_zone(&state, cmd_id, Zone::Hand));
    }

    #[test]
    fn no_redirect_to_library() {
        let mut state = setup_commander_game();
        let cmd_id = create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

        assert!(!should_redirect_to_command_zone(
            &state,
            cmd_id,
            Zone::Library
        ));
    }

    #[test]
    fn no_redirect_for_non_commander() {
        let mut state = setup_commander_game();
        let obj_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        // is_commander defaults to false

        assert!(!should_redirect_to_command_zone(
            &state,
            obj_id,
            Zone::Graveyard
        ));
    }

    // --- Color Identity Tests ---

    #[test]
    fn color_identity_allows_subset() {
        let mut state = setup_commander_game();
        create_commander_in_command_zone(
            &mut state,
            PlayerId(0),
            "Niv-Mizzet",
            vec![ManaColor::Blue, ManaColor::Red],
        );

        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Red],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue, ManaColor::Red],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
    }

    #[test]
    fn color_identity_blocks_off_identity() {
        let mut state = setup_commander_game();
        create_commander_in_command_zone(&mut state, PlayerId(0), "Krenko", vec![ManaColor::Red]);

        assert!(!can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
        assert!(!can_cast_in_color_identity(
            &state,
            &[ManaColor::Green],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
    }

    #[test]
    fn color_identity_allows_colorless() {
        let mut state = setup_commander_game();
        create_commander_in_command_zone(&mut state, PlayerId(0), "Krenko", vec![ManaColor::Red]);

        // Colorless cards (empty color array) are always allowed
        assert!(can_cast_in_color_identity(
            &state,
            &[],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
    }

    #[test]
    fn color_identity_allows_all_when_no_commander() {
        let state = setup_commander_game();

        // No commanders created -- should allow any color
        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
    }

    #[test]
    fn color_identity_includes_mana_cost_colors() {
        // CR 903.4: A commander's identity includes colors from its mana cost.
        let mut state = setup_commander_game();
        let cmd_id = create_commander_in_command_zone(
            &mut state,
            PlayerId(0),
            "Colorless Commander",
            vec![], // No color indicator
        );
        // Give it a {R} mana cost so its identity includes Red
        state.objects.get_mut(&cmd_id).unwrap().mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        };

        // A Red card should be allowed (commander has Red in identity via mana cost)
        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Red],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
        // Blue should still be blocked
        assert!(!can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue],
            &ManaCost::NoCost,
            PlayerId(0)
        ));
    }

    #[test]
    fn color_identity_card_mana_cost_checked() {
        // CR 903.4: A card with {R} in its mana cost has Red identity even if colorless.
        let mut state = setup_commander_game();
        create_commander_in_command_zone(
            &mut state,
            PlayerId(0),
            "Mono-Green Commander",
            vec![ManaColor::Green],
        );

        // Colorless card with {R} in mana cost → Red identity → blocked by Green commander
        let red_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 1,
        };
        assert!(!can_cast_in_color_identity(
            &state,
            &[], // colorless card
            &red_cost,
            PlayerId(0)
        ));

        // Colorless card with {G} in mana cost → Green identity → allowed
        let green_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        };
        assert!(can_cast_in_color_identity(
            &state,
            &[],
            &green_cost,
            PlayerId(0)
        ));
    }

    // --- Deck Validation Tests ---

    #[test]
    fn validate_commander_deck_correct() {
        let identity = vec![ManaColor::Red, ManaColor::White];
        let names: Vec<String> = (0..100).map(|i| format!("Card {}", i)).collect();
        let mut color_map = HashMap::new();
        color_map.insert("Card 0".to_string(), vec![ManaColor::Red]);
        color_map.insert("Card 1".to_string(), vec![ManaColor::White]);

        let result = validate_commander_deck(&identity, &names, &color_map, &HashMap::new(), 100);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_commander_deck_wrong_size() {
        let identity = vec![ManaColor::Red];
        let names: Vec<String> = (0..60).map(|i| format!("Card {}", i)).collect();
        let color_map = HashMap::new();

        let result = validate_commander_deck(&identity, &names, &color_map, &HashMap::new(), 100);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors[0].contains("100 cards"));
    }

    #[test]
    fn validate_commander_deck_non_singleton() {
        let identity = vec![ManaColor::Red];
        let mut names: Vec<String> = (0..98).map(|i| format!("Card {}", i)).collect();
        names.push("Duplicate Card".to_string());
        names.push("Duplicate Card".to_string());
        let color_map = HashMap::new();

        let result = validate_commander_deck(&identity, &names, &color_map, &HashMap::new(), 100);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| e.contains("Duplicate Card")));
    }

    #[test]
    fn validate_commander_deck_basic_lands_exempt_from_singleton() {
        let identity = vec![ManaColor::Red];
        let mut names: Vec<String> = (0..90).map(|i| format!("Card {}", i)).collect();
        // Add 10 Mountains (basic land)
        for _ in 0..10 {
            names.push("Mountain".to_string());
        }
        let color_map = HashMap::new();

        let result = validate_commander_deck(&identity, &names, &color_map, &HashMap::new(), 100);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_commander_deck_wrong_colors() {
        let identity = vec![ManaColor::Red];
        let names: Vec<String> = (0..100).map(|i| format!("Card {}", i)).collect();
        let mut color_map = HashMap::new();
        color_map.insert("Card 0".to_string(), vec![ManaColor::Blue]); // off-identity

        let result = validate_commander_deck(&identity, &names, &color_map, &HashMap::new(), 100);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| e.contains("Card 0")));
    }

    #[test]
    fn validate_commander_deck_mana_cost_outside_identity() {
        // CR 903.4: A card with red mana cost should fail in a mono-white deck.
        let identity = vec![ManaColor::White];
        let names: Vec<String> = (0..100).map(|i| format!("Card {}", i)).collect();
        let color_map = HashMap::new();
        let mut mana_cost_map = HashMap::new();
        mana_cost_map.insert(
            "Card 0".to_string(),
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
        );

        let result = validate_commander_deck(&identity, &names, &color_map, &mana_cost_map, 100);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors
            .iter()
            .any(|e| e.contains("Card 0") && e.contains("mana cost")));
    }

    #[test]
    fn validate_commander_deck_mana_cost_within_identity() {
        // CR 903.4: A card with red mana cost should pass in a R/W deck.
        let identity = vec![ManaColor::Red, ManaColor::White];
        let names: Vec<String> = (0..100).map(|i| format!("Card {}", i)).collect();
        let color_map = HashMap::new();
        let mut mana_cost_map = HashMap::new();
        mana_cost_map.insert(
            "Card 0".to_string(),
            ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
        );

        let result = validate_commander_deck(&identity, &names, &color_map, &mana_cost_map, 100);
        assert!(result.is_ok());
    }

    // --- Integration Tests ---

    #[test]
    fn integration_commander_cast_from_command_zone_with_tax() {
        use crate::game::casting::handle_cast_spell;
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};
        use crate::types::game_state::WaitingFor;
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
        use crate::types::phase::Phase;

        let mut state = setup_commander_game();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.turn_number = 2;

        // Create commander in command zone
        let cmd_id = create_commander_in_command_zone(
            &mut state,
            PlayerId(0),
            "Kaalia",
            vec![ManaColor::Red, ManaColor::White, ManaColor::Black],
        );
        let card_id = state.objects[&cmd_id].card_id;

        // Give the commander a mana cost and an ability
        {
            let obj = state.objects.get_mut(&cmd_id).unwrap();
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 2,
            };
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "Commander".to_string(),
                    description: None,
                },
            ));
        }

        // Give player mana to cast (1R + 2 generic = 3 total for first cast)
        let player_data = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player_data.mana_pool.add(ManaUnit {
                color: ManaType::Red,
                source_id: crate::types::identifiers::ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
                grants: vec![],
                expiry: None,
            });
        }

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), cmd_id, card_id, &mut events);
        assert!(
            result.is_ok(),
            "First cast from command zone should succeed"
        );
        assert!(matches!(result.unwrap(), WaitingFor::Priority { .. }));

        // Commander tax should be 2 after first cast (for next cast)
        assert_eq!(commander_tax(&state, cmd_id), 2);
    }

    #[test]
    fn integration_commander_zone_redirection_on_death() {
        use crate::types::events::GameEvent;

        let mut state = setup_commander_game();

        // Create commander on the battlefield
        let cmd_id = create_commander_in_command_zone(
            &mut state,
            PlayerId(0),
            "Kaalia",
            vec![ManaColor::Red],
        );

        // Move commander to battlefield first
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, cmd_id, Zone::Battlefield, &mut events);
        assert_eq!(state.objects[&cmd_id].zone, Zone::Battlefield);

        // Now "destroy" it (move to graveyard) -- should redirect to command zone
        events.clear();
        crate::game::zones::move_to_zone(&mut state, cmd_id, Zone::Graveyard, &mut events);

        // Commander should be in command zone, not graveyard
        assert_eq!(state.objects[&cmd_id].zone, Zone::Command);

        // ZoneChanged event should show it went to Command, not Graveyard
        let zone_changed = events
            .iter()
            .find(|e| matches!(e, GameEvent::ZoneChanged { .. }));
        assert!(zone_changed.is_some());
        if let Some(GameEvent::ZoneChanged { to, .. }) = zone_changed {
            assert_eq!(*to, Zone::Command);
        }
    }

    #[test]
    fn integration_non_commander_format_no_redirection() {
        let mut state = crate::types::game_state::GameState::new_two_player(42);

        // Create a regular creature on the battlefield
        let obj_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Move to graveyard -- should go to graveyard normally (not redirected)
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut events);
        assert_eq!(state.objects[&obj_id].zone, Zone::Graveyard);
    }

    #[test]
    fn integration_deck_loading_creates_commander_in_command_zone() {
        use crate::game::deck_loading::create_commander_from_card_face;
        use crate::types::ability::PtValue;
        use crate::types::card::CardFace;
        use crate::types::card_type::CardType;

        let mut state = setup_commander_game();
        let face = CardFace {
            name: "Kaalia of the Vast".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![
                    ManaCostShard::Red,
                    ManaCostShard::White,
                    ManaCostShard::Black,
                ],
                generic: 1,
            },
            card_type: CardType {
                supertypes: vec![crate::types::card_type::Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Human".to_string(), "Cleric".to_string()],
            },
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![crate::types::keywords::Keyword::Flying],
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
        };

        let obj_id = create_commander_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];

        assert_eq!(obj.zone, Zone::Command);
        assert!(obj.is_commander);
        assert_eq!(obj.name, "Kaalia of the Vast");
        assert_eq!(
            obj.color,
            vec![ManaColor::Red, ManaColor::White, ManaColor::Black]
        );
    }
}
