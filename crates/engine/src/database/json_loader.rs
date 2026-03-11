use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::database::card_db::CardDatabase;
use crate::database::mtgjson::{load_atomic_cards, parse_mtgjson_mana_cost, AtomicCard};
use crate::game::deck_loading::derive_colors_from_mana_cost;
use crate::schema::{AbilityFile, FaceAbilities};
use crate::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect, PtValue};
use crate::types::card::{CardFace, CardLayout, CardRules};
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;

/// Internal layout classification from MTGJSON layout strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutKind {
    Single,
    Split,
    Flip,
    Transform,
    Meld,
    Adventure,
    Modal,
}

/// Map MTGJSON layout string to internal LayoutKind.
fn map_layout(layout_str: &str) -> LayoutKind {
    match layout_str {
        "normal" | "saga" | "class" | "case" | "leveler" => LayoutKind::Single,
        "split" => LayoutKind::Split,
        "flip" => LayoutKind::Flip,
        "transform" => LayoutKind::Transform,
        "meld" => LayoutKind::Meld,
        "adventure" => LayoutKind::Adventure,
        "modal_dfc" => LayoutKind::Modal,
        _ => LayoutKind::Single,
    }
}

/// Build a CardType from MTGJSON atomic card data.
fn build_card_type(mtgjson: &AtomicCard) -> CardType {
    let supertypes = mtgjson
        .supertypes
        .iter()
        .filter_map(|s| Supertype::from_str(s).ok())
        .collect();
    let core_types = mtgjson
        .types
        .iter()
        .filter_map(|s| CoreType::from_str(s).ok())
        .collect();
    let subtypes = mtgjson.subtypes.clone();
    CardType {
        supertypes,
        core_types,
        subtypes,
    }
}

/// Map MTGJSON single-letter color codes to ManaColor.
fn map_mtgjson_color(code: &str) -> Option<ManaColor> {
    match code {
        "W" => Some(ManaColor::White),
        "U" => Some(ManaColor::Blue),
        "B" => Some(ManaColor::Black),
        "R" => Some(ManaColor::Red),
        "G" => Some(ManaColor::Green),
        _ => None,
    }
}

/// Parse a power/toughness string into a PtValue.
fn parse_pt_value(s: &str) -> PtValue {
    match s.parse::<i32>() {
        Ok(n) => PtValue::Fixed(n),
        Err(_) => PtValue::Variable(s.to_string()),
    }
}

/// Build a CardFace by merging MTGJSON metadata with parsed ability data.
fn build_card_face(
    mtgjson: &AtomicCard,
    abilities: &FaceAbilities,
    oracle_id: Option<String>,
) -> CardFace {
    let face_name = mtgjson
        .face_name
        .as_deref()
        .unwrap_or(&mtgjson.name)
        .to_string();
    let mana_cost = mtgjson
        .mana_cost
        .as_deref()
        .map(parse_mtgjson_mana_cost)
        .unwrap_or_default();

    // Derive colors from mana cost, compare with MTGJSON colors
    let mana_derived_colors = derive_colors_from_mana_cost(&mana_cost);
    let mtgjson_colors: Vec<ManaColor> = mtgjson
        .colors
        .iter()
        .filter_map(|c| map_mtgjson_color(c))
        .collect();
    let color_override = if mtgjson_colors != mana_derived_colors {
        Some(mtgjson_colors)
    } else {
        None
    };

    CardFace {
        name: face_name,
        mana_cost,
        card_type: build_card_type(mtgjson),
        power: mtgjson.power.as_ref().map(|s| parse_pt_value(s)),
        toughness: mtgjson.toughness.as_ref().map(|s| parse_pt_value(s)),
        loyalty: mtgjson.loyalty.clone(),
        defense: mtgjson.defense.clone(),
        oracle_text: mtgjson.text.clone(),
        non_ability_text: None,
        flavor_name: None,
        keywords: mtgjson
            .keywords
            .as_ref()
            .map(|kws| {
                kws.iter()
                    .map(|s| s.parse::<Keyword>().unwrap())
                    .filter(|k| !matches!(k, Keyword::Unknown(_)))
                    .collect()
            })
            .unwrap_or_default(),
        abilities: abilities.abilities.clone(),
        triggers: abilities.triggers.clone(),
        static_abilities: abilities.statics.clone(),
        replacements: abilities.replacements.clone(),
        color_override,
        scryfall_oracle_id: oracle_id,
    }
}

