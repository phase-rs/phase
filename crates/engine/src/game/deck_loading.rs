use std::collections::{HashMap, HashSet};

use rand::seq::SliceRandom;
use serde::{Deserialize, Serialize};

use crate::database::CardDatabase;
use crate::types::ability::PtValue;
use crate::types::card::CardFace;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::zones::create_object;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckEntry {
    pub card: CardFace,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerDeckPayload {
    pub main_deck: Vec<DeckEntry>,
    #[serde(default)]
    pub sideboard: Vec<DeckEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckPayload {
    pub player: PlayerDeckPayload,
    pub opponent: PlayerDeckPayload,
    #[serde(default)]
    pub ai_decks: Vec<PlayerDeckPayload>,
}

/// Lightweight deck format using card names only.
/// Resolved into a DeckPayload via a CardDatabase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlayerDeckList {
    pub main_deck: Vec<String>,
    #[serde(default)]
    pub sideboard: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckList {
    pub player: PlayerDeckList,
    pub opponent: PlayerDeckList,
    #[serde(default)]
    pub ai_decks: Vec<PlayerDeckList>,
}

/// Resolve a flat name list into DeckEntry entries using the card database.
/// Groups duplicate names and skips unresolvable names.
fn resolve_names(db: &CardDatabase, names: &[String]) -> Vec<DeckEntry> {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for name in names {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }
    let mut entries = Vec::new();
    for (name, count) in counts {
        if let Some(face) = db.get_face_by_name(name) {
            entries.push(DeckEntry {
                card: face.clone(),
                count,
            });
        }
    }
    entries
}

/// Resolve a DeckList (name-only) into a DeckPayload (full CardFace objects)
/// using a CardDatabase for lookup. Unresolvable names are silently skipped.
pub fn resolve_deck_list(db: &CardDatabase, list: &DeckList) -> DeckPayload {
    DeckPayload {
        player: PlayerDeckPayload {
            main_deck: resolve_names(db, &list.player.main_deck),
            sideboard: resolve_names(db, &list.player.sideboard),
        },
        opponent: PlayerDeckPayload {
            main_deck: resolve_names(db, &list.opponent.main_deck),
            sideboard: resolve_names(db, &list.opponent.sideboard),
        },
        ai_decks: list
            .ai_decks
            .iter()
            .map(|deck| PlayerDeckPayload {
                main_deck: resolve_names(db, &deck.main_deck),
                sideboard: resolve_names(db, &deck.sideboard),
            })
            .collect(),
    }
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

/// Extract i32 from a typed PtValue. Variable P/T defaults to 0 at runtime.
fn parse_pt(val: &Option<PtValue>) -> Option<i32> {
    val.as_ref().map(|pt| match pt {
        PtValue::Fixed(n) => *n,
        PtValue::Variable(_) => 0,
    })
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
    let loyalty = card_face
        .loyalty
        .as_ref()
        .and_then(|s| s.parse::<u32>().ok());
    let keywords = card_face.keywords.clone();
    let color = card_face
        .color_override
        .clone()
        .unwrap_or_else(|| derive_colors_from_mana_cost(&card_face.mana_cost));

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
    obj.trigger_definitions = card_face.triggers.clone();
    obj.static_definitions = card_face.static_abilities.clone();
    obj.replacement_definitions = card_face.replacements.clone();
    obj.color = color.clone();
    obj.base_color = color;
    obj.modal = card_face.modal.clone();
    obj.additional_cost = card_face.additional_cost.clone();

    obj_id
}

/// Create a commander GameObject from a CardFace, placing it in the command zone.
pub fn create_commander_from_card_face(
    state: &mut GameState,
    card_face: &CardFace,
    owner: PlayerId,
) -> crate::types::identifiers::ObjectId {
    let card_id = CardId(state.next_object_id);
    let obj_id = create_object(state, card_id, owner, card_face.name.clone(), Zone::Command);

    let power = parse_pt(&card_face.power);
    let toughness = parse_pt(&card_face.toughness);
    let loyalty = card_face
        .loyalty
        .as_ref()
        .and_then(|s| s.parse::<u32>().ok());
    let keywords = card_face.keywords.clone();
    let color = card_face
        .color_override
        .clone()
        .unwrap_or_else(|| derive_colors_from_mana_cost(&card_face.mana_cost));

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
    obj.trigger_definitions = card_face.triggers.clone();
    obj.static_definitions = card_face.static_abilities.clone();
    obj.replacement_definitions = card_face.replacements.clone();
    obj.color = color.clone();
    obj.base_color = color;
    obj.is_commander = true;
    obj.modal = card_face.modal.clone();
    obj.additional_cost = card_face.additional_cost.clone();

    obj_id
}

/// Load deck data into a GameState, creating GameObjects in each player's library and shuffling.
pub fn load_deck_into_state(state: &mut GameState, payload: &DeckPayload) {
    state.deck_pools.clear();
    state.sideboard_submitted.clear();

    state
        .deck_pools
        .push(crate::types::game_state::PlayerDeckPool {
            player: PlayerId(0),
            registered_main: payload.player.main_deck.clone(),
            registered_sideboard: payload.player.sideboard.clone(),
            current_main: payload.player.main_deck.clone(),
            current_sideboard: payload.player.sideboard.clone(),
        });
    state
        .deck_pools
        .push(crate::types::game_state::PlayerDeckPool {
            player: PlayerId(1),
            registered_main: payload.opponent.main_deck.clone(),
            registered_sideboard: payload.opponent.sideboard.clone(),
            current_main: payload.opponent.main_deck.clone(),
            current_sideboard: payload.opponent.sideboard.clone(),
        });
    for (i, ai_deck) in payload.ai_decks.iter().enumerate() {
        let player_id = PlayerId((2 + i) as u8);
        state
            .deck_pools
            .push(crate::types::game_state::PlayerDeckPool {
                player: player_id,
                registered_main: ai_deck.main_deck.clone(),
                registered_sideboard: ai_deck.sideboard.clone(),
                current_main: ai_deck.main_deck.clone(),
                current_sideboard: ai_deck.sideboard.clone(),
            });
    }

    for entry in &payload.player.main_deck {
        for _ in 0..entry.count {
            create_object_from_card_face(state, &entry.card, PlayerId(0));
        }
    }

    for entry in &payload.opponent.main_deck {
        for _ in 0..entry.count {
            create_object_from_card_face(state, &entry.card, PlayerId(1));
        }
    }

    // Load additional AI decks into PlayerId(2), PlayerId(3), etc.
    for (i, ai_deck) in payload.ai_decks.iter().enumerate() {
        let player_id = PlayerId((2 + i) as u8);
        for entry in &ai_deck.main_deck {
            for _ in 0..entry.count {
                create_object_from_card_face(state, &entry.card, player_id);
            }
        }
    }

    // Collect all creature subtypes for Changeling CDA expansion
    let mut creature_types: HashSet<String> = HashSet::new();
    let all_entries = payload
        .player
        .main_deck
        .iter()
        .chain(&payload.opponent.main_deck)
        .chain(payload.ai_decks.iter().flat_map(|d| &d.main_deck));
    for entry in all_entries {
        if entry
            .card
            .card_type
            .core_types
            .contains(&crate::types::card_type::CoreType::Creature)
        {
            creature_types.extend(entry.card.card_type.subtypes.iter().cloned());
        }
    }
    let mut sorted: Vec<String> = creature_types.into_iter().collect();
    sorted.sort();
    state.all_creature_types = sorted;

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
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, DamageAmount, Effect, PtValue,
        StaticDefinition, TargetFilter,
    };
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
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![Keyword::Trample],
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Pump {
                    power: PtValue::Fixed(0),
                    toughness: PtValue::Fixed(0),
                    target: TargetFilter::Any,
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap)],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
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
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: DamageAmount::Fixed(3),
                    target: TargetFilter::Any,
                },
            )],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
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
        face.power = Some(PtValue::Variable("*".to_string()));
        face.toughness = Some(PtValue::Variable("*".to_string()));

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
            player: PlayerDeckPayload {
                main_deck: vec![
                    DeckEntry {
                        card: make_creature_face(),
                        count: 4,
                    },
                    DeckEntry {
                        card: make_instant_face(),
                        count: 2,
                    },
                ],
                sideboard: vec![],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![DeckEntry {
                    card: make_creature_face(),
                    count: 3,
                }],
                sideboard: vec![],
            },
            ai_decks: vec![],
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
            player: PlayerDeckPayload {
                main_deck: entries,
                sideboard: vec![],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![],
                sideboard: vec![],
            },
            ai_decks: vec![],
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
        face.triggers = vec![crate::types::ability::TriggerDefinition::new(
            crate::types::triggers::TriggerMode::ChangesZone,
        )
        .destination(Zone::Battlefield)];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.trigger_definitions.len(), 1);
        assert_eq!(
            obj.trigger_definitions[0].mode,
            crate::types::triggers::TriggerMode::ChangesZone
        );
    }

    #[test]
    fn create_object_with_static_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.static_abilities = vec![StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddPower { value: 2 }])];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.static_definitions.len(), 1);
        assert_eq!(
            obj.static_definitions[0].mode,
            crate::types::statics::StaticMode::Continuous
        );
    }

    #[test]
    fn create_object_with_replacement_definitions() {
        let mut state = GameState::new_two_player(42);
        let mut face = make_creature_face();
        face.replacements = vec![crate::types::ability::ReplacementDefinition::new(
            crate::types::replacements::ReplacementEvent::DamageDone,
        )
        .valid_card(TargetFilter::SelfRef)];

        let obj_id = create_object_from_card_face(&mut state, &face, PlayerId(0));
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert_eq!(
            obj.replacement_definitions[0].event,
            crate::types::replacements::ReplacementEvent::DamageDone
        );
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
            player: PlayerDeckPayload {
                main_deck: vec![DeckEntry {
                    card: make_creature_face(),
                    count: 4,
                }],
                sideboard: vec![],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![],
                sideboard: vec![],
            },
            ai_decks: vec![],
        };
        let json = serde_json::to_string(&payload).unwrap();
        let deserialized: DeckPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.player.main_deck.len(), 1);
        assert_eq!(deserialized.player.main_deck[0].count, 4);
        assert_eq!(deserialized.player.main_deck[0].card.name, "Grizzly Bears");
    }
}
