use std::collections::HashMap;

use engine::database::CardDatabase;
use engine::game::deck_loading::{DeckEntry, PlayerDeckPayload};
use engine::types::card::CardFace;
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
    let (companion, mut companion_missing) = resolve_entries(db, &deck.companion, "companion");
    missing.append(&mut companion_missing);
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
        companion,
        attraction_deck,
        planar_deck,
        scheme_deck,
        contraption_deck,
        signature_spell,
        sticker_sheets: deck.sticker_sheets.clone(),
        bracket_tier: deck.bracket_tier,
    })
}

/// A key that resolves back to this exact face.
///
/// A multi-face card's face can lose the short-name key to another entry;
/// `oracle_gen` then files it under `<lowercased name> [<oracle id>]`, and that
/// key is the only way back to it. Emitting the bare name for such a face would
/// substitute the winner on the next resolve.
fn round_trip_name(db: &CardDatabase, face: &CardFace) -> String {
    let Some(oracle_id) = face.scryfall_oracle_id.as_deref() else {
        return face.name.clone();
    };
    match db.get_face_by_name(&face.name) {
        // The bare name leads back to this exact face.
        Some(found) if found.scryfall_oracle_id.as_deref() == Some(oracle_id) => face.name.clone(),
        // Either another entry holds the short name, or no entry holds it at
        // all. Both mean the bare name does not lead back here, and the
        // oracle-id key is what does.
        Some(_) | None => format!("{} [{}]", face.name.to_lowercase(), oracle_id),
    }
}

fn expand_entries(db: &CardDatabase, entries: &[DeckEntry]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| {
            std::iter::repeat_n(round_trip_name(db, &entry.card), entry.count as usize)
        })
        .collect()
}