/// Synthesize basic land mana abilities per CR 305.6.
/// For each land subtype matching a basic land type, adds a {T}: Add {color} ability.
fn synthesize_basic_land_mana(face: &mut CardFace) {
    let land_mana: Vec<(&str, &str)> = vec![
        ("Plains", "W"),
        ("Island", "U"),
        ("Swamp", "B"),
        ("Mountain", "R"),
        ("Forest", "G"),
    ];

    for (subtype, mana_char) in land_mana {
        if face.card_type.subtypes.iter().any(|s| s == subtype) {
            let color = match mana_char {
                "W" => crate::types::mana::ManaColor::White,
                "U" => crate::types::mana::ManaColor::Blue,
                "B" => crate::types::mana::ManaColor::Black,
                "R" => crate::types::mana::ManaColor::Red,
                "G" => crate::types::mana::ManaColor::Green,
                _ => continue,
            };
            face.abilities.push(AbilityDefinition {
                kind: AbilityKind::Activated,
                effect: Effect::Mana {
                    produced: vec![color],
                },
                cost: Some(AbilityCost::Tap),
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
        }
    }
}

/// Synthesize Equip activated ability from keywords per CR 702.6.
/// Extracts `Keyword::Equip(cost)` and creates an activated Attach ability.
fn synthesize_equip(face: &mut CardFace) {
    let equip_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::Equip(cost) = kw {
                Some(AbilityDefinition {
                    kind: AbilityKind::Activated,
                    effect: Effect::Attach {
                        target: crate::types::ability::TargetFilter::Typed {
                            card_type: Some(crate::types::ability::TypeFilter::Creature),
                            subtype: None,
                            controller: Some(crate::types::ability::ControllerRef::You),
                            properties: vec![],
                        },
                    },
                    cost: Some(AbilityCost::Mana { cost: cost.clone() }),
                    sub_ability: None,
                    duration: None,
                    description: None,
                    target_prompt: None,
                    sorcery_speed: false,
                })
            } else {
                None
            }
        })
        .collect();

    face.abilities.extend(equip_abilities);
}

/// Build CardRules from MTGJSON face data and an ability file.
fn build_card_rules(
    mtgjson_faces: &[AtomicCard],
    ability_file: &AbilityFile,
    oracle_id: Option<String>,
) -> CardRules {
    let layout_kind = map_layout(&mtgjson_faces[0].layout);

    let build_face = |face_idx: usize, face_abilities: &FaceAbilities| -> CardFace {
        let mtgjson_face = mtgjson_faces.get(face_idx).unwrap_or(&mtgjson_faces[0]);
        let face_oracle_id = mtgjson_face
            .identifiers
            .scryfall_oracle_id
            .clone()
            .or_else(|| oracle_id.clone());
        let mut face = build_card_face(mtgjson_face, face_abilities, face_oracle_id);
        synthesize_basic_land_mana(&mut face);
        synthesize_equip(&mut face);
        face
    };

    let layout = if mtgjson_faces.len() >= 2 && !ability_file.faces.is_empty() {
        // Multi-face card with per-face abilities
        let empty = FaceAbilities {
            abilities: vec![],
            triggers: vec![],
            statics: vec![],
            replacements: vec![],
        };
        let face_a = build_face(0, ability_file.faces.first().unwrap_or(&empty));
        let face_b = build_face(1, ability_file.faces.get(1).unwrap_or(&empty));
        match layout_kind {
            LayoutKind::Split => CardLayout::Split(face_a, face_b),
            LayoutKind::Flip => CardLayout::Flip(face_a, face_b),
            LayoutKind::Transform => CardLayout::Transform(face_a, face_b),
            LayoutKind::Meld => CardLayout::Meld(face_a, face_b),
            LayoutKind::Adventure => CardLayout::Adventure(face_a, face_b),
            LayoutKind::Modal => CardLayout::Modal(face_a, face_b),
            LayoutKind::Single => CardLayout::Single(face_a), // shouldn't happen
        }
    } else {
        // Single-face card: use flat ability file fields
        let flat_abilities = FaceAbilities {
            abilities: ability_file.abilities.clone(),
            triggers: ability_file.triggers.clone(),
            statics: ability_file.statics.clone(),
            replacements: ability_file.replacements.clone(),
        };
        let face = build_face(0, &flat_abilities);
        CardLayout::Single(face)
    };

    CardRules {
        layout,
        meld_with: None,
        partner_with: None,
    }
}

