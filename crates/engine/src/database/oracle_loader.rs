use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::database::bracket_lists::BracketSignals;
use crate::database::card_db::{
    build_name_alias_index, build_search_face_keys, collect_creature_type_vocabulary, CardDatabase,
};
use crate::database::legality::normalize_legalities;
use crate::database::mtgjson::load_atomic_cards;
use crate::database::synthesis::{
    build_oracle_face, build_oracle_face_multi, layout_faces, map_layout, LayoutKind,
};
use crate::types::card::{CardFace, CardLayout, CardRules};

/// Load a card database from MTGJSON, running the Oracle text parser on each card.
pub fn load_from_mtgjson(mtgjson_path: &Path) -> Result<CardDatabase, Box<dyn Error>> {
    let atomic = load_atomic_cards(mtgjson_path)?;

    let mut cards: HashMap<String, CardRules> = HashMap::new();
    let mut face_index: HashMap<String, CardFace> = HashMap::new();
    let mut oracle_id_index: HashMap<String, Vec<String>> = HashMap::new();
    let mut face_order_index: HashMap<String, usize> = HashMap::new();
    let mut layout_index: HashMap<String, crate::types::card::LayoutKind> = HashMap::new();
    let mut bracket_signals_by_name: HashMap<String, BracketSignals> = HashMap::new();
    let mut legalities = HashMap::new();
    let errors: Vec<(PathBuf, String)> = Vec::new();

    for faces in atomic.data.values() {
        let oracle_id = faces
            .first()
            .and_then(|f| f.identifiers.scryfall_oracle_id.clone());

        let layout_kind = map_layout(&faces[0].layout);

        if faces.len() >= 2 {
            // B8: Multi-face cards use parser-extracted keywords only to prevent
            // MTGJSON cross-face keyword leakage (e.g., Saga back-face Flying on front).
            let face_a = build_oracle_face_multi(&faces[0], oracle_id.clone());
            let face_b = build_oracle_face_multi(&faces[1], oracle_id.clone());
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
                // CR 702.xxx: Prepare (Strixhaven) — Adventure-family two-face layout.
                LayoutKind::Prepare => CardLayout::Prepare(face_a, face_b),
                LayoutKind::Specialize => {
                    let mut variant_faces = vec![face_b];
                    for extra in faces.iter().skip(2) {
                        variant_faces.push(build_oracle_face_multi(extra, oracle_id.clone()));
                    }
                    CardLayout::Specialize(face_a, variant_faces)
                }
                LayoutKind::Single => CardLayout::Single(face_a),
            };
            for (face_idx, (face, source)) in layout_faces(&layout)
                .into_iter()
                .zip(faces.iter())
                .enumerate()
            {
                let key = face.name.to_lowercase();
                if let Some(oracle_id) = face.scryfall_oracle_id.clone() {
                    oracle_id_index
                        .entry(oracle_id.clone())
                        .or_default()
                        .push(key.clone());
                    face_order_index.insert(key.clone(), face_idx);
                    if let Some(runtime_kind) = runtime_layout_kind(layout_kind) {
                        layout_index.entry(oracle_id).or_insert(runtime_kind);
                    }
                }
                face_index.insert(key.clone(), face.clone());
                if source.is_game_changer {
                    bracket_signals_by_name.insert(
                        key.clone(),
                        BracketSignals {
                            game_changer: true,
                            ..Default::default()
                        },
                    );
                }
                if let Some(card_legalities) = legalities_by_name.get(&key).cloned() {
                    legalities.insert(key, card_legalities);
                }
            }
            let rules = CardRules {
                layout: layout.clone(),
                meld_with: None,
            };
            let primary_name = rules.name().to_lowercase();
            cards.insert(primary_name, rules);
        } else {
            let face = build_oracle_face(&faces[0], oracle_id);
            let key = face.name.to_lowercase();
            if let Some(oracle_id) = face.scryfall_oracle_id.clone() {
                oracle_id_index
                    .entry(oracle_id)
                    .or_default()
                    .push(key.clone());
                face_order_index.insert(key.clone(), 0);
            }
            let card_legalities = normalize_legalities(&faces[0].legalities);
            let rules = CardRules {
                layout: CardLayout::Single(face.clone()),
                meld_with: None,
            };
            cards.insert(key.clone(), rules);
            face_index.insert(key.clone(), face);
            if faces[0].is_game_changer {
                bracket_signals_by_name.insert(
                    key.clone(),
                    BracketSignals {
                        game_changer: true,
                        ..Default::default()
                    },
                );
            }
            if !card_legalities.is_empty() {
                legalities.insert(key, card_legalities);
            }
        }
    }

    let creature_type_vocabulary = collect_creature_type_vocabulary(face_index.values());
    let search_face_keys = build_search_face_keys(&face_index, &face_order_index);
    Ok(CardDatabase {
        cards,
        name_alias_index: build_name_alias_index(face_index.keys()),
        face_index,
        oracle_id_index,
        face_order_index,
        search_face_keys,
        layout_index,
        legalities,
        printings_index: HashMap::new(),
        rulings_index: HashMap::new(),
        errors,
        bracket_lists: Default::default(),
        bracket_signals_by_name,
        creature_type_vocabulary,
    })
}