/// The inverse of [`resolve_deck`], for the seats whose submitted name list was
/// never retained. Lives beside its forward twin because the two must agree
/// about `DeckData`'s field set, and is an exhaustive struct literal so a new
/// field is a build error here rather than a silently defaulted one.
///
/// The guarantee is *re-resolves to the same cards*, not *reproduces the
/// submitted spelling*: names are canonical and case-folded duplicates
/// coalesce, so the recovered list is a fixed point from the first pass on.
pub fn deck_data_from_payload(db: &CardDatabase, payload: &PlayerDeckPayload) -> DeckData {
    DeckData {
        main_deck: expand_entries(db, &payload.main_deck),
        sideboard: expand_entries(db, &payload.sideboard),
        commander: expand_entries(db, &payload.commander),
        companion: expand_entries(db, &payload.companion),
        attraction_deck: expand_entries(db, &payload.attraction_deck),
        planar_deck: expand_entries(db, &payload.planar_deck),
        scheme_deck: expand_entries(db, &payload.scheme_deck),
        contraption_deck: expand_entries(db, &payload.contraption_deck),
        signature_spell: expand_entries(db, &payload.signature_spell),
        // CR 123.2c: a player has access to only the stickers on the chosen
        // sheets, so dropping this changes which stickers a restored player may
        // use. Reaches `engine::game::stickers::set_player_sticker_sheets`.
        sticker_sheets: payload.sticker_sheets.clone(),
        // Decides `validate_cedh_bracket`, which `start_game` runs whenever an
        // AI seat is at cEDH difficulty.
        bracket_tier: payload.bracket_tier,
    }
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
    fn the_inverse_preserves_the_non_section_fields() {
        let db = db_from(&["Forest"]);
        let mut source = deck(&["Forest"], &[], &[]);
        source.sticker_sheets = vec!["Vampire Champion Fury".to_string()];
        source.bracket_tier = engine::game::bracket_estimate::CommanderBracketTier::Cedh;

        let payload = resolve_deck(&db, &source).expect("source resolves");
        let recovered = deck_data_from_payload(&db, &payload);
        let round_tripped = resolve_deck(&db, &recovered).expect("recovered list resolves");

        assert_eq!(round_tripped.sticker_sheets, source.sticker_sheets);
        assert_eq!(round_tripped.bracket_tier, source.bracket_tier);
        // Both are load-bearing downstream: this one gates `start_game`.
        assert!(engine::database::legality::validate_cedh_bracket(&[&round_tripped]).is_ok());
    }

    #[test]
    fn the_inverse_preserves_the_card_multiset_under_case_folding() {
        let db = db_from(&["Forest"]);
        let payload = resolve_deck(&db, &deck(&["Forest", "forest", "Forest"], &[], &[]))
            .expect("case-folded duplicates resolve");
        // Reach guard: entries are keyed by the RAW submitted string, so the
        // two spellings really did stay apart on the way in — which is what
        // makes the coalescing below a measured property of the recovery.
        assert_eq!(payload.main_deck.len(), 2);
        assert_eq!(payload.main_deck[0].count, 2);
        assert_eq!(payload.main_deck[1].count, 1);

        let recovered = deck_data_from_payload(&db, &payload);
        assert_eq!(recovered.main_deck, vec!["Forest".to_string(); 3]);

        // A fixed point from the first pass on: the canonical names now
        // coalesce into a single entry that expands to the same multiset.
        let second_pass = resolve_deck(&db, &recovered).unwrap();
        assert_eq!(second_pass.main_deck.len(), 1);
        assert_eq!(
            deck_data_from_payload(&db, &second_pass).main_deck,
            recovered.main_deck
        );
    }

    #[test]
    fn the_inverse_emits_a_key_that_resolves_back_to_the_same_face() {
        // The shape `oracle_gen` mints for a homonym collision: the winner
        // holds the bare key, the loser only `<lowercased name> [<oracle id>]`.
        const WINNER: &str = "4457ed35-7c10-48c8-9776-456485fdf070";
        const LOSER: &str = "5963eef1-1022-42b1-8a0c-fc9850bfc2a3";
        let mut winner = card("Lightning Bolt");
        winner["scryfall_oracle_id"] = json!(WINNER);
        let mut loser = card("Lightning Bolt");
        loser["scryfall_oracle_id"] = json!(LOSER);
        let alias_key = format!("lightning bolt [{LOSER}]");
        let db = db_from_values(&[("lightning bolt", winner), (alias_key.as_str(), loser)]);

        let payload = resolve_deck(&db, &deck(&[&alias_key], &[], &[])).expect("alias resolves");
        // Reach guard: the fixture really produced the loser face, whose bare
        // name belongs to the winner.
        assert_eq!(payload.main_deck[0].card.name, "Lightning Bolt");
        assert_eq!(
            payload.main_deck[0].card.scryfall_oracle_id.as_deref(),
            Some(LOSER)
        );

        let recovered = deck_data_from_payload(&db, &payload);
        let round_tripped = resolve_deck(&db, &recovered).expect("recovered list resolves");

        assert_eq!(
            round_tripped.main_deck[0]
                .card
                .scryfall_oracle_id
                .as_deref(),
            Some(LOSER),
            "the recovered list must name this face, not the homonym that won"
        );
        // Control: the bare name — what a payload-only inverse emits — resolves
        // to a different card entirely.
        let bare = resolve_deck(&db, &deck(&["Lightning Bolt"], &[], &[])).unwrap();
        assert_eq!(
            bare.main_deck[0].card.scryfall_oracle_id.as_deref(),
            Some(WINNER)
        );
    }

    #[test]
    fn round_trip_name_uses_the_oracle_id_key_when_nothing_holds_the_bare_name() {
        // `oracle_gen` files a face that cannot claim the short name under
        // `<lowercased name> [<oracle id>]`. When no entry holds the bare name
        // at all, that key is the only way back to this face.
        let oracle_id = "oracle-Shifty Face";
        let storage_key = format!("shifty face [{oracle_id}]");
        let entries: serde_json::Map<String, Value> = [(storage_key.clone(), card("Shifty Face"))]
            .into_iter()
            .collect();
        let db = CardDatabase::from_json_str(&Value::Object(entries).to_string())
            .expect("fixture database parses");

        let face = db
            .get_face_by_name(&storage_key)
            .expect("the oracle-id key resolves");
        // Reach guard: the branch under test is only entered when the bare-name
        // lookup misses, so a fixture that still answered it would pass
        // vacuously.
        assert!(
            db.get_face_by_name("Shifty Face").is_none(),
            "fixture precondition: no entry holds the bare name"
        );

        let emitted = round_trip_name(&db, face);
        assert_eq!(emitted, storage_key);
        assert!(
            db.get_face_by_name(&emitted).is_some(),
            "the emitted name must resolve back to a face, or the seat loses its deck on restore"
        );
    }

    #[test]
    fn round_trip_name_keeps_the_bare_name_when_it_leads_back() {
        // Control for the case above: the common face still round-trips under
        // its short name, so the oracle-id form stays confined to the miss.
        let db = db_from(&["Forest"]);
        let face = db.get_face_by_name("Forest").expect("bare name resolves");
        assert_eq!(round_trip_name(&db, face), "Forest");
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