/// Normalize a card name for fuzzy matching by keeping only alphanumeric
/// chars and `/` (for multi-face separators), lowercased, with whitespace collapsed.
/// Handles apostrophes (e.g. "Healer's Hawk" matching "Healer S Hawk"),
/// commas (e.g. "Jace, the Mind Sculptor" matching "Jace The Mind Sculptor"),
/// and other punctuation differences between MTGJSON names and filename-derived names.
fn normalize_for_match(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric() || *c == '/')
        .collect::<String>()
        .to_lowercase()
}

/// Convert a snake_case filename stem to Title Case card name.
fn filename_to_card_name(stem: &str) -> String {
    stem.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    upper + &chars.collect::<String>()
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build fast lookup indexes from MTGJSON data for O(1) card name resolution.
/// Returns (lowercase_map, normalized_map, normalized_prefix_map).
fn build_mtgjson_indexes(
    atomic: &crate::database::mtgjson::AtomicCardsFile,
) -> (
    HashMap<String, &Vec<crate::database::mtgjson::AtomicCard>>,
    HashMap<String, &Vec<crate::database::mtgjson::AtomicCard>>,
    HashMap<String, &Vec<crate::database::mtgjson::AtomicCard>>,
) {
    let mut lowercase_map = HashMap::with_capacity(atomic.data.len());
    let mut normalized_map = HashMap::with_capacity(atomic.data.len());
    let mut normalized_prefix_map = HashMap::with_capacity(atomic.data.len());

    for (key, val) in &atomic.data {
        let key_lower = key.to_lowercase();
        lowercase_map.entry(key_lower.clone()).or_insert(val);
        let key_normalized = normalize_for_match(key);
        normalized_map.entry(key_normalized.clone()).or_insert(val);

        // Multi-face prefix: "name a // name b" → index by "name a"
        if let Some(idx) = key_lower.find(" // ") {
            let prefix = &key_lower[..idx];
            normalized_prefix_map
                .entry(prefix.to_string())
                .or_insert(val);
            let prefix_normalized = normalize_for_match(&key[..key.find(" // ").unwrap_or(0)]);
            normalized_prefix_map
                .entry(prefix_normalized)
                .or_insert(val);
        }
    }

    (lowercase_map, normalized_map, normalized_prefix_map)
}

/// Load a card database from MTGJSON metadata and per-card ability JSON files.
pub fn load_json(
    mtgjson_path: &Path,
    abilities_dir: &Path,
) -> Result<CardDatabase, Box<dyn Error>> {
    let atomic = load_atomic_cards(mtgjson_path)?;
    let (lowercase_map, normalized_map, prefix_map) = build_mtgjson_indexes(&atomic);

    let mut cards = HashMap::new();
    let mut face_index = HashMap::new();
    let mut errors: Vec<(PathBuf, String)> = Vec::new();

    // Walk abilities directory for .json files
    if abilities_dir.is_dir() {
        for entry in std::fs::read_dir(abilities_dir)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str());
            if ext != Some("json") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            // Skip schema.json
            if stem == "schema" {
                continue;
            }

            // Parse ability file
            let content = match std::fs::read_to_string(&path) {
                Ok(c) => c,
                Err(e) => {
                    errors.push((path.clone(), e.to_string()));
                    continue;
                }
            };
            let ability_file: AbilityFile = match serde_json::from_str(&content) {
                Ok(f) => f,
                Err(e) => {
                    errors.push((path.clone(), e.to_string()));
                    continue;
                }
            };

            // Derive card name from filename
            let card_name = filename_to_card_name(&stem);

            // Find matching MTGJSON entry via pre-built indexes (all O(1) lookups)
            let card_name_lower = card_name.to_lowercase();
            let card_name_normalized = normalize_for_match(&card_name);
            let mtgjson_entry = atomic
                .data
                .get(&card_name)
                .or_else(|| lowercase_map.get(&card_name_lower).copied())
                .or_else(|| normalized_map.get(&card_name_normalized).copied())
                .or_else(|| prefix_map.get(&card_name_lower).copied())
                .or_else(|| prefix_map.get(&card_name_normalized).copied());

            let mtgjson_faces = match mtgjson_entry {
                Some(faces) => faces,
                None => {
                    errors.push((
                        path.clone(),
                        format!("No MTGJSON match for card name: {}", card_name),
                    ));
                    continue;
                }
            };

            let oracle_id = mtgjson_faces
                .first()
                .and_then(|f| f.identifiers.scryfall_oracle_id.clone());

            let card_rules = build_card_rules(mtgjson_faces, &ability_file, oracle_id);

            // Index each face
            for face in layout_faces(&card_rules.layout) {
                face_index.insert(face.name.to_lowercase(), face.clone());
            }

            let name_key = card_rules.name().to_lowercase();
            cards.insert(name_key, card_rules);
        }
    }

    Ok(CardDatabase {
        cards,
        face_index,
        errors,
    })
}

