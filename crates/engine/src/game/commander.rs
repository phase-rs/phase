use std::collections::HashMap;

use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

/// Returns the commander tax for a given commander: 2 * times_cast_from_command_zone.
pub fn commander_tax(state: &GameState, commander_id: ObjectId) -> u32 {
    state
        .commander_cast_count
        .get(&commander_id)
        .copied()
        .unwrap_or(0)
        * 2
}

/// Record that a commander was cast from the command zone, incrementing its cast count.
pub fn record_commander_cast(state: &mut GameState, commander_id: ObjectId) {
    *state
        .commander_cast_count
        .entry(commander_id)
        .or_insert(0) += 1;
}

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

/// Check if a card's colors are a subset of the commander's color identity.
/// Returns true if the cast is legal under color identity rules.
pub fn can_cast_in_color_identity(
    state: &GameState,
    card_colors: &[ManaColor],
    player: PlayerId,
) -> bool {
    // Collect color identity from all commanders this player owns in the command zone
    // or on the battlefield (commanders can be cast and return)
    let commander_identity: Vec<ManaColor> = state
        .objects
        .values()
        .filter(|obj| obj.is_commander && obj.owner == player)
        .flat_map(|obj| obj.color.iter().copied())
        .collect();

    // If no commander found (non-Commander format), allow everything
    if commander_identity.is_empty() {
        return true;
    }

    // Every color in the card must be in the commander's identity
    card_colors
        .iter()
        .all(|c| commander_identity.contains(c))
}

/// Validate a Commander deck: 100 cards, singleton (except basics), all cards within
/// commander's color identity.
pub fn validate_commander_deck(
    deck_colors: &[ManaColor],
    card_names: &[String],
    card_color_map: &HashMap<String, Vec<ManaColor>>,
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

    // Check color identity
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
        obj.card_types
            .core_types
            .push(CoreType::Creature);
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
        let cmd_id =
            create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

        assert!(should_redirect_to_command_zone(
            &state,
            cmd_id,
            Zone::Graveyard
        ));
    }

    #[test]
    fn redirect_commander_from_exile() {
        let mut state = setup_commander_game();
        let cmd_id =
            create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

        assert!(should_redirect_to_command_zone(
            &state,
            cmd_id,
            Zone::Exile
        ));
    }

    #[test]
    fn no_redirect_to_hand() {
        let mut state = setup_commander_game();
        let cmd_id =
            create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

        assert!(!should_redirect_to_command_zone(
            &state,
            cmd_id,
            Zone::Hand
        ));
    }

    #[test]
    fn no_redirect_to_library() {
        let mut state = setup_commander_game();
        let cmd_id =
            create_commander_in_command_zone(&mut state, PlayerId(0), "Kaalia", vec![]);

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
            PlayerId(0)
        ));
        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Red],
            PlayerId(0)
        ));
        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue, ManaColor::Red],
            PlayerId(0)
        ));
    }

    #[test]
    fn color_identity_blocks_off_identity() {
        let mut state = setup_commander_game();
        create_commander_in_command_zone(
            &mut state,
            PlayerId(0),
            "Krenko",
            vec![ManaColor::Red],
        );

        assert!(!can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue],
            PlayerId(0)
        ));
        assert!(!can_cast_in_color_identity(
            &state,
            &[ManaColor::Green],
            PlayerId(0)
        ));
    }

    #[test]
    fn color_identity_allows_colorless() {
        let mut state = setup_commander_game();
        create_commander_in_command_zone(
            &mut state,
            PlayerId(0),
            "Krenko",
            vec![ManaColor::Red],
        );

        // Colorless cards (empty color array) are always allowed
        assert!(can_cast_in_color_identity(&state, &[], PlayerId(0)));
    }

    #[test]
    fn color_identity_allows_all_when_no_commander() {
        let state = setup_commander_game();

        // No commanders created -- should allow any color
        assert!(can_cast_in_color_identity(
            &state,
            &[ManaColor::Blue],
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

        let result = validate_commander_deck(&identity, &names, &color_map, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_commander_deck_wrong_size() {
        let identity = vec![ManaColor::Red];
        let names: Vec<String> = (0..60).map(|i| format!("Card {}", i)).collect();
        let color_map = HashMap::new();

        let result = validate_commander_deck(&identity, &names, &color_map, 100);
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

        let result = validate_commander_deck(&identity, &names, &color_map, 100);
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

        let result = validate_commander_deck(&identity, &names, &color_map, 100);
        assert!(result.is_ok());
    }

    #[test]
    fn validate_commander_deck_wrong_colors() {
        let identity = vec![ManaColor::Red];
        let names: Vec<String> = (0..100).map(|i| format!("Card {}", i)).collect();
        let mut color_map = HashMap::new();
        color_map.insert("Card 0".to_string(), vec![ManaColor::Blue]); // off-identity

        let result = validate_commander_deck(&identity, &names, &color_map, 100);
        assert!(result.is_err());
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| e.contains("Card 0")));
    }
}
