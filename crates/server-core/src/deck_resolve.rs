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

