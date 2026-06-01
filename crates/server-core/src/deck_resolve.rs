use std::collections::HashMap;

use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckEntry, PlayerDeckPayload};
use tracing::warn;

use crate::protocol::DeckData;

fn resolve_entries(
    db: &CardDatabase,
    names: &[String],
    section: &str,
) -> (Vec<DeckEntry>, Vec<String>) {
    // Count copies while recording first-appearance order. Iterating the
    // `counts` HashMap directly (as before) produced the resolved `entries`
    // and the `missing` list in a randomized, run-to-run order, which is a
    // reproducibility hazard for a seeded engine. Resolve in deterministic
    // input order instead.
    let mut counts: HashMap<&str, u32> = HashMap::new();
    let mut order: Vec<&str> = Vec::new();
    for name in names {
        let count = counts.entry(name.as_str()).or_insert(0);
        if *count == 0 {
            order.push(name.as_str());
        }
        *count += 1;
    }

    let mut entries = Vec::new();
    let mut missing = Vec::new();

    for name in order {
        match db.get_face_by_name(name) {
            Some(face) => {
                entries.push(DeckEntry {
                    card: face.clone(),
                    count: counts[name],
                });
            }
            None => {
                missing.push(format!("{section}:{name}"));
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
    let (commander, mut commander_missing) = resolve_entries(db, &deck.commander, "commander");
    missing.append(&mut commander_missing);

    if !missing.is_empty() {
        missing.sort();
        warn!(
            missing_count = missing.len(),
            "deck contains unresolvable card names"
        );
        return Err(format!("Unresolvable card names: {}", missing.join(", ")));
    }

    Ok(PlayerDeckPayload {
        main_deck,
        sideboard,
        commander,
        bracket_tier: deck.bracket_tier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deck(main: &[&str], sideboard: &[&str], commander: &[&str]) -> DeckData {
        fn v(s: &[&str]) -> Vec<String> {
            s.iter().map(|x| x.to_string()).collect()
        }
        DeckData {
            main_deck: v(main),
            sideboard: v(sideboard),
            commander: v(commander),
            bracket_tier: Default::default(),
        }
    }

    #[test]
    fn resolve_entries_dedups_and_preserves_first_appearance_order() {
        // An empty database leaves every name unresolved, so the dedup and
        // ordering behavior is observable through `missing` without needing
        // real card data.
        let db = CardDatabase::default();
        let names = [
            "Bolt".to_string(),
            "Forest".to_string(),
            "Bolt".to_string(),
            "Island".to_string(),
        ];
        let (entries, missing) = resolve_entries(&db, &names, "main");

        assert!(entries.is_empty());
        // Deduplicated ("Bolt" appears once) and in first-appearance order —
        // not the randomized HashMap iteration order.
        let missing: Vec<&str> = missing.iter().map(String::as_str).collect();
        assert_eq!(missing, ["main:Bolt", "main:Forest", "main:Island"]);
    }

    #[test]
    fn resolve_deck_aggregates_missing_across_sections_in_sorted_order() {
        let db = CardDatabase::default();
        let err = resolve_deck(&db, &deck(&["Zed"], &["Alpha"], &["Mid"])).unwrap_err();

        let c = err.find("commander:Mid").expect("commander entry present");
        let m = err.find("main:Zed").expect("main entry present");
        let s = err.find("sideboard:Alpha").expect("sideboard entry present");
        // Sorted alphabetically: commander: < main: < sideboard:
        assert!(c < m && m < s, "missing names not sorted: {err}");
    }

    #[test]
    fn resolve_deck_with_unresolved_name_errors() {
        let db = CardDatabase::default();
        assert!(resolve_deck(&db, &deck(&["Nonexistent Card"], &[], &[])).is_err());
    }

    #[test]
    fn resolve_deck_empty_deck_is_ok() {
        let db = CardDatabase::default();
        let payload = resolve_deck(&db, &deck(&[], &[], &[])).unwrap();
        assert!(payload.main_deck.is_empty());
        assert!(payload.sideboard.is_empty());
        assert!(payload.commander.is_empty());
    }
}
