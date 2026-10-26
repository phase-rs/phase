use std::collections::HashMap;

use engine::database::CardDatabase;
use engine::game::deck_loading::{
    normalize_deck_printings, round_trip_name, DeckEntry, PlayerDeckPayload,
};
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

    let mut payload = PlayerDeckPayload {
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
        printings: deck.printings.clone(),
        printing_entry_names: Vec::new(),
    };
    normalize_deck_printings(db, &mut payload);
    Ok(payload)
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
        // Display-only (CR 203.1 + CR 206.1): carried so a restored or re-paired
        // seat keeps the owner's printing choices.
        printings: payload.printings.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::card_printing::{CardPrinting, DeckPrinting, DeckSection};
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

    // Pre-existing P1 measured on fb7d812da, seed 4910, before loader edits.
    // Both submitted-header and inverse-header replay differ from server live:
    // (id == card_id, live oracle, replay oracle), with live/base printing None
    // on both sides: (1, Forest, Island), (2, Island, Forest),
    // (3, Island, Forest), (4, Forest, Island), (9, Forest, Island),
    // (11, Forest, Island), (12, Island, Forest), (15, Island, Forest).
    // Oracle strings above have the fixture prefix "oracle-". This diagnostic
    // records inherited ordering, not an expected-failure regression contract.
    #[test]
    fn phase10_preexisting_p1_diagnostic() {
        use engine::game::deck_loading::{
            load_and_hydrate_decks, DeckList, DeckPayload, PlayerDeckList,
        };
        use engine::game::engine::start_game_with_starting_player;
        use engine::game::replay::reconstruct_initial_state;
        use engine::types::{
            format::FormatConfig, game_state::GameState, player::PlayerId, replay::ReplayHeader,
        };
        use std::sync::Arc;
        let db = Arc::new(db_from(&["Forest", "Island"]));
        let source = deck(
            &[
                "Forest", "Island", "forest", "Forest", "Island", "forest", "Forest", "Island",
            ],
            &[],
            &[],
        );
        for annotations in [
            vec![],
            vec![
                selected(DeckSection::MainDeck, "Forest", 2, "P1-A"),
                selected(DeckSection::MainDeck, "Forest", 3, "P1-B"),
            ],
        ] {
            let mut source = source.clone();
            source.printings = annotations;
            let resolved = resolve_deck(&db, &source).expect("resolves");
            assert_eq!(
                resolved
                    .main_deck
                    .iter()
                    .map(|e| (e.card.name.as_str(), e.count))
                    .collect::<Vec<_>>(),
                [("Forest", 3), ("Island", 3), ("Forest", 2)]
            );
            let payload = DeckPayload {
                player: resolved.clone(),
                opponent: resolved.clone(),
                ..Default::default()
            };
            let mut live = GameState::new(FormatConfig::freeform(), 2, 4910);
            load_and_hydrate_decks(&mut live, &payload, Some(&db));
            let result = start_game_with_starting_player(&mut live, PlayerId(0));
            assert!(matches!(
                result.disposition,
                engine::types::game_state::ActionDisposition::Applied
            ));
            assert_eq!(
                live.players
                    .iter()
                    .map(|p| p.hand.len())
                    .collect::<Vec<_>>(),
                [7, 7]
            );
            assert_eq!(
                live.objects.values().filter(|o| o.name == "Forest").count(),
                10
            );
            assert_eq!(
                live.objects.values().filter(|o| o.name == "Island").count(),
                6
            );
            let reading = |s: &GameState| {
                let mut rows: Vec<_> = s
                    .objects
                    .values()
                    .map(|o| {
                        (
                            o.id.0,
                            o.card_id.0,
                            o.printed_ref.as_ref().unwrap().oracle_id.clone(),
                            o.printing.get().clone(),
                            o.base_printing.get().clone(),
                        )
                    })
                    .collect();
                rows.sort_by_key(|r| r.0);
                rows
            };
            for (label, input) in [
                ("submitted", source.clone()),
                ("inverse", deck_data_from_payload(&db, &resolved)),
            ] {
                let seat = PlayerDeckList {
                    main_deck: input.main_deck,
                    printings: input.printings,
                    ..Default::default()
                };
                let header = ReplayHeader {
                    format_config: FormatConfig::freeform(),
                    match_config: Default::default(),
                    player_count: 2,
                    first_player: Some(0),
                    seed: 4910,
                    deck_data: Some(DeckList {
                        player: seat.clone(),
                        opponent: seat,
                        ..Default::default()
                    }),
                };
                let decoded =
                    serde_json::from_str(&serde_json::to_string(&header).unwrap()).unwrap();
                let replay = reconstruct_initial_state(&decoded, Some(&db)).expect("replay");
                assert_eq!(
                    replay
                        .players
                        .iter()
                        .map(|p| p.hand.len())
                        .collect::<Vec<_>>(),
                    [7, 7]
                );
                let multiset = |state: &GameState| {
                    let mut rows: Vec<_> = state
                        .objects
                        .values()
                        .map(|object| {
                            assert_eq!(object.printing.get(), object.base_printing.get());
                            (
                                object.printed_ref.as_ref().unwrap().oracle_id.clone(),
                                object.printing.get().as_ref().map(|p| p.set_code.clone()),
                            )
                        })
                        .collect();
                    rows.sort();
                    rows
                };
                assert_eq!(multiset(&live), multiset(&replay));
                assert_eq!(
                    live.objects
                        .values()
                        .filter(|o| o.printing.get().is_some())
                        .count(),
                    if source.printings.is_empty() { 0 } else { 10 }
                );
                let differing: Vec<_> = reading(&live)
                    .into_iter()
                    .zip(reading(&replay))
                    .filter(|(a, b)| a != b)
                    .collect();
                println!(
                    "P1 {label} annotated={}: {differing:?}",
                    !source.printings.is_empty()
                );
            }
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

    /// One Forest annotation in the main deck: a fixed point of any later
    /// normalization (a resolvable name, a section that holds it, a count within
    /// the deck's).
    fn forest_printing() -> DeckPrinting {
        DeckPrinting {
            section: DeckSection::MainDeck,
            name: "Forest".to_string(),
            count: 1,
            printing: CardPrinting {
                set_code: "lea".to_string(),
                collector_number: "294".to_string(),
            },
        }
    }

    #[test]
    fn resolve_deck_carries_the_printings_sidecar() {
        let db = db_from(&["Forest"]);
        let mut deck = deck(&["Forest"], &[], &[]);
        deck.printings = vec![forest_printing()];

        let payload = resolve_deck(&db, &deck).expect("deck resolves");
        // Reach guard: the deck itself resolved, so the carry below is not a
        // by-product of an early return.
        assert_eq!(payload.main_deck.len(), 1);
        assert_eq!(payload.printings, deck.printings);
    }

    /// The mixed annotation array: fifteen elements of which exactly three decode. Kept, in
    /// order: a valid `main_deck` annotation, one whose section this build does not
    /// know (`Unknown`), and a `commander` annotation with an extra key. Dropped:
    /// count -1, 1.5 and 4294967296; a numeric section; a missing `printing`; a
    /// string `printing`; and scalar, array and nested-map elements. Copied verbatim
    /// into the engine integration test `owner_printing_sidecar.rs` (test code
    /// cannot be shared across the crates).
    const MIXED_PRINTINGS: &str = r#"[
        {"section":"main_deck","name":"Forest","count":2,"printing":{"set_code":"lea","collector_number":"294"}},
        {"section":"main_deck","name":"Forest","count":-1,"printing":{"set_code":"lea","collector_number":"294"}},
        {"section":"main_deck","name":"Forest","count":1.5,"printing":{"set_code":"lea","collector_number":"294"}},
        {"section":"main_deck","name":"Forest","count":4294967296,"printing":{"set_code":"lea","collector_number":"294"}},
        {"section":7,"name":"Forest","count":1,"printing":{"set_code":"lea","collector_number":"294"}},
        {"section":"main_deck","name":"Forest","count":1},
        {"section":"main_deck","name":"Forest","count":1,"printing":"lea/294"},
        7,
        "text",
        null,
        true,
        [1,2,[3]],
        {"a":{"b":[{"c":1}]}},
        {"section":"sticker_sheets","name":"Sheet","count":1,"printing":{"set_code":"lea","collector_number":"294"}},
        {"section":"commander","name":"Forest","count":1,"printing":{"set_code":"lea","collector_number":"294"},"extra":{"k":[1]}}
    ]"#;

    fn mixed_printings_kept() -> Vec<DeckPrinting> {
        let lea = || CardPrinting {
            set_code: "lea".to_string(),
            collector_number: "294".to_string(),
        };
        vec![
            DeckPrinting {
                section: DeckSection::MainDeck,
                name: "Forest".to_string(),
                count: 2,
                printing: lea(),
            },
            DeckPrinting {
                section: DeckSection::Unknown,
                name: "Sheet".to_string(),
                count: 1,
                printing: lea(),
            },
            DeckPrinting {
                section: DeckSection::Commander,
                name: "Forest".to_string(),
                count: 1,
                printing: lea(),
            },
        ]
    }

    /// The sidecar decodes leniently inside the real adjacently tagged frames, in both
    /// key orders. With `data` first serde buffers the whole `data` object through its
    /// private `Content` before any field decoder runs, so this reaches
    /// `deserialize_deck_printings` through `ContentDeserializer`. Reverting the
    /// `deserialize_with` attribute fails every frame here (the malformed elements
    /// become a decode error).
    #[test]
    fn the_printings_sidecar_decodes_leniently_inside_adjacently_tagged_deck_frames() {
        use seat_reducer::types::DeckChoice;

        let data_first = r#"{"data":{"main_deck":["Forest"],"printings":MIXED,"sticker_sheets":["Sheet"]},"type":"DeckList"}"#
            .replace("MIXED", MIXED_PRINTINGS);
        let type_first = r#"{"type":"DeckList","data":{"main_deck":["Forest"],"printings":MIXED,"sticker_sheets":["Sheet"]}}"#
            .replace("MIXED", MIXED_PRINTINGS);
        for (order, frame) in [("data first", data_first), ("type first", type_first)] {
            let choice: DeckChoice = serde_json::from_str(&frame)
                .unwrap_or_else(|err| panic!("{order}: the frame must decode: {err}"));
            let DeckChoice::DeckList(deck) = choice else {
                panic!("{order}: expected a DeckList choice");
            };
            assert_eq!(deck.printings, mixed_printings_kept(), "{order}");
            // Reach guard: the field after `printings` decoded, so the stream
            // continued past the dropped elements.
            assert_eq!(deck.sticker_sheets, vec!["Sheet".to_string()], "{order}");
        }

        // A non-array value is an empty sidecar, not a frame error.
        let non_array = r#"{"data":{"main_deck":["Forest"],"printings":5,"sticker_sheets":["Sheet"]},"type":"DeckList"}"#;
        let DeckChoice::DeckList(deck) =
            serde_json::from_str::<DeckChoice>(non_array).expect("a non-array sidecar decodes")
        else {
            panic!("expected a DeckList choice");
        };
        assert!(deck.printings.is_empty());
        assert_eq!(deck.sticker_sheets, vec!["Sheet".to_string()]);

        // The native server's frame type: `deck` first, so `display_name` and the
        // required `public` are read after the buffered `deck` object.
        let create = r#"{"data":{"deck":{"main_deck":["Forest"],"printings":MIXED,"sticker_sheets":["Sheet"]},"display_name":"Host","public":false},"type":"CreateGameWithSettings"}"#
            .replace("MIXED", MIXED_PRINTINGS);
        let message: crate::protocol::ClientMessage =
            serde_json::from_str(&create).expect("the create frame decodes");
        let crate::protocol::ClientMessage::CreateGameWithSettings {
            deck, display_name, ..
        } = message
        else {
            panic!("expected CreateGameWithSettings");
        };
        assert_eq!(deck.printings, mixed_printings_kept());
        assert_eq!(display_name, "Host");
    }

    /// A well-formed element whose object key is serde_json's private raw-value
    /// token never fails the deck. This lives in server-core, not the engine test
    /// binary, on purpose: `lobby-broker` turns on `serde_json/raw_value`, and
    /// cargo unifies it into every server-core build, while the engine-only graph
    /// has it off, so the row would be vacuous there. With `raw_value` on,
    /// `serde_json::Value`'s own `Deserialize` re-parses such an object as an
    /// embedded raw document and fails the deck; the decoder's own element
    /// materialiser never consults the token.
    #[test]
    fn a_raw_value_token_key_inside_an_element_never_fails_the_deck() {
        let frame = r#"{"main_deck":["Forest"],"printings":[
            {"$serde_json::private::RawValue":"not json"},
            {"section":"main_deck","name":"Forest","count":1,"printing":{"set_code":"lea","collector_number":"294"},"extra":{"$serde_json::private::RawValue":"not json"}}
        ],"sticker_sheets":["Sheet"]}"#;
        let deck: DeckData =
            serde_json::from_str(frame).expect("a token-keyed element must not fail the deck");
        assert_eq!(deck.printings, vec![forest_printing()]);
        assert_eq!(deck.sticker_sheets, vec!["Sheet".to_string()]);
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
        source.printings = vec![forest_printing()];

        let payload = resolve_deck(&db, &source).expect("source resolves");
        let recovered = deck_data_from_payload(&db, &payload);
        let round_tripped = resolve_deck(&db, &recovered).expect("recovered list resolves");

        assert_eq!(round_tripped.sticker_sheets, source.sticker_sheets);
        assert_eq!(round_tripped.bracket_tier, source.bracket_tier);
        // The printings sidecar is carried unchanged through the inverse.
        assert_eq!(round_tripped.printings, source.printings);
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
        let mut source = deck(&[storage_key.as_str(); 8], &[], &[]);
        source.printings = vec![selected(
            DeckSection::MainDeck,
            &storage_key,
            3,
            "qualified",
        )];
        let payload = resolve_deck(&db, &source).unwrap();
        assert_eq!(payload.printings, source.printings);
        let inverse = deck_data_from_payload(&db, &payload);
        let restored = resolve_deck(&db, &inverse).unwrap();
        assert_eq!(restored.printings, source.printings);
        for resolved in [&payload, &restored] {
            let state = loaded(
                &db,
                resolved,
                engine::types::format::FormatConfig::freeform(),
                0,
            );
            let objects: Vec<_> = state.objects.values().filter(|o| o.owner.0 == 0).collect();
            assert_eq!(objects.len(), 8);
            assert_eq!(
                objects
                    .iter()
                    .filter(
                        |o| o.printing.get() == &Some(source.printings[0].printing.clone())
                            && o.base_printing.get() == o.printing.get()
                    )
                    .count(),
                3
            );
        }
    }

    #[test]
    fn round_trip_name_keeps_the_bare_name_when_it_leads_back() {
        // Control for the case above: the common face still round-trips under
        // its short name, so the oracle-id form stays confined to the miss.
        let db = db_from(&["Forest"]);
        let face = db.get_face_by_name("Forest").expect("bare name resolves");
        assert_eq!(round_trip_name(&db, face), "Forest");
    }

    fn selected(section: DeckSection, name: &str, count: u32, set: &str) -> DeckPrinting {
        DeckPrinting {
            section,
            name: name.into(),
            count,
            printing: CardPrinting {
                set_code: set.into(),
                collector_number: "1".into(),
            },
        }
    }

    fn loaded(
        db: &CardDatabase,
        seat: &PlayerDeckPayload,
        format: engine::types::format::FormatConfig,
        mode: usize,
    ) -> engine::types::game_state::GameState {
        use engine::game::deck_loading::{
            load_and_hydrate_decks, load_deck_into_state, DeckPayload,
        };
        use engine::types::{
            game_state::{ActionDisposition, GameState, WaitingFor},
            player::PlayerId,
        };
        let payload = DeckPayload {
            player: seat.clone(),
            opponent: seat.clone(),
            ..Default::default()
        };
        let mut state = GameState::new(format, 2, 4910);
        assert!(state.card_db.is_none());
        match mode {
            0 => load_and_hydrate_decks(&mut state, &payload, Some(db)),
            1 => load_and_hydrate_decks(&mut state, &payload, None),
            2 => load_deck_into_state(&mut state, &payload),
            _ => unreachable!(),
        }
        if mode != 0 {
            assert!(state.card_db.is_none());
        }
        let result = engine::game::engine::start_game_with_starting_player(&mut state, PlayerId(0));
        assert!(matches!(result.disposition, ActionDisposition::Applied));
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision { .. }
        ));
        assert_eq!(
            state
                .players
                .iter()
                .map(|p| p.hand.len())
                .collect::<Vec<_>>(),
            [7, 7]
        );
        state
    }

    #[test]
    fn homonym_printings_survive_inverse_and_database_free_loading() {
        use engine::types::format::FormatConfig;
        let mut winner = card("Twin");
        winner["scryfall_oracle_id"] = json!("winner");
        let mut loser = card("Twin");
        loser["scryfall_oracle_id"] = json!("loser");
        let db = db_from_values(&[("twin", winner), ("twin [loser]", loser)]);
        for choice in ["Twin", "twin [loser]"] {
            for format in [FormatConfig::freeform(), FormatConfig::commander()] {
                let command = format.command_zone_holds_decklist_commander();
                let mut source = deck(
                    &[
                        "Twin",
                        "twin [loser]",
                        "twin [loser]",
                        "twin [loser]",
                        "TWIN",
                        "TWIN",
                        "TWIN",
                        "TWIN",
                    ],
                    &[],
                    if command { &["Twin"] } else { &[] },
                );
                source.printings =
                    vec![selected(DeckSection::MainDeck, choice, u32::MAX, "chosen")];
                let resolved = resolve_deck(&db, &source).unwrap();
                assert_eq!(
                    resolved
                        .main_deck
                        .iter()
                        .map(|e| e.count)
                        .collect::<Vec<_>>(),
                    [1, 3, 4]
                );
                assert_eq!(
                    resolved.printings,
                    [selected(
                        DeckSection::MainDeck,
                        choice,
                        if choice == "Twin" { 5 } else { 3 },
                        "chosen"
                    )]
                );
                let inverse = resolve_deck(&db, &deck_data_from_payload(&db, &resolved)).unwrap();
                assert_eq!(inverse.printings, resolved.printings);
                let decoded: PlayerDeckPayload =
                    serde_json::from_str(&serde_json::to_string(&resolved).unwrap()).unwrap();
                for payload in [&resolved, &inverse, &decoded] {
                    for mode in 0..3 {
                        let state = loaded(&db, payload, format.clone(), mode);
                        for owner in [0, 1] {
                            for (oracle, count) in [("winner", 5), ("loser", 3)] {
                                let objects: Vec<_> = state
                                    .objects
                                    .values()
                                    .filter(|o| {
                                        o.owner.0 == owner
                                            && o.printed_ref.as_ref().unwrap().oracle_id == oracle
                                    })
                                    .collect();
                                assert_eq!(objects.len(), count);
                                let selected_oracle =
                                    if choice == "Twin" { "winner" } else { "loser" };
                                let expected = if oracle == selected_oracle {
                                    count - usize::from(command && oracle == "winner")
                                } else {
                                    0
                                };
                                assert_eq!(
                                    objects
                                        .iter()
                                        .filter(|o| o.printing.get().is_some())
                                        .count(),
                                    expected
                                );
                                for object in objects {
                                    assert_eq!(object.printing.get(), object.base_printing.get());
                                    if let Some(printing) = object.printing.get() {
                                        assert_eq!(printing, &source.printings[0].printing);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn simultaneous_homonym_printings_keep_independent_budgets_and_objects() {
        let mut winner = card("Twin");
        winner["scryfall_oracle_id"] = json!("winner");
        let mut loser = card("Twin");
        loser["scryfall_oracle_id"] = json!("loser");
        let db = db_from_values(&[("twin", winner), ("twin [loser]", loser)]);
        let mut source = deck(
            &[
                "Twin",
                "twin [loser]",
                "TWIN",
                "twin [loser]",
                "TWIN",
                "twin [loser]",
                "TWIN",
                "TWIN",
            ],
            &[],
            &[],
        );
        source.printings = vec![
            selected(DeckSection::MainDeck, "Twin", u32::MAX, "winner-art"),
            selected(DeckSection::MainDeck, "twin [loser]", u32::MAX, "loser-art"),
        ];
        let expected = [
            selected(DeckSection::MainDeck, "Twin", 5, "winner-art"),
            selected(DeckSection::MainDeck, "twin [loser]", 3, "loser-art"),
        ];
        let resolved = resolve_deck(&db, &source).unwrap();
        assert_eq!(resolved.printings, expected);
        let inverse = deck_data_from_payload(&db, &resolved);
        assert_eq!(inverse.printings, expected);
        let restored = resolve_deck(&db, &inverse).unwrap();
        assert_eq!(restored.printings, expected);
        for payload in [&resolved, &restored] {
            for mode in 0..3 {
                let state = loaded(
                    &db,
                    payload,
                    engine::types::format::FormatConfig::freeform(),
                    mode,
                );
                assert_eq!(state.objects.len(), 16);
                for owner in [0, 1] {
                    for (oracle, count, printing) in [
                        ("winner", 5, &expected[0].printing),
                        ("loser", 3, &expected[1].printing),
                    ] {
                        let objects: Vec<_> = state
                            .objects
                            .values()
                            .filter(|object| {
                                object.owner.0 == owner
                                    && object.printed_ref.as_ref().unwrap().oracle_id == oracle
                            })
                            .collect();
                        assert_eq!(objects.len(), count);
                        for object in objects {
                            assert_eq!(object.name, "Twin");
                            assert_eq!(object.printing.get().as_ref(), Some(printing));
                            assert_eq!(object.base_printing.get().as_ref(), Some(printing));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn server_normalizes_duplicate_alias_budgets_and_invalid_rows() {
        let db = db_from(&["Forest", "Island"]);
        let mut source = deck(
            &[
                "Forest", "Island", "forest", "Forest", "Island", "forest", "Forest", "Island",
            ],
            &["Forest"],
            &[],
        );
        source.printings = vec![
            selected(DeckSection::Unknown, "Forest", 1, "invalid"),
            selected(DeckSection::MainDeck, "Forest", 0, "zero"),
            selected(DeckSection::MainDeck, "Missing", 9, "missing"),
            selected(DeckSection::Commander, "Forest", 9, "wrong-section"),
            selected(DeckSection::MainDeck, "FOREST", 2, "first"),
            selected(DeckSection::Sideboard, "Forest", 9, "side"),
            selected(DeckSection::MainDeck, "forest", u32::MAX, "second"),
        ];
        for invalid in ["", "bad\u{0085}value"] {
            for field in 0..3 {
                let mut row = selected(DeckSection::MainDeck, "Forest", 1, "invalid");
                match field {
                    0 => row.name = invalid.into(),
                    1 => row.printing.set_code = invalid.into(),
                    2 => row.printing.collector_number = invalid.into(),
                    _ => unreachable!(),
                }
                source.printings.insert(0, row);
            }
        }
        let resolved = resolve_deck(&db, &source).unwrap();
        assert_eq!(
            resolved.printings,
            [
                selected(DeckSection::MainDeck, "Forest", 2, "first"),
                selected(DeckSection::Sideboard, "Forest", 1, "side"),
                selected(DeckSection::MainDeck, "Forest", 3, "second"),
            ]
        );
        let state = loaded(
            &db,
            &resolved,
            engine::types::format::FormatConfig::freeform(),
            0,
        );
        for owner in [0, 1] {
            let mut prints: Vec<_> = state
                .objects
                .values()
                .filter(|o| o.owner.0 == owner)
                .filter_map(|o| {
                    assert_eq!(o.printing.get(), o.base_printing.get());
                    o.printing.get().as_ref().map(|p| p.set_code.as_str())
                })
                .collect();
            prints.sort();
            assert_eq!(prints, ["first", "first", "second", "second", "second"]);
        }
        source.main_deck.push("Missing".into());
        assert!(resolve_deck(&db, &source).is_err());
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
