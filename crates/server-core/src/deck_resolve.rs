use std::collections::HashMap;

use engine::database::CardDatabase;
use engine::game::deck_loading::DeckEntry;

use crate::protocol::DeckData;

/// Resolve a DeckData (card name strings) into Vec<DeckEntry> using a CardDatabase.
/// Groups duplicate names into a single DeckEntry with aggregated count.
/// Returns Err listing unresolvable card names if any lookup fails.
pub fn resolve_deck(db: &CardDatabase, deck: &DeckData) -> Result<Vec<DeckEntry>, String> {
    // Group names by occurrence count
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for name in &deck.main_deck {
        *counts.entry(name.as_str()).or_insert(0) += 1;
    }

    let mut entries = Vec::new();
    let mut missing = Vec::new();

    for (name, count) in &counts {
        match db.get_face_by_name(name) {
            Some(face) => {
                entries.push(DeckEntry {
                    card: face.clone(),
                    count: *count,
                });
            }
            None => {
                missing.push(name.to_string());
            }
        }
    }

    if !missing.is_empty() {
        missing.sort();
        return Err(format!("Unresolvable card names: {}", missing.join(", ")));
    }

    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn create_card_file(dir: &Path, name: &str, content: &str) {
        let file_path = dir.join(format!("{}.txt", name));
        fs::write(file_path, content).unwrap();
    }

    fn forest_content() -> &'static str {
        "Name:Forest\nManaCost:no cost\nTypes:Basic Land Forest\nOracle:({T}: Add {G}.)"
    }

    fn bolt_content() -> &'static str {
        "Name:Lightning Bolt\nManaCost:R\nTypes:Instant\nA:SP$ DealDamage | Cost$ R | NumDmg$ 3\nOracle:Lightning Bolt deals 3 damage to any target."
    }

    fn make_db(dir: &Path) -> CardDatabase {
        CardDatabase::load(dir).unwrap()
    }

    #[test]
    fn resolve_deck_all_valid_names() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "forest", forest_content());
        create_card_file(tmp.path(), "bolt", bolt_content());
        let db = make_db(tmp.path());

        let deck = DeckData {
            main_deck: vec![
                "Forest".to_string(),
                "Forest".to_string(),
                "Forest".to_string(),
                "Forest".to_string(),
                "Lightning Bolt".to_string(),
            ],
            sideboard: vec![],
        };

        let result = resolve_deck(&db, &deck);
        assert!(result.is_ok());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 2); // Two unique cards

        // Find Forest entry and check count
        let forest_entry = entries.iter().find(|e| e.card.name == "Forest").unwrap();
        assert_eq!(forest_entry.count, 4);

        let bolt_entry = entries
            .iter()
            .find(|e| e.card.name == "Lightning Bolt")
            .unwrap();
        assert_eq!(bolt_entry.count, 1);
    }

    #[test]
    fn resolve_deck_missing_name_returns_error() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "forest", forest_content());
        let db = make_db(tmp.path());

        let deck = DeckData {
            main_deck: vec![
                "Forest".to_string(),
                "Nonexistent Card".to_string(),
                "Also Missing".to_string(),
            ],
            sideboard: vec![],
        };

        let result = resolve_deck(&db, &deck);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Also Missing"));
        assert!(err.contains("Nonexistent Card"));
    }

    #[test]
    fn resolve_deck_empty_deck_returns_empty_vec() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "forest", forest_content());
        let db = make_db(tmp.path());

        let deck = DeckData {
            main_deck: vec![],
            sideboard: vec![],
        };

        let result = resolve_deck(&db, &deck);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn resolve_deck_deduplicates_with_aggregated_count() {
        let tmp = tempfile::tempdir().unwrap();
        create_card_file(tmp.path(), "forest", forest_content());
        let db = make_db(tmp.path());

        let deck = DeckData {
            main_deck: vec!["Forest".to_string(); 10],
            sideboard: vec![],
        };

        let result = resolve_deck(&db, &deck);
        assert!(result.is_ok());
        let entries = result.unwrap();
        assert_eq!(entries.len(), 1); // One unique card
        assert_eq!(entries[0].count, 10);
        assert_eq!(entries[0].card.name, "Forest");
    }
}
