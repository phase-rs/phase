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
                // CR 202.3d + CR 709.4b: build through the engine's single
                // authority so a split card in a server-resolved deck carries the
                // combined off-stack mana value override, matching the in-engine
                // resolver. A direct `DeckEntry { card: face.clone(), .. }` here
                // skipped the override, so server-side companion checks
                // (Keruga / Lurrus / Obosh) read only the submitted face's value.
                entries.push(DeckEntry::from_resolved_face(db, face, counts[name]));
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
    let (attraction_deck, mut attraction_missing) =
        resolve_entries(db, &deck.attraction_deck, "attraction_deck");
    missing.append(&mut attraction_missing);
    let (planar_deck, mut planar_missing) = resolve_entries(db, &deck.planar_deck, "planar_deck");
    missing.append(&mut planar_missing);
    let (scheme_deck, mut scheme_missing) = resolve_entries(db, &deck.scheme_deck, "scheme_deck");
    missing.append(&mut scheme_missing);
    let (contraption_deck, mut contraption_missing) =
        resolve_entries(db, &deck.contraption_deck, "contraption_deck");
    missing.append(&mut contraption_missing);
    let (signature_spell, mut sig_missing) =
        resolve_entries(db, &deck.signature_spell, "signature_spell");
    missing.append(&mut sig_missing);

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
        attraction_deck,
        planar_deck,
        scheme_deck,
        contraption_deck,
        signature_spell,
        sticker_sheets: deck.sticker_sheets.clone(),
        bracket_tier: deck.bracket_tier,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    fn deck(main: &[&str], sideboard: &[&str], commander: &[&str]) -> DeckData {
        fn v(s: &[&str]) -> Vec<String> {
            s.iter().map(|x| x.to_string()).collect()
        }
        DeckData {
            main_deck: v(main),
            sideboard: v(sideboard),
            commander: v(commander),
            ..Default::default()
        }
    }

    #[test]
    fn resolve_deck_preserves_selected_sticker_sheets() {
        let db = db_from(&["Forest"]);
        let mut deck = deck(&["Forest"], &[], &[]);
        deck.sticker_sheets = vec![
            "Vampire Champion Fury".to_string(),
            "Wild Ogre Bupkis".to_string(),
        ];

        let payload = resolve_deck(&db, &deck).expect("deck resolves");
        assert_eq!(payload.sticker_sheets, deck.sticker_sheets);
    }

    fn card(name: &str) -> Value {
        json!({
            "name": name,
            "mana_cost": { "type": "Cost", "shards": [], "generic": 0 },
            "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
            "power": null,
            "toughness": null,
            "loyalty": null,
            "defense": null,
            "oracle_text": null,
            "non_ability_text": null,
            "flavor_name": null,
            "keywords": [],
            "abilities": [],
            "triggers": [],
            "static_abilities": [],
            "replacements": [],
            "color_override": null,
            "color_identity": [],
            "scryfall_oracle_id": format!("oracle-{name}"),
        })
    }

    fn db_from(names: &[&str]) -> CardDatabase {
        let entries: serde_json::Map<String, Value> = names
            .iter()
            .map(|name| (name.to_lowercase(), card(name)))
            .collect();
        CardDatabase::from_json_str(&Value::Object(entries).to_string()).unwrap()
    }

    /// One half of a split card: the shared `scryfall_oracle_id` and the
    /// `"layout": "split"` discriminant make `CardDatabase` fold the two faces
    /// into a single split card for off-stack characteristic queries.
    fn split_face(name: &str, oracle_id: &str, generic: u32) -> Value {
        json!({
            "name": name,
            "mana_cost": { "type": "Cost", "shards": [], "generic": generic },
            "card_type": { "supertypes": [], "core_types": ["Instant"], "subtypes": [] },
            "power": null,
            "toughness": null,
            "loyalty": null,
            "defense": null,
            "oracle_text": null,
            "non_ability_text": null,
            "flavor_name": null,
            "keywords": [],
            "abilities": [],
            "triggers": [],
            "static_abilities": [],
            "replacements": [],
            "color_override": null,
            "color_identity": [],
            "scryfall_oracle_id": oracle_id,
            "layout": "split",
        })
    }

    fn db_from_values(cards: &[(&str, Value)]) -> CardDatabase {
        let entries: serde_json::Map<String, Value> = cards
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect();
        CardDatabase::from_json_str(&Value::Object(entries).to_string()).unwrap()
    }

    /// CR 202.3d + CR 709.4b: a split card resolved through the SERVER transport
    /// resolver must carry the combined off-stack mana value, not just the
    /// submitted front face's. Regression for the fix that routes
    /// `resolve_entries` through `DeckEntry::from_resolved_face`: before it, the
    /// server cloned the face directly and server-side companion checks
    /// (Keruga / Lurrus / Obosh) saw only the front half's mana value.
    #[test]
    fn resolve_entries_stamps_split_card_off_stack_mana_value_override() {
        // Commit // Memory analog: front half MV 3, back half MV 4 → combined
        // off-stack MV 7. A deck holding only the "Commit" face must expose 7.
        let db = db_from_values(&[
            ("commit", split_face("Commit", "o-commit-memory", 3)),
            ("memory", split_face("Memory", "o-commit-memory", 4)),
        ]);

        let (entries, missing) = resolve_entries(&db, &["Commit".to_string()], "main");
        assert!(missing.is_empty(), "split face resolves: {missing:?}");
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];

        // The submitted face's own mana value is only 3 …
        assert_eq!(
            entry.card.mana_cost.mana_value(),
            3,
            "front face raw mana value"
        );
        // … but the server-resolved entry must expose the COMBINED off-stack MV.
        assert_eq!(
            entry.off_stack_mana_value(),
            7,
            "server-resolved split card must report the combined off-stack mana value \
             (CR 202.3d/709.4b), not the front face's — otherwise companion eligibility \
             is evaluated against the wrong value"
        );
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
    fn resolve_deck_dedups_resolved_entries_in_first_appearance_order() {
        let db = db_from(&["Forest", "Lightning Bolt", "Shock"]);
        let payload = resolve_deck(
            &db,
            &deck(&["Forest", "Lightning Bolt", "Forest", "Shock"], &[], &[]),
        )
        .unwrap();

        let entries: Vec<_> = payload
            .main_deck
            .iter()
            .map(|entry| (entry.card.name.as_str(), entry.count))
            .collect();
        assert_eq!(
            entries,
            [("Forest", 2), ("Lightning Bolt", 1), ("Shock", 1)]
        );
    }

    #[test]
    fn resolve_deck_aggregates_missing_across_sections_in_sorted_order() {
        let db = CardDatabase::default();
        let err = resolve_deck(&db, &deck(&["Zed"], &["Alpha"], &["Mid"])).unwrap_err();

        let c = err.find("commander:Mid").expect("commander entry present");
        let m = err.find("main:Zed").expect("main entry present");
        let s = err
            .find("sideboard:Alpha")
            .expect("sideboard entry present");
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
