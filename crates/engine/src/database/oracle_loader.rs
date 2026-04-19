use std::collections::HashMap;
use std::error::Error;
use std::path::{Path, PathBuf};

use crate::database::card_db::CardDatabase;
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
            let face_b = build_oracle_face_multi(&faces[1], oracle_id);
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
        layout_index: HashMap::new(),
        legalities,
        printings_index: HashMap::new(),
        rulings_index: HashMap::new(),
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
