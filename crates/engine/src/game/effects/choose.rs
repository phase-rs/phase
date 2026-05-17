use crate::game::players;
use crate::types::ability::{ChoiceType, Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;

/// Choose: present the player with a named set of options (creature type, color, etc.).
/// CR 700.2: Modal and choice-based spells/abilities require the controller to choose
/// from available options as part of casting or resolution.
/// Sets WaitingFor::NamedChoice so the player can select one.
/// The engine processes the ChooseOption response in engine.rs,
/// storing the result in GameState::last_named_choice for continuations.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (choice_type, persist) = match &ability.effect {
        Effect::Choose {
            choice_type,
            persist,
        } => (choice_type.clone(), *persist),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected Choose effect".to_string(),
            ))
        }
    };

    let options = compute_options(
        state,
        &choice_type,
        ability.controller,
        &ability.chosen_players,
    );

    state.waiting_for = WaitingFor::NamedChoice {
        player: ability.controller,
        choice_type,
        options,
        source_id: if persist {
            Some(ability.source_id)
        } else {
            None
        },
    };

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

const FALLBACK_CREATURE_TYPES: &[&str] = &[
    "Human",
    "Elf",
    "Goblin",
    "Merfolk",
    "Zombie",
    "Soldier",
    "Wizard",
    "Dragon",
    "Angel",
    "Demon",
    "Beast",
    "Bird",
    "Cat",
    "Elemental",
    "Faerie",
    "Giant",
    "Knight",
    "Rogue",
    "Spirit",
    "Vampire",
    "Warrior",
];

const ODD_OR_EVEN: &[&str] = &["Odd", "Even"];

const BASIC_LAND_TYPES: &[&str] = &["Plains", "Island", "Swamp", "Mountain", "Forest"];

const CARD_TYPES: &[&str] = &[
    "Artifact",
    "Creature",
    "Enchantment",
    "Instant",
    "Land",
    "Planeswalker",
    "Sorcery",
];

/// CR 205.3i: All land subtypes. Derived from `is_land_subtype()` in `types/card_type.rs`.
const LAND_TYPES: &[&str] = &[
    "Cave",
    "Desert",
    "Forest",
    "Gate",
    "Island",
    "Lair",
    "Locus",
    "Mine",
    "Mountain",
    "Plains",
    "Planet",
    "Power-Plant",
    "Sphere",
    "Swamp",
    "Tower",
    "Town",
    "Urza's",
];

/// Compute the valid options for a given choice type.
/// CR 700.2: The controller of a modal spell or ability chooses options as part of
/// casting or resolution. If an option would be illegal, it can't be chosen.
///
/// `already_chosen` is the resolution-scoped list of players picked by earlier
/// `Choose(Player)` instructions in this chain. CR 608.2c + the Gluntch card
/// ruling ("three distinct players") require each successive "choose a player"
/// to exclude players already chosen — `ChoiceType::Player` and
/// `ChoiceType::Opponent` filter them out. When fewer eligible players remain
/// than the card asks for, the options list is empty and the choice (and its
/// dependent effect) does nothing — the standard empty-options path.
fn compute_options(
    state: &GameState,
    choice_type: &ChoiceType,
    controller: PlayerId,
    already_chosen: &[PlayerId],
) -> Vec<String> {
    match choice_type {
        // CR 205.3m: Creature types are shared between creature and kindred cards.
        ChoiceType::CreatureType => {
            if state.all_creature_types.is_empty() {
                to_strings(FALLBACK_CREATURE_TYPES)
            } else {
                let mut types = state.all_creature_types.clone();
                types.sort();
                types.dedup();
                types
            }
        }
        // CR 105.1 + CR 105.4: A color choice is one of white, blue, black, red, or green.
        ChoiceType::Color { excluded } => ManaColor::ALL
            .iter()
            .filter(|color| !excluded.contains(color))
            .map(|color| color_name(*color).to_string())
            .collect(),
        ChoiceType::OddOrEven => to_strings(ODD_OR_EVEN),
        // CR 305.6: The basic land types are Plains, Island, Swamp, Mountain, and Forest.
        ChoiceType::BasicLandType => to_strings(BASIC_LAND_TYPES),
        // CR 205.2a: The card types are artifact, battle, conspiracy, creature,
        // dungeon, enchantment, instant, land, phenomenon, plane, planeswalker,
        // scheme, sorcery, kindred, and vanguard.
        ChoiceType::CardType => to_strings(CARD_TYPES),
        // CardName options are provided by the frontend from its local card database.
        // The engine sends an empty list to avoid serializing 30k+ names every state update.
        ChoiceType::CardName => Vec::new(),
        ChoiceType::NumberRange { min, max } => (*min..=*max).map(|n| n.to_string()).collect(),
        ChoiceType::Labeled { options } => options.clone(),
        // CR 205.3i: Land types include the basic land types plus Cave, Desert, Gate, etc.
        ChoiceType::LandType => to_strings(LAND_TYPES),
        // CR 800.4a: An opponent is any other player in the game.
        // CR 608.2c: Exclude players already chosen earlier in this resolution.
        ChoiceType::Opponent => players::opponents(state, controller)
            .iter()
            .filter(|id| !already_chosen.contains(id))
            .map(|id| id.0.to_string())
            .collect(),
        // CR 102.1: A player is one of the people in the game.
        // CR 608.2c: Exclude players already chosen earlier in this resolution.
        ChoiceType::Player => state
            .seat_order
            .iter()
            .filter(|id| !already_chosen.contains(id))
            .map(|id| id.0.to_string())
            .collect(),
        ChoiceType::TwoColors => two_color_options(),
        ChoiceType::Word | ChoiceType::Artist => Vec::new(),
    }
}

