use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::database::card_db::CardDatabase;
use crate::database::legality::normalize_legalities;
use crate::database::mtgjson::{load_atomic_cards, parse_mtgjson_mana_cost, AtomicCard};
use crate::database::synthesis::{
    build_card_type, layout_faces, map_layout, map_mtgjson_color, parse_pt_value, synthesize_all,
    LayoutKind,
};
use crate::game::printed_cards::derive_colors_from_mana_cost;
use crate::parser::oracle::parse_oracle_text;
use crate::types::card::{CardFace, CardLayout, CardRules};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;

fn build_oracle_face(mtgjson: &AtomicCard, oracle_id: Option<String>) -> CardFace {
    let card_type = build_card_type(mtgjson);
    // Raw MTGJSON keyword names (lowercased) for keyword-only line detection
    let mtgjson_keyword_names: Vec<String> = mtgjson
        .keywords
        .as_ref()
        .map(|kws| kws.iter().map(|s| s.to_ascii_lowercase()).collect())
        .unwrap_or_default();

    // Parsed keywords from MTGJSON (filtering Unknown entries like bare "Protection")
    let mut keywords: Vec<Keyword> = mtgjson
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

    let parsed = parse_oracle_text(
        oracle_text,
        face_name,
        &mtgjson_keyword_names,
        &types,
        &subtypes,
    );

    // Merge keywords extracted from Oracle text (e.g. "protection from multicolored")
    // with MTGJSON-parsed keywords to form the complete set
    keywords.extend(parsed.extracted_keywords);

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
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
    };

    synthesize_all(&mut face);
    face
}

/// Load a card database from MTGJSON, running the Oracle text parser on each card.
pub fn load_from_mtgjson(mtgjson_path: &Path) -> Result<CardDatabase, Box<dyn Error>> {
    let atomic = load_atomic_cards(mtgjson_path)?;

    let mut cards: HashMap<String, CardRules> = HashMap::new();
    let mut face_index: HashMap<String, CardFace> = HashMap::new();
    let mut legalities = HashMap::new();
    let errors: Vec<(PathBuf, String)> = Vec::new();

    for faces in atomic.data.values() {
        let oracle_id = faces
            .first()
            .and_then(|f| f.identifiers.scryfall_oracle_id.clone());

        let layout_kind = map_layout(&faces[0].layout);

        if faces.len() >= 2 {
            let face_a = build_oracle_face(&faces[0], oracle_id.clone());
            let face_b = build_oracle_face(&faces[1], oracle_id);
            let mut legalities_by_name = HashMap::new();
            let legalities_a = normalize_legalities(&faces[0].legalities);
            if !legalities_a.is_empty() {
                legalities_by_name.insert(face_a.name.to_lowercase(), legalities_a);
            }
            let legalities_b = normalize_legalities(&faces[1].legalities);
            if !legalities_b.is_empty() {
                legalities_by_name.insert(face_b.name.to_lowercase(), legalities_b);
            }
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
                let key = face.name.to_lowercase();
                face_index.insert(key.clone(), face.clone());
                if let Some(card_legalities) = legalities_by_name.get(&key).cloned() {
                    legalities.insert(key, card_legalities);
                }
            }
            let rules = CardRules {
                layout: layout.clone(),
                meld_with: None,
                partner_with: None,
            };
            let primary_name = rules.name().to_lowercase();
            cards.insert(primary_name, rules);
        } else {
            let face = build_oracle_face(&faces[0], oracle_id);
            let key = face.name.to_lowercase();
            let card_legalities = normalize_legalities(&faces[0].legalities);
            let rules = CardRules {
                layout: CardLayout::Single(face.clone()),
                meld_with: None,
                partner_with: None,
            };
            cards.insert(key.clone(), rules);
            face_index.insert(key.clone(), face);
            if !card_legalities.is_empty() {
                legalities.insert(key, card_legalities);
            }
        }
    }

    Ok(CardDatabase {
        cards,
        face_index,
        oracle_id_index: HashMap::new(),
        legalities,
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
