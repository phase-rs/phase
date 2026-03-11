use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::database::card_db::CardDatabase;
use crate::database::mtgjson::{load_atomic_cards, parse_mtgjson_mana_cost, AtomicCard};
use crate::game::deck_loading::derive_colors_from_mana_cost;
use crate::parser::oracle::parse_oracle_text;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, PtValue, TargetFilter,
    TypeFilter,
};
use crate::types::card::{CardFace, CardLayout};
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

fn parse_pt_value(s: &str) -> PtValue {
    match s.parse::<i32>() {
        Ok(n) => PtValue::Fixed(n),
        Err(_) => PtValue::Variable(s.to_string()),
    }
}

fn synthesize_basic_land_mana(face: &mut CardFace) {
    let land_mana: Vec<(&str, ManaColor)> = vec![
        ("Plains", ManaColor::White),
        ("Island", ManaColor::Blue),
        ("Swamp", ManaColor::Black),
        ("Mountain", ManaColor::Red),
        ("Forest", ManaColor::Green),
    ];

    for (subtype, color) in land_mana {
        if face.card_type.subtypes.iter().any(|s| s == subtype) {
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

fn synthesize_equip(face: &mut CardFace) {
    let equip_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::Equip(cost) = kw {
                Some(AbilityDefinition {
                    kind: AbilityKind::Activated,
                    effect: Effect::Attach {
                        target: TargetFilter::Typed {
                            card_type: Some(TypeFilter::Creature),
                            subtype: None,
                            controller: Some(ControllerRef::You),
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

fn build_oracle_face(mtgjson: &AtomicCard, oracle_id: Option<String>) -> CardFace {
    let card_type = build_card_type(mtgjson);
    let keywords: Vec<Keyword> = mtgjson
        .keywords
        .as_ref()
        .map(|kws| {
            kws.iter()
                .map(|s| s.parse::<Keyword>().unwrap())
                .filter(|k| !matches!(k, Keyword::Unknown(_)))
                .collect()
        })
        .unwrap_or_default();

    let oracle_text = mtgjson.text.as_deref().unwrap_or("");
    let face_name = mtgjson.face_name.as_deref().unwrap_or(&mtgjson.name);

    let types: Vec<String> = mtgjson.types.clone();
    let subtypes: Vec<String> = mtgjson.subtypes.clone();

    let parsed = parse_oracle_text(oracle_text, face_name, &keywords, &types, &subtypes);

    let mana_cost = mtgjson
        .mana_cost
        .as_deref()
        .map(parse_mtgjson_mana_cost)
        .unwrap_or_default();

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

    let mut face = CardFace {
        name: face_name.to_string(),
        mana_cost,
        card_type,
        power: mtgjson.power.as_ref().map(|s| parse_pt_value(s)),
        toughness: mtgjson.toughness.as_ref().map(|s| parse_pt_value(s)),
        loyalty: mtgjson.loyalty.clone(),
        defense: mtgjson.defense.clone(),
        oracle_text: mtgjson.text.clone(),
        non_ability_text: None,
        flavor_name: None,
        keywords,
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        color_override,
        scryfall_oracle_id: oracle_id,
    };

    synthesize_basic_land_mana(&mut face);
    synthesize_equip(&mut face);
    face
}

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

/// Load a card database from MTGJSON, running the Oracle text parser on each card.
pub fn load_from_mtgjson(mtgjson_path: &Path) -> Result<CardDatabase, Box<dyn Error>> {
    let atomic = load_atomic_cards(mtgjson_path)?;

    let mut face_index: HashMap<String, CardFace> = HashMap::new();
    let errors: Vec<(PathBuf, String)> = Vec::new();

    for (_card_name, faces) in &atomic.data {
        let oracle_id = faces
            .first()
            .and_then(|f| f.identifiers.scryfall_oracle_id.clone());

        let layout_kind = map_layout(&faces[0].layout);

        if faces.len() >= 2 {
            let face_a = build_oracle_face(&faces[0], oracle_id.clone());
            let face_b = build_oracle_face(&faces[1], oracle_id);
            let layout = match layout_kind {
                LayoutKind::Split => CardLayout::Split(face_a, face_b),
                LayoutKind::Flip => CardLayout::Flip(face_a, face_b),
                LayoutKind::Transform => CardLayout::Transform(face_a, face_b),
                LayoutKind::Meld => CardLayout::Meld(face_a, face_b),
                LayoutKind::Adventure => CardLayout::Adventure(face_a, face_b),
                LayoutKind::Modal => CardLayout::Modal(face_a, face_b),
                LayoutKind::Single => CardLayout::Single(face_a),
            };
            for face in layout_faces(&layout) {
                face_index.insert(face.name.to_lowercase(), face.clone());
            }
        } else {
            let face = build_oracle_face(&faces[0], oracle_id);
            face_index.insert(face.name.to_lowercase(), face);
        }
    }

    Ok(CardDatabase {
        cards: HashMap::new(),
        face_index,
        errors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn load_from_mtgjson_test_fixture() {
        let fixture_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json");
        let db = load_from_mtgjson(&fixture_path).unwrap();

        // Test fixture should have cards
        assert!(db.card_count() > 0);
        assert!(db.errors().is_empty());

        // Lightning Bolt should be parseable
        let bolt = db.get_face_by_name("Lightning Bolt").unwrap();
        assert_eq!(bolt.name, "Lightning Bolt");
        assert!(bolt.oracle_text.is_some());
    }
}