fn to_strings(strs: &[&str]) -> Vec<String> {
    strs.iter().map(|&s| s.to_string()).collect()
}

fn color_name(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "White",
        ManaColor::Blue => "Blue",
        ManaColor::Black => "Black",
        ManaColor::Red => "Red",
        ManaColor::Green => "Green",
    }
}

/// Generate all 10 two-color combinations from the 5 mana colors.
/// Order within a pair doesn't matter, so we use ordered pairs (i < j).
fn two_color_options() -> Vec<String> {
    let mut options = Vec::with_capacity(10);
    let colors: Vec<_> = ManaColor::ALL
        .iter()
        .map(|color| color_name(*color))
        .collect();
    for (i, &c1) in colors.iter().enumerate() {
        for &c2 in &colors[i + 1..] {
            options.push(format!("{c1}, {c2}"));
        }
    }
    options
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_choose_ability(choice_type: ChoiceType) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Choose {
                choice_type,
                persist: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn choose_creature_type_sets_named_choice() {
        let mut state = GameState::new_two_player(42);
        state.all_creature_types = vec!["Elf".to_string(), "Goblin".to_string()];

        let ability = make_choose_ability(ChoiceType::CreatureType);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice {
                player,
                choice_type,
                options,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*choice_type, ChoiceType::CreatureType);
                assert!(options.contains(&"Elf".to_string()));
                assert!(options.contains(&"Goblin".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_color_offers_five_colors() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::color());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 5);
                assert!(options.contains(&"White".to_string()));
                assert!(options.contains(&"Blue".to_string()));
                assert!(options.contains(&"Black".to_string()));
                assert!(options.contains(&"Red".to_string()));
                assert!(options.contains(&"Green".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_color_with_excluded_color_offers_remaining_colors() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::color_excluding(vec![ManaColor::White]));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                ..
            } => {
                assert_eq!(
                    *choice_type,
                    ChoiceType::Color {
                        excluded: vec![ManaColor::White],
                    }
                );
                assert_eq!(options, &["Blue", "Black", "Red", "Green"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_odd_or_even_offers_two_options() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::OddOrEven);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["Odd", "Even"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_basic_land_type_offers_five_types() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::BasicLandType);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 5);
                assert!(options.contains(&"Forest".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_card_type_offers_seven_types() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::CardType);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 7);
                assert!(options.contains(&"Creature".to_string()));
                assert!(options.contains(&"Instant".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_creature_type_with_empty_all_types_uses_fallback() {
        let mut state = GameState::new_two_player(42);
        // all_creature_types is empty by default
        let ability = make_choose_ability(ChoiceType::CreatureType);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert!(!options.is_empty());
                assert!(options.contains(&"Human".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_card_name_sends_empty_options() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::CardName);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::NamedChoice {
                choice_type,
                options,
                ..
            } => {
                assert_eq!(*choice_type, ChoiceType::CardName);
                assert!(options.is_empty());
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn resolve_emits_effect_resolved_event() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::color());
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(events.len(), 1);
        match &events[0] {
            GameEvent::EffectResolved { kind, source_id } => {
                assert_eq!(*kind, EffectKind::Choose);
                assert_eq!(*source_id, ObjectId(100));
            }
            other => panic!("Expected EffectResolved, got {:?}", other),
        }
    }

    #[test]
    fn choose_number_range_generates_options() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Choose {
                choice_type: ChoiceType::NumberRange { min: 0, max: 5 },
                persist: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["0", "1", "2", "3", "4", "5"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_labeled_uses_provided_options() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Choose {
                choice_type: ChoiceType::Labeled {
                    options: vec!["Left".to_string(), "Right".to_string()],
                },
                persist: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["Left", "Right"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_land_type_offers_all_land_types() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::LandType);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert!(options.contains(&"Plains".to_string()));
                assert!(options.contains(&"Forest".to_string()));
                assert!(options.contains(&"Sphere".to_string()));
                assert!(options.contains(&"Urza's".to_string()));
                assert!(options.len() >= 14);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_opponent_lists_opponents() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::Opponent);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                // Player 0 is controller, so opponent is player 1
                assert_eq!(options, &["1"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_player_lists_all_players() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::Player);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options.len(), 2);
                assert!(options.contains(&"0".to_string()));
                assert!(options.contains(&"1".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_player_excludes_already_chosen_players() {
        // CR 608.2c + Gluntch ruling: a successive "choose a player" omits
        // players already chosen earlier in the same resolution.
        let mut state = GameState::new_two_player(42);
        let mut ability = make_choose_ability(ChoiceType::Player);
        ability.chosen_players = vec![PlayerId(0)];
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert_eq!(options, &["1"]);
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_player_with_all_players_chosen_offers_no_options() {
        // Edge: when every player is already chosen, the option set is empty
        // — the choice (and its dependent effect) does nothing.
        let mut state = GameState::new_two_player(42);
        let mut ability = make_choose_ability(ChoiceType::Player);
        ability.chosen_players = vec![PlayerId(0), PlayerId(1)];
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                assert!(options.is_empty());
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }

    #[test]
    fn choose_two_colors_offers_ten_combinations() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choose_ability(ChoiceType::TwoColors);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => {
                // C(5,2) = 10 unique pairs
                assert_eq!(options.len(), 10);
                assert!(options.contains(&"White, Blue".to_string()));
                assert!(options.contains(&"Red, Green".to_string()));
            }
            other => panic!("Expected NamedChoice, got {:?}", other),
        }
    }
}
