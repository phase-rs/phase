use std::collections::HashMap;

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::parser::ability::{parse_replacement, parse_static, parse_trigger};
use crate::types::card::CardFace;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::mana::{ManaCost, ManaCostShard, ManaColor};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::keywords::parse_keywords;
use super::zones::create_object;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckEntry {
    pub card: CardFace,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckPayload {
    pub player_deck: Vec<DeckEntry>,
    pub opponent_deck: Vec<DeckEntry>,
}

/// Derive ManaColor values from a ManaCostShard.
fn shard_colors(shard: &ManaCostShard) -> Vec<ManaColor> {
    match shard {
        ManaCostShard::White | ManaCostShard::TwoWhite | ManaCostShard::PhyrexianWhite => {
            vec![ManaColor::White]
        }
        ManaCostShard::Blue | ManaCostShard::TwoBlue | ManaCostShard::PhyrexianBlue => {
            vec![ManaColor::Blue]
        }
        ManaCostShard::Black | ManaCostShard::TwoBlack | ManaCostShard::PhyrexianBlack => {
            vec![ManaColor::Black]
        }
        ManaCostShard::Red | ManaCostShard::TwoRed | ManaCostShard::PhyrexianRed => {
            vec![ManaColor::Red]
        }
        ManaCostShard::Green | ManaCostShard::TwoGreen | ManaCostShard::PhyrexianGreen => {
            vec![ManaColor::Green]
        }
        ManaCostShard::WhiteBlue | ManaCostShard::PhyrexianWhiteBlue => {
            vec![ManaColor::White, ManaColor::Blue]
        }
        ManaCostShard::WhiteBlack | ManaCostShard::PhyrexianWhiteBlack => {
            vec![ManaColor::White, ManaColor::Black]
        }
        ManaCostShard::BlueBlack | ManaCostShard::PhyrexianBlueBlack => {
            vec![ManaColor::Blue, ManaColor::Black]
        }
        ManaCostShard::BlueRed | ManaCostShard::PhyrexianBlueRed => {
            vec![ManaColor::Blue, ManaColor::Red]
        }
        ManaCostShard::BlackRed | ManaCostShard::PhyrexianBlackRed => {
            vec![ManaColor::Black, ManaColor::Red]
        }
        ManaCostShard::BlackGreen | ManaCostShard::PhyrexianBlackGreen => {
            vec![ManaColor::Black, ManaColor::Green]
        }
        ManaCostShard::RedWhite | ManaCostShard::PhyrexianRedWhite => {
            vec![ManaColor::Red, ManaColor::White]
        }
        ManaCostShard::RedGreen | ManaCostShard::PhyrexianRedGreen => {
            vec![ManaColor::Red, ManaColor::Green]
        }
        ManaCostShard::GreenWhite | ManaCostShard::PhyrexianGreenWhite => {
            vec![ManaColor::Green, ManaColor::White]
        }
        ManaCostShard::GreenBlue | ManaCostShard::PhyrexianGreenBlue => {
            vec![ManaColor::Green, ManaColor::Blue]
        }
        ManaCostShard::ColorlessWhite => vec![ManaColor::White],
        ManaCostShard::ColorlessBlue => vec![ManaColor::Blue],
        ManaCostShard::ColorlessBlack => vec![ManaColor::Black],
        ManaCostShard::ColorlessRed => vec![ManaColor::Red],
        ManaCostShard::ColorlessGreen => vec![ManaColor::Green],
        ManaCostShard::Colorless | ManaCostShard::Snow | ManaCostShard::X => vec![],
    }
}

/// Derive color identity from a ManaCost by collecting unique ManaColor values from shards.
pub(crate) fn derive_colors_from_mana_cost(mana_cost: &ManaCost) -> Vec<ManaColor> {
    match mana_cost {
        ManaCost::NoCost => vec![],
        ManaCost::Cost { shards, .. } => {
            let mut colors = Vec::new();
            for shard in shards {
                for color in shard_colors(shard) {
                    if !colors.contains(&color) {
                        colors.push(color);
                    }
                }
            }
            colors
        }
    }
}

/// Parse a power/toughness string to i32. Variable P/T like "*" defaults to 0.
fn parse_pt(val: &Option<String>) -> Option<i32> {
    val.as_ref().map(|s| s.parse::<i32>().unwrap_or(0))
}

/// Create a fully-populated GameObject from a CardFace and place it in the owner's library.
pub fn create_object_from_card_face(
    state: &mut GameState,
    card_face: &CardFace,
    owner: PlayerId,
) -> crate::types::identifiers::ObjectId {
    let card_id = CardId(state.next_object_id);
    let obj_id = create_object(state, card_id, owner, card_face.name.clone(), Zone::Library);

    let power = parse_pt(&card_face.power);
    let toughness = parse_pt(&card_face.toughness);
    let loyalty = card_face.loyalty.as_ref().and_then(|s| s.parse::<u32>().ok());
    let keywords = parse_keywords(&card_face.keywords);
    let color = card_face
        .color_override
        .clone()
        .unwrap_or_else(|| derive_colors_from_mana_cost(&card_face.mana_cost));

    // Parse trigger definitions, skipping failures
    let trigger_definitions = card_face
        .triggers
        .iter()
        .filter_map(|raw| parse_trigger(raw).ok())
        .collect();

    // Parse static definitions, skipping failures
    let static_definitions = card_face
        .static_abilities
        .iter()
        .filter_map(|raw| parse_static(raw).ok())
        .collect();

    // Parse replacement definitions, skipping failures
    let replacement_definitions = card_face
        .replacements
        .iter()
        .filter_map(|raw| parse_replacement(raw).ok())
        .collect();

    let obj = state.objects.get_mut(&obj_id).expect("just created");
    obj.card_types = card_face.card_type.clone();
    obj.mana_cost = card_face.mana_cost.clone();
    obj.power = power;
    obj.toughness = toughness;
    obj.base_power = power;
    obj.base_toughness = toughness;
    obj.loyalty = loyalty;
    obj.keywords = keywords.clone();
    obj.base_keywords = keywords;
    obj.abilities = card_face.abilities.clone();
    obj.svars = card_face.svars.clone();
    obj.trigger_definitions = trigger_definitions;
    obj.static_definitions = static_definitions;
    obj.replacement_definitions = replacement_definitions;
    obj.color = color.clone();
    obj.base_color = color;

    obj_id
}

/// Load deck data into a GameState, creating GameObjects in each player's library and shuffling.
pub fn load_deck_into_state(state: &mut GameState, payload: &DeckPayload) {
    for entry in &payload.player_deck {
        for _ in 0..entry.count {
            create_object_from_card_face(state, &entry.card, PlayerId(0));
        }
    }

    for entry in &payload.opponent_deck {
        for _ in 0..entry.count {
            create_object_from_card_face(state, &entry.card, PlayerId(1));
        }
    }

    // Shuffle each player's library
    // Extract libraries, shuffle with rng, then put back to avoid conflicting mutable borrows
    let mut libraries: Vec<Vec<crate::types::identifiers::ObjectId>> =
        state.players.iter().map(|p| p.library.clone()).collect();
    for lib in &mut libraries {
        lib.shuffle(&mut state.rng);
    }
    for (i, lib) in libraries.into_iter().enumerate() {
        state.players[i].library = lib;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card_type::CardType;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCostShard;

    fn make_creature_face() -> CardFace {
        CardFace {
            name: "Grizzly Bears".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![crate::types::card_type::CoreType::Creature],
                subtypes: vec!["Bear".to_string()],
            },
            power: Some("2".to_string()),
            toughness: Some("2".to_string()),
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec!["Trample".to_string()],
            abilities: vec!["AB$ Pump | Cost$ G".to_string()],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            svars: HashMap::new(),
            color_override: None,
        }
    }

    fn make_instant_face() -> CardFace {
        CardFace {
            name: "Lightning Bolt".to_string(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![crate::types::card_type::CoreType::Instant],
                subtypes: vec![],
            },
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![],
            abilities: vec!["SP$ DealDamage | NumDmg$ 3".to_string()],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            svars: HashMap::new(),
            color_override: None,
        }
    }

    #[test]
    fn create_object_from_card_face_populates_characteristics() {
        let mut state = GameState::new_two_player(42);
        let face = make_creature_face();
        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.name, "Grizzly Bears");
        assert_eq!(obj.power, Some(2));
        assert_eq!(obj.toughness, Some(2));
        assert_eq!(obj.base_power, Some(2));
        assert_eq!(obj.base_toughness, Some(2));
        assert_eq!(obj.keywords, vec![Keyword::Trample]);
        assert_eq!(obj.base_keywords, vec![Keyword::Trample]);
        assert_eq!(obj.color, vec![ManaColor::Green]);
        assert_eq!(obj.base_color, vec![ManaColor::Green]);
        assert_eq!(
            obj.mana_cost,
            ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            }
        );
        assert_eq!(obj.abilities.len(), 1);
        assert_eq!(obj.zone, Zone::Library);
        assert_eq!(obj.owner, PlayerId(0));
    }

    #[test]
    fn create_object_from_card_face_color_override() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.color_override = Some(vec![ManaColor::White, ManaColor::Green]);

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.color, vec![ManaColor::White, ManaColor::Green]);
    }

    #[test]
    fn create_object_variable_pt_defaults_to_zero() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.power = Some("*".to_string());
        face.toughness = Some("*".to_string());

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.power, Some(0));
        assert_eq!(obj.toughness, Some(0));
        assert_eq!(obj.base_power, Some(0));
        assert_eq!(obj.base_toughness, Some(0));
    }

    #[test]
    fn create_object_no_pt_stays_none() {
        let mut state = GameState::new_two_player(42);
        let face = make_instant_face();

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert!(obj.power.is_none());
        assert!(obj.toughness.is_none());
    }

    #[test]
    fn load_deck_creates_correct_object_count() {
        let mut state = GameState::new_two_player(42);
        let payload = DeckPayload {
            player_deck: vec![
                DeckEntry {
                    card: make_creature_face(),
                    count: 4,
                },
                DeckEntry {
                    card: make_instant_face(),
                    count: 2,
                },
            ],
            opponent_deck: vec![DeckEntry {
                card: make_creature_face(),
                count: 3,
            }],
        };

        load_deck_into_state(&mut state, &payload);

        assert_eq!(state.players[0].library.len(), 6); // 4 + 2
        assert_eq!(state.players[1].library.len(), 3);
        assert_eq!(state.objects.len(), 9); // 6 + 3
    }

    #[test]
    fn load_deck_shuffles_libraries() {
        // Use a large enough deck that shuffle is virtually guaranteed to change order
        let mut entries = Vec::new();
        for i in 0..20 {
            entries.push(DeckEntry {
                card: CardFace {
                    name: format!("Card {}", i),
                    ..make_creature_face()
                },
                count: 1,
            });
        }

        let mut state = GameState::new_two_player(42);
        let payload = DeckPayload {
            player_deck: entries,
            opponent_deck: vec![],
        };
        load_deck_into_state(&mut state, &payload);

        // Collect names in library order
        let names: Vec<String> = state.players[0]
            .library
            .iter()
            .map(|id| state.objects[id].name.clone())
            .collect();

        // Check that the order differs from insertion order (Card 0, Card 1, ...)
        let insertion_order: Vec<String> = (0..20).map(|i| format!("Card {}", i)).collect();
        assert_ne!(names, insertion_order, "Library should be shuffled");
    }

    #[test]
    fn create_object_with_trigger_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.triggers =
            vec!["Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield".to_string()];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.trigger_definitions.len(), 1);
        assert_eq!(obj.trigger_definitions[0].mode, "ChangesZone");
    }

    #[test]
    fn create_object_with_static_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.static_abilities =
            vec!["Mode$ Continuous | Affected$ Card.Self | AddPower$ 2".to_string()];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.static_definitions.len(), 1);
        assert_eq!(obj.static_definitions[0].mode, "Continuous");
    }

    #[test]
    fn create_object_with_replacement_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.replacements =
            vec!["Event$ DamageDone | ActiveZones$ Battlefield | ValidSource$ Card.Self".to_string()];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert_eq!(obj.replacement_definitions[0].event, "DamageDone");
    }

    #[test]
    fn derive_colors_multicolor() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            generic: 1,
        };
        let colors = derive_colors_from_mana_cost(&cost);
        assert_eq!(colors, vec![ManaColor::White, ManaColor::Blue]);
    }

    #[test]
    fn derive_colors_no_cost() {
        let colors = derive_colors_from_mana_cost(&ManaCost::NoCost);
        assert!(colors.is_empty());
    }

    #[test]
    fn derive_colors_hybrid() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::WhiteBlue],
            generic: 0,
        };
        let colors = derive_colors_from_mana_cost(&cost);
        assert_eq!(colors, vec![ManaColor::White, ManaColor::Blue]);
    }

    #[test]
    fn derive_colors_deduplicates() {
        let cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        };
        let colors = derive_colors_from_mana_cost(&cost);
        assert_eq!(colors, vec![ManaColor::Red]);
    }

    #[test]
    fn deck_payload_serializes_roundtrips() {
        let payload = DeckPayload {
            player_deck: vec![DeckEntry {
                card: make_creature_face(),
                count: 4,
            }],
            opponent_deck: vec![],
        };
        let json = serde_json::to_string(&payload).unwrap();
        let deserialized: DeckPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.player_deck.len(), 1);
        assert_eq!(deserialized.player_deck[0].count, 4);
        assert_eq!(deserialized.player_deck[0].card.name, "Grizzly Bears");
    }
}