/// Extract face references from a CardLayout.
fn layout_faces(layout: &CardLayout) -> Vec<&CardFace> {
    match layout {
        CardLayout::Single(face) => vec![face],
        CardLayout::Split(a, b)
        | CardLayout::Flip(a, b)
        | CardLayout::Transform(a, b)
        | CardLayout::Meld(a, b)
        | CardLayout::Adventure(a, b)
        | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b) => vec![a, b],
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![base];
            faces.extend(variants);
            faces
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::mtgjson::{AtomicCard, AtomicIdentifiers};
    use crate::types::mana::{ManaCost, ManaCostShard};

    fn make_test_atomic_card(name: &str) -> AtomicCard {
        AtomicCard {
            name: name.to_string(),
            mana_cost: Some("{1}{G}".to_string()),
            colors: vec!["G".to_string()],
            color_identity: vec!["G".to_string()],
            power: Some("2".to_string()),
            toughness: Some("2".to_string()),
            loyalty: None,
            defense: None,
            text: Some("Test creature.".to_string()),
            layout: "normal".to_string(),
            type_line: Some("Creature -- Bear".to_string()),
            types: vec!["Creature".to_string()],
            subtypes: vec!["Bear".to_string()],
            supertypes: vec![],
            keywords: Some(vec![]),
            side: None,
            face_name: None,
            mana_value: 2.0,
            legalities: HashMap::new(),
            identifiers: AtomicIdentifiers {
                scryfall_oracle_id: Some("test-oracle-id-123".to_string()),
            },
        }
    }

    fn empty_face_abilities() -> FaceAbilities {
        FaceAbilities {
            abilities: vec![],
            triggers: vec![],
            statics: vec![],
            replacements: vec![],
        }
    }

    #[test]
    fn build_card_face_produces_correct_fields() {
        let mtgjson = make_test_atomic_card("Grizzly Bears");
        let abilities = empty_face_abilities();
        let face = build_card_face(&mtgjson, &abilities, Some("oracle-123".to_string()));

        assert_eq!(face.name, "Grizzly Bears");
        assert_eq!(
            face.mana_cost,
            ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::Green],
            }
        );
        assert_eq!(face.power, Some(PtValue::Fixed(2)));
        assert_eq!(face.toughness, Some(PtValue::Fixed(2)));
        assert_eq!(face.oracle_text.as_deref(), Some("Test creature."));
        assert!(face.card_type.core_types.contains(&CoreType::Creature));
        assert_eq!(face.card_type.subtypes, vec!["Bear".to_string()]);
    }

    #[test]
    fn build_card_face_populates_scryfall_oracle_id() {
        let mtgjson = make_test_atomic_card("Test");
        let face = build_card_face(
            &mtgjson,
            &empty_face_abilities(),
            Some("my-oracle-id".to_string()),
        );
        assert_eq!(face.scryfall_oracle_id.as_deref(), Some("my-oracle-id"));
    }

    #[test]
    fn build_card_face_filters_unknown_keywords() {
        let mut mtgjson = make_test_atomic_card("Ruin-Lurker Bat");
        mtgjson.keywords = Some(vec![
            "Flying".to_string(),
            "Lifelink".to_string(),
            "Scry".to_string(),     // action keyword — not in enum
            "Mill".to_string(),     // action keyword — not in enum
            "Landfall".to_string(), // ability word — not in enum
        ]);

        let face = build_card_face(&mtgjson, &empty_face_abilities(), None);

        assert_eq!(face.keywords.len(), 2);
        assert!(face.keywords.contains(&Keyword::Flying));
        assert!(face.keywords.contains(&Keyword::Lifelink));
    }

    #[test]
    fn build_card_type_maps_correctly() {
        let mut mtgjson = make_test_atomic_card("Test");
        mtgjson.supertypes = vec!["Legendary".to_string()];
        mtgjson.types = vec!["Creature".to_string(), "Artifact".to_string()];
        mtgjson.subtypes = vec!["Golem".to_string()];

        let ct = build_card_type(&mtgjson);
        assert_eq!(ct.supertypes, vec![Supertype::Legendary]);
        assert!(ct.core_types.contains(&CoreType::Creature));
        assert!(ct.core_types.contains(&CoreType::Artifact));
        assert_eq!(ct.subtypes, vec!["Golem".to_string()]);
    }

    #[test]
    fn map_layout_normal_to_single() {
        assert_eq!(map_layout("normal"), LayoutKind::Single);
    }

    #[test]
    fn map_layout_transform() {
        assert_eq!(map_layout("transform"), LayoutKind::Transform);
    }

    #[test]
    fn map_layout_adventure() {
        assert_eq!(map_layout("adventure"), LayoutKind::Adventure);
    }

    #[test]
    fn map_layout_split() {
        assert_eq!(map_layout("split"), LayoutKind::Split);
    }

    #[test]
    fn map_layout_modal_dfc() {
        assert_eq!(map_layout("modal_dfc"), LayoutKind::Modal);
    }

    #[test]
    fn map_layout_unknown_to_single() {
        assert_eq!(map_layout("some_future_layout"), LayoutKind::Single);
    }

    #[test]
    fn synthesize_basic_land_mana_forest() {
        let mut face = CardFace {
            name: "Forest".to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                supertypes: vec![Supertype::Basic],
                core_types: vec![CoreType::Land],
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
        };

        synthesize_basic_land_mana(&mut face);

        assert_eq!(face.abilities.len(), 1);
        assert_eq!(face.abilities[0].kind, AbilityKind::Activated);
        match &face.abilities[0].effect {
            Effect::Mana { produced } => assert_eq!(*produced, vec![ManaColor::Green]),
            other => panic!("Expected Mana effect, got {:?}", other),
        }
        assert_eq!(face.abilities[0].cost, Some(AbilityCost::Tap));
    }

    #[test]
    fn synthesize_basic_land_mana_plains() {
        let mut face = CardFace {
            name: "Plains".to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                supertypes: vec![Supertype::Basic],
                core_types: vec![CoreType::Land],
                subtypes: vec!["Plains".to_string()],
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
        };

        synthesize_basic_land_mana(&mut face);

        assert_eq!(face.abilities.len(), 1);
        match &face.abilities[0].effect {
            Effect::Mana { produced } => assert_eq!(*produced, vec![ManaColor::White]),
            other => panic!("Expected Mana effect, got {:?}", other),
        }
    }

    #[test]
    fn synthesize_basic_land_mana_dual_type() {
        let mut face = CardFace {
            name: "Tundra".to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Land],
                subtypes: vec!["Plains".to_string(), "Island".to_string()],
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
        };

        synthesize_basic_land_mana(&mut face);

        assert_eq!(face.abilities.len(), 2);
        let produced: Vec<&Vec<ManaColor>> = face
            .abilities
            .iter()
            .filter_map(|a| match &a.effect {
                Effect::Mana { produced } => Some(produced),
                _ => None,
            })
            .collect();
        assert!(produced.contains(&&vec![ManaColor::White]));
        assert!(produced.contains(&&vec![ManaColor::Blue]));
    }

    #[test]
    fn synthesize_basic_land_mana_no_land_subtype() {
        let mut face = CardFace {
            name: "Wastes".to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                supertypes: vec![Supertype::Basic],
                core_types: vec![CoreType::Land],
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
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
        };

        synthesize_basic_land_mana(&mut face);
        assert!(face.abilities.is_empty());
    }

    #[test]
    fn synthesize_equip_with_cost() {
        let mut face = CardFace {
            name: "Bonesplitter".to_string(),
            mana_cost: ManaCost::Cost {
                generic: 1,
                shards: vec![],
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Equipment".to_string()],
            },
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![Keyword::Equip(ManaCost::Cost {
                generic: 1,
                shards: vec![],
            })],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
        };

        synthesize_equip(&mut face);

        assert_eq!(face.abilities.len(), 1);
        assert_eq!(face.abilities[0].kind, AbilityKind::Activated);
        assert_eq!(
            face.abilities[0].cost,
            Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 1,
                    shards: vec![],
                }
            })
        );
        match &face.abilities[0].effect {
            Effect::Attach { .. } => {} // expected
            other => panic!("Expected Attach effect, got {:?}", other),
        }
    }

    #[test]
    fn synthesize_equip_no_keyword() {
        let mut face = CardFace {
            name: "Torch".to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![Keyword::Flying],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
        };

        synthesize_equip(&mut face);
        assert!(face.abilities.is_empty());
    }

    #[test]
    fn synthesize_equip_variant_cost() {
        let mut face = CardFace {
            name: "Sword of Test".to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![Keyword::Equip(ManaCost::Cost {
                generic: 1,
                shards: vec![ManaCostShard::White],
            })],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            scryfall_oracle_id: None,
        };

        synthesize_equip(&mut face);

        assert_eq!(face.abilities.len(), 1);
        assert_eq!(
            face.abilities[0].cost,
            Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::White],
                }
            })
        );
    }

    #[test]
    fn color_override_set_when_colors_differ() {
        let mut mtgjson = make_test_atomic_card("Dryad Arbor");
        // Dryad Arbor: no mana cost but is green
        mtgjson.mana_cost = None;
        mtgjson.colors = vec!["G".to_string()];
        mtgjson.types = vec!["Land".to_string(), "Creature".to_string()];

        let face = build_card_face(&mtgjson, &empty_face_abilities(), None);
        // ManaCost::NoCost derives empty colors, but MTGJSON says Green
        assert_eq!(face.color_override, Some(vec![ManaColor::Green]));
    }

    #[test]
    fn color_override_not_set_when_colors_match() {
        let mtgjson = make_test_atomic_card("Grizzly Bears");
        // {1}{G} derives [Green], MTGJSON colors = ["G"] = [Green] -> match
        let face = build_card_face(&mtgjson, &empty_face_abilities(), None);
        assert!(face.color_override.is_none());
    }

    #[test]
    fn load_json_single_card() {
        let tmp = tempfile::tempdir().unwrap();
        let abilities_dir = tmp.path().join("abilities");
        std::fs::create_dir_all(&abilities_dir).unwrap();

        // Write a lightning bolt ability file
        let bolt_json = r#"{
            "abilities": [{
                "kind": "Spell",
                "effect": {
                    "type": "DealDamage",
                    "amount": { "type": "Fixed", "value": 3 },
                    "target": { "type": "Any" }
                }
            }]
        }"#;
        std::fs::write(abilities_dir.join("lightning_bolt.json"), bolt_json).unwrap();

        // Copy the test fixture as our MTGJSON data
        let fixture_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json");

        let db = load_json(&fixture_path, &abilities_dir).unwrap();
        assert_eq!(db.card_count(), 1);

        let bolt = db.get_by_name("Lightning Bolt").unwrap();
        match &bolt.layout {
            CardLayout::Single(face) => {
                assert_eq!(face.name, "Lightning Bolt");
                assert!(face.scryfall_oracle_id.is_some());
                assert_eq!(face.abilities.len(), 1);
            }
            other => panic!("Expected Single layout, got {:?}", other),
        }
    }

    #[test]
    fn load_json_reports_missing_ability_files() {
        let tmp = tempfile::tempdir().unwrap();
        let abilities_dir = tmp.path().join("abilities");
        std::fs::create_dir_all(&abilities_dir).unwrap();

        // Write an ability file for a card that doesn't exist in MTGJSON
        let json = r#"{ "abilities": [] }"#;
        std::fs::write(abilities_dir.join("nonexistent_card.json"), json).unwrap();

        let fixture_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json");

        let db = load_json(&fixture_path, &abilities_dir).unwrap();
        assert_eq!(db.card_count(), 0);
        assert!(!db.errors().is_empty());
        assert!(db.errors()[0].1.contains("No MTGJSON match"));
    }

    #[test]
    fn load_json_multi_face_card() {
        let tmp = tempfile::tempdir().unwrap();
        let abilities_dir = tmp.path().join("abilities");
        std::fs::create_dir_all(&abilities_dir).unwrap();

        // Write a multi-face ability file for Delver of Secrets
        let delver_json = r#"{
            "abilities": [],
            "faces": [
                { "abilities": [] },
                { "abilities": [] }
            ]
        }"#;
        std::fs::write(abilities_dir.join("delver_of_secrets.json"), delver_json).unwrap();

        let fixture_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json");

        let db = load_json(&fixture_path, &abilities_dir).unwrap();
        // card_count() returns max(cards, faces); Delver has 2 faces so face_index=2
        assert_eq!(db.card_count(), 2);

        let delver = db.get_by_name("Delver of Secrets").unwrap();
        match &delver.layout {
            CardLayout::Transform(face_a, face_b) => {
                assert_eq!(face_a.name, "Delver of Secrets");
                assert_eq!(face_b.name, "Insectile Aberration");
                assert!(face_a.scryfall_oracle_id.is_some());
            }
            other => panic!("Expected Transform layout, got {:?}", other),
        }

        // Both faces should be in the face index
        assert!(db.get_face_by_name("Delver of Secrets").is_some());
        assert!(db.get_face_by_name("Insectile Aberration").is_some());
    }

    #[test]
    fn filename_to_card_name_conversion() {
        assert_eq!(filename_to_card_name("lightning_bolt"), "Lightning Bolt");
        assert_eq!(
            filename_to_card_name("delver_of_secrets"),
            "Delver Of Secrets"
        );
        assert_eq!(filename_to_card_name("forest"), "Forest");
    }
}