fn runtime_layout_kind(layout_kind: LayoutKind) -> Option<crate::types::card::LayoutKind> {
    match layout_kind {
        LayoutKind::Split => Some(crate::types::card::LayoutKind::Split),
        LayoutKind::Flip => Some(crate::types::card::LayoutKind::Flip),
        LayoutKind::Transform => Some(crate::types::card::LayoutKind::Transform),
        LayoutKind::Meld => Some(crate::types::card::LayoutKind::Meld),
        LayoutKind::Adventure => Some(crate::types::card::LayoutKind::Adventure),
        LayoutKind::Modal => Some(crate::types::card::LayoutKind::Modal),
        LayoutKind::Prepare => Some(crate::types::card::LayoutKind::Prepare),
        LayoutKind::Single | LayoutKind::Specialize => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::search::CardSearchQuery;
    use std::path::Path;
    use tempfile::NamedTempFile;

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

    #[test]
    fn load_from_mtgjson_transform_dfc_search_uses_front_face_order() {
        let fixture = NamedTempFile::new().expect("temporary AtomicCards fixture");
        std::fs::write(
            fixture.path(),
            r#"{
                "data": {
                    "The Legend of Kyoshi // Avatar Kyoshi": [
                        {
                            "name": "The Legend of Kyoshi",
                            "colors": ["G"],
                            "colorIdentity": ["G"],
                            "layout": "transform",
                            "types": ["Enchantment"],
                            "subtypes": ["Saga"],
                            "supertypes": ["Legendary"],
                            "manaCost": "{4}{G}{G}",
                            "manaValue": 6.0,
                            "text": "",
                            "identifiers": { "scryfallOracleId": "o-kyoshi-raw" }
                        },
                        {
                            "name": "Avatar Kyoshi",
                            "colors": ["G"],
                            "colorIdentity": ["G"],
                            "layout": "transform",
                            "types": ["Creature"],
                            "subtypes": ["Avatar"],
                            "manaValue": 0.0,
                            "text": "",
                            "identifiers": { "scryfallOracleId": "o-kyoshi-raw" }
                        }
                    ]
                }
            }"#,
        )
        .expect("write AtomicCards fixture");

        let db = load_from_mtgjson(fixture.path()).expect("raw MTGJSON fixture loads");

        assert_eq!(
            db.get_face_by_oracle_id("o-kyoshi-raw")
                .map(|face| face.name.as_str()),
            Some("The Legend of Kyoshi")
        );

        let results = db.search(&CardSearchQuery {
            text: "avatar kyoshi".into(),
            ..Default::default()
        });
        assert_eq!(results.total, 1);
        assert_eq!(results.results[0].name, "The Legend of Kyoshi");
    }
}
