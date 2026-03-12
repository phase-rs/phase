use std::collections::HashMap;

use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckEntry, PlayerDeckPayload};

use crate::protocol::DeckData;

fn resolve_entries(
    db: &CardDatabase,
    names: &[String],
    section: &str,
) -> (Vec<DeckEntry>, Vec<String>) {
    let mut counts: HashMap<&str, u32> = HashMap::new();
    for name in names {
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
                missing.push(format!("{}:{}", section, name));
            }
        }
    }

    (entries, missing)
}

/// Resolve a DeckData (card name strings) into a typed PlayerDeckPayload using a CardDatabase.
/// Groups duplicate names into a single DeckEntry with aggregated count.
/// Returns Err listing unresolvable card names if any lookup fails.
pub fn resolve_deck(db: &CardDatabase, deck: &DeckData) -> Result<PlayerDeckPayload, String> {
    let (main_deck, mut missing) = resolve_entries(db, &deck.main_deck, "main");
    let (sideboard, mut sideboard_missing) = resolve_entries(db, &deck.sideboard, "sideboard");
    missing.append(&mut sideboard_missing);

    if !missing.is_empty() {
        missing.sort();
        return Err(format!("Unresolvable card names: {}", missing.join(", ")));
    }

    Ok(PlayerDeckPayload {
        main_deck,
        sideboard,
    })
}
