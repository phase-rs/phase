//! Coverage metadata changes support classification, never parsed rules.

use std::collections::BTreeMap;
use std::io::Read;
use std::sync::LazyLock;

use engine::database::CardDatabase;
use engine::game::coverage::{analyze_coverage, card_face_gaps, CardCoverageResult};
use engine::game::deck_loading::default_scheme_deck_entries;
use engine::game::deck_validation::{evaluate_deck_compatibility, DeckCompatibilityRequest};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::confirmed_misparse::{ConfirmedMisparses, MisparseClass};
use engine::types::format::{GameFormat, SelectedFormat};
use flate2::read::GzDecoder;
use serde_json::{Map, Value};
use strum::IntoEnumIterator;

fn fixture() -> &'static Map<String, Value> {
    static RAW: LazyLock<Map<String, Value>> = LazyLock::new(|| {
        // Regeneration requires scripts/gen-test-fixture.py and gzip -9 -n.
        let mut json = String::new();
        GzDecoder::new(&include_bytes!("../fixtures/integration_cards.json.gz")[..])
            .read_to_string(&mut json)
            .unwrap();
        serde_json::from_str(&json).unwrap()
    });
    &RAW
}

fn one_registry(rows: &[(&str, MisparseClass)]) -> ConfirmedMisparses {
    let entries: Vec<String> = rows
        .iter()
        .map(|(key, class)| {
            format!(
                "{{ key = {}, class = {}, clauses = [\"census-cited clause\"] }}",
                serde_json::to_string(key).unwrap(),
                serde_json::to_string(class).unwrap(),
            )
        })
        .collect();
    ConfirmedMisparses::parse(&format!("face = [{}]", entries.join(", "))).unwrap()
}

fn one_row(key: &str, class: MisparseClass) -> ConfirmedMisparses {
    one_registry(&[(key, class)])
}

/// Preserve the original export envelope and every unmodified value. Only the
/// marker produced by the production stamper is replaced, including removal.
fn stamped_export(raw: &Map<String, Value>, list: &ConfirmedMisparses) -> Map<String, Value> {
    let mut faces: BTreeMap<&str, CardFace> = raw
        .iter()
        .map(|(key, value)| (key.as_str(), serde_json::from_value(value.clone()).unwrap()))
        .collect();
    list.stamp_all(faces.iter_mut().map(|(key, face)| (*key, face)));
    let mut result = raw.clone();
    for (key, face) in faces {
        let entry = result.get_mut(key).unwrap().as_object_mut().unwrap();
        match face.metadata.confirmed_misparse {
            Some(class) => {
                let metadata = entry
                    .entry("metadata")
                    .or_insert_with(|| Value::Object(Map::new()));
                metadata.as_object_mut().unwrap().insert(
                    "confirmed_misparse".to_string(),
                    serde_json::to_value(class).unwrap(),
                );
            }
            None => {
                if let Some(metadata) = entry.get_mut("metadata") {
                    let metadata = metadata.as_object_mut().unwrap();
                    metadata.remove("confirmed_misparse");
                    if metadata.is_empty() {
                        entry.remove("metadata");
                    }
                }
            }
        }
    }
    result
}

fn database(raw: &Map<String, Value>) -> CardDatabase {
    CardDatabase::from_json_str(&serde_json::to_string(raw).unwrap()).unwrap()
}

fn report(db: &CardDatabase) -> BTreeMap<String, CardCoverageResult> {
    analyze_coverage(db)
        .cards
        .into_iter()
        .map(|card| (card.card_face_key.clone().expect("exact export key"), card))
        .collect()
}

fn handlers(card: &CardCoverageResult) -> Vec<&str> {
    card.gap_details
        .iter()
        .map(|gap| gap.handler.as_str())
        .collect()
}

#[test]
fn confirmed_misparse_parser_validates_exact_identity_and_schema() {
    let rows = r#"face = [
            { key = "same", class = "wrong_count", clauses = ["census-cited clause"] },
            { key = "same [id]", class = "missing_effect", clauses = ["other clause"] },
        ]"#;
    let list = ConfirmedMisparses::parse(rows).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.class_of("same"), Some(MisparseClass::WrongCount));
    assert_eq!(
        list.class_of("same [id]"),
        Some(MisparseClass::MissingEffect)
    );
    assert_eq!(list.class_of("Same"), None);
    assert_eq!(list.iter().next().unwrap().clauses, ["census-cited clause"]);
    assert!(ConfirmedMisparses::parse("face = []").unwrap().is_empty());
    for invalid in [
        r#"face = [{ key = "same", class = "other", clauses = [] }, { key = "same", class = "other", clauses = [] }]"#,
        r#"face = [{ key = "same", class = "unknown", clauses = [] }]"#,
        r#"face = [{ key = "same", class = "other", clauses = [], why = "not allowed" }]"#,
        "face = []\nunknown = true",
    ] {
        assert!(ConfirmedMisparses::parse(invalid).is_err(), "{invalid}");
    }
}
#[test]
fn confirmed_misparse_round_trip_and_clear_preserve_unlisted_export() {
    let raw: Map<String, Value> = ["lightning bolt", "plains"]
        .into_iter()
        .map(|key| (key.to_string(), fixture()[key].clone()))
        .collect();
    let old = database(&raw);
    assert_eq!(
        old.get_face_by_name("Lightning Bolt")
            .unwrap()
            .metadata
            .confirmed_misparse,
        None
    );
    let list = one_row("lightning bolt", MisparseClass::WrongCount);
    let marked_raw = stamped_export(&raw, &list);
    assert_eq!(marked_raw["plains"], raw["plains"]);
    let marked = database(&marked_raw);
    assert_eq!(
        marked
            .get_face_by_name("Lightning Bolt")
            .unwrap()
            .metadata
            .confirmed_misparse,
        Some(MisparseClass::WrongCount)
    );
    let cleared = stamped_export(&marked_raw, &ConfirmedMisparses::empty());
    assert_eq!(cleared, raw);
    assert_eq!(
        database(&cleared)
            .get_face_by_name("Lightning Bolt")
            .unwrap()
            .metadata
            .confirmed_misparse,
        None
    );

    let absent = one_row("absent exact key", MisparseClass::Other);
    let mut face: CardFace = serde_json::from_value(raw["lightning bolt"].clone()).unwrap();
    let outcome = absent.stamp_all([("lightning bolt", &mut face)]);
    assert_eq!(outcome.marked, 0);
    assert_eq!(outcome.unmatched_keys, ["absent exact key"]);
}

#[test]
fn confirmed_misparse_each_class_changes_both_coverage_authorities() {
    // All classes share one database: `analyze_coverage` has a large fixed cost per call.
    const POOL: usize = 60;
    let mut candidates: Vec<&String> = fixture().keys().collect();
    candidates.sort();
    candidates.truncate(POOL);
    let pool: Map<String, Value> = candidates
        .iter()
        .map(|key| (key.to_string(), fixture()[key.as_str()].clone()))
        .collect();
    let baseline = database(&stamped_export(&pool, &ConfirmedMisparses::empty()));
    let before = report(&baseline);
    let classes: Vec<MisparseClass> = MisparseClass::iter().collect();
    // Reach guards folded into selection: no hidden-key faces, and every chosen
    // card is baseline-supported, gap-free, and has abilities to parse.
    let chosen: Vec<&String> = candidates
        .iter()
        .copied()
        .filter(|key| !key.contains('['))
        .filter(|key| {
            let row = &before[key.as_str()];
            row.supported
                && row.gap_details.is_empty()
                && !baseline
                    .get_face_by_name(key.as_str())
                    .expect("pool face")
                    .abilities
                    .is_empty()
        })
        .take(classes.len())
        .collect();
    assert_eq!(
        chosen.len(),
        classes.len(),
        "first {POOL} fixture keys must supply one distinct gap-free card per MisparseClass"
    );
    let raw: Map<String, Value> = chosen
        .iter()
        .map(|key| (key.to_string(), fixture()[key.as_str()].clone()))
        .collect();
    let rows: Vec<(&str, MisparseClass)> = chosen
        .iter()
        .zip(&classes)
        .map(|(key, class)| (key.as_str(), *class))
        .collect();
    let marked = database(&stamped_export(&raw, &one_registry(&rows)));
    let after = report(&marked);
    for (key, class) in rows {
        let marked_row = &after[key];
        assert_eq!(
            class.handler(),
            format!(
                "Misparse:{}",
                serde_json::to_value(class).unwrap().as_str().unwrap()
            )
        );
        assert!(!marked_row.supported, "{key}: {class:?}");
        assert_eq!(handlers(marked_row), [class.handler()], "{key}: {class:?}");
        assert_eq!(
            card_face_gaps(marked.get_face_by_name(key).unwrap()),
            [class.handler()],
            "{key}: {class:?}"
        );
        assert_eq!(
            serde_json::to_value(&marked_row.parse_details).unwrap(),
            serde_json::to_value(&before[key].parse_details).unwrap(),
            "{key}: {class:?}"
        );
    }
}

/// Every face is compared; printed-name collisions cannot collapse rows.
fn compare_population(raw: &Map<String, Value>, list: &ConfirmedMisparses, label: &str) {
    let baseline = database(&stamped_export(raw, &ConfirmedMisparses::empty()));
    let before = report(&baseline);
    drop(baseline);
    let marked = database(&stamped_export(raw, list));
    let after = report(&marked);
    assert_eq!(before.len(), raw.len());
    assert_eq!(after.len(), before.len());
    let mut previously_supported = 0;
    let mut already_unsupported = 0;
    let mut classes = BTreeMap::new();
    for (key, original) in &before {
        let changed = &after[key];
        assert_eq!(
            serde_json::to_value(&original.parse_details).unwrap(),
            serde_json::to_value(&changed.parse_details).unwrap(),
            "parse details: {key}"
        );
        if let Some(class) = list.class_of(key) {
            *classes.entry(class).or_insert(0usize) += 1;
            assert!(!changed.supported, "listed {key}");
            let mut expected = handlers(original);
            expected.push(class.handler());
            expected.sort_unstable();
            expected.dedup();
            assert_eq!(handlers(changed), expected, "listed gaps: {key}");
            assert!(
                card_face_gaps(marked.get_face_by_name(key).unwrap())
                    .iter()
                    .any(|gap| gap == class.handler()),
                "face gaps: {key}"
            );
            if original.supported {
                previously_supported += 1;
            } else {
                already_unsupported += 1;
            }
        } else {
            assert_eq!(
                serde_json::to_value(original).unwrap(),
                serde_json::to_value(changed).unwrap(),
                "unlisted {key}"
            );
        }
    }
    assert!(!list.is_empty(), "{label}: population must be nonempty");
    assert!(
        previously_supported + already_unsupported > 0,
        "{label}: listed-present witness"
    );
    assert!(
        previously_supported > 0,
        "{label}: real false-green witness"
    );
    eprintln!("{label}: F={previously_supported} U={already_unsupported} per_class={classes:?} compared={}", before.len());
}

#[test]
fn confirmed_misparse_fixture_population_and_unlisted_invariance() {
    compare_population(
        fixture(),
        &one_row("lightning bolt", MisparseClass::WrongCount),
        "fixture constructed population",
    );
    compare_population(
        fixture(),
        ConfirmedMisparses::committed(),
        "fixture committed population",
    );
}

#[test]
fn confirmed_misparse_full_export_population_and_unlisted_invariance() {
    let Some(raw) = super::support::shared_card_export_json() else {
        return;
    };
    compare_population(
        raw,
        &one_row("lightning bolt", MisparseClass::WrongCount),
        "full export constructed population",
    );
    compare_population(
        raw,
        ConfirmedMisparses::committed(),
        "full export committed population",
    );
}

#[test]
fn confirmed_misparse_full_compatibility_counts_unique_cards_without_changing_legality() {
    let raw: Map<String, Value> = ["lightning bolt", "plains", "absolving lammasu"]
        .into_iter()
        .map(|key| (key.to_string(), fixture()[key].clone()))
        .collect();
    let baseline = database(&stamped_export(&raw, &ConfirmedMisparses::empty()));
    assert!(card_face_gaps(baseline.get_face_by_name("Lightning Bolt").unwrap()).is_empty());
    assert!(card_face_gaps(baseline.get_face_by_name("Plains").unwrap()).is_empty());
    assert!(!card_face_gaps(baseline.get_face_by_name("Absolving Lammasu").unwrap()).is_empty());
    let mut main_deck = vec![
        "Lightning Bolt".to_string(),
        "Lightning Bolt".to_string(),
        "Absolving Lammasu".to_string(),
    ];
    main_deck.extend(std::iter::repeat_n("Plains".to_string(), 57));
    let request = DeckCompatibilityRequest {
        main_deck,
        selected_format: Some(SelectedFormat::Tag(GameFormat::Vintage)),
        ..Default::default()
    };
    let before = evaluate_deck_compatibility(&baseline, &request);
    assert_eq!(before.selected_format_compatible, Some(true));
    assert_eq!(before.format_legality["vintage"], "legal");
    let marked = database(&stamped_export(
        &raw,
        &one_row("lightning bolt", MisparseClass::WrongCount),
    ));
    let after = evaluate_deck_compatibility(&marked, &request);
    assert_eq!(before.format_legality, after.format_legality);
    assert_eq!(
        before.selected_format_compatible,
        after.selected_format_compatible
    );
    assert_eq!(
        before.selected_format_reasons,
        after.selected_format_reasons
    );
    assert_eq!(before.unknown_cards, after.unknown_cards);
    let before = before.coverage.unwrap();
    let after = after.coverage.unwrap();
    assert_eq!((before.supported_unique, before.total_unique), (2, 3));
    assert_eq!((after.supported_unique, after.total_unique), (1, 3));
    assert_eq!(
        after
            .unsupported_cards
            .iter()
            .map(|card| card.name.as_str())
            .collect::<Vec<_>>(),
        ["Absolving Lammasu", "Lightning Bolt"]
    );
    assert_eq!(
        serde_json::to_value(&before.unsupported_cards[0]).unwrap(),
        serde_json::to_value(&after.unsupported_cards[0]).unwrap()
    );
    assert_eq!(after.unsupported_cards[1].copies, 2);
    assert_eq!(after.unsupported_cards[1].gaps, ["Misparse:wrong_count"]);
}

/// CR 904.3: A scheme deck contains at least 20 Scheme cards. A scheme the
/// coverage authority holds unsupported is rejected by the same rule that
/// rejects any other unsupported scheme, so a marker must reach the deck gate.
#[test]
fn confirmed_misparse_scheme_rejects_marked_real_scheme() {
    const WITNESS: &str = "i call for slaughter";
    let witness = fixture()[WITNESS].clone();
    let mut raw: Map<String, Value> = Map::new();
    raw.insert("plains".to_string(), fixture()["plains"].clone());
    raw.insert(WITNESS.to_string(), witness.clone());
    // Control schemes share the witness's real face data under distinct names.
    for index in 1..=19 {
        let name = format!("Scheme {index}");
        let mut control = witness.clone();
        control["name"] = Value::String(name.clone());
        raw.insert(name.to_lowercase(), control);
    }
    let baseline = database(&stamped_export(&raw, &ConfirmedMisparses::empty()));
    let mut scheme_deck: Vec<String> = (1..=19).map(|index| format!("Scheme {index}")).collect();
    scheme_deck.push("I Call for Slaughter".to_string());
    for name in &scheme_deck {
        assert!(
            card_face_gaps(baseline.get_face_by_name(name).unwrap()).is_empty(),
            "{name} must be a gap-free control"
        );
    }
    let request = DeckCompatibilityRequest {
        main_deck: vec!["Plains".to_string(); 60],
        scheme_deck,
        selected_format: Some(SelectedFormat::Tag(GameFormat::Archenemy)),
        player_count: 4,
        ..Default::default()
    };
    let before = evaluate_deck_compatibility(&baseline, &request);
    assert_eq!(
        before.selected_format_compatible,
        Some(true),
        "{:?}",
        before.selected_format_reasons
    );

    let marked = database(&stamped_export(
        &raw,
        &one_row(WITNESS, MisparseClass::MissingEffect),
    ));
    let after = evaluate_deck_compatibility(&marked, &request);
    assert_eq!(after.selected_format_compatible, Some(false));
    assert_eq!(
        after.selected_format_reasons,
        ["Unsupported scheme cards: I Call for Slaughter"]
    );
}

#[test]
fn confirmed_misparse_hidden_key_is_not_a_printed_name_lookup() {
    let face = fixture()["lightning bolt"].clone();
    let raw: Map<String, Value> = [
        ("lightning bolt".to_string(), face.clone()),
        ("lightning bolt [hidden-id]".to_string(), face),
    ]
    .into_iter()
    .collect();
    let list = one_row("lightning bolt [hidden-id]", MisparseClass::WrongObject);
    let stamped = stamped_export(&raw, &list);
    assert_eq!(stamped["lightning bolt"], raw["lightning bolt"]);
    let db = database(&stamped);
    let results = report(&db);
    assert_eq!(results.len(), 2);
    assert!(results["lightning bolt"].supported);
    assert!(!results["lightning bolt [hidden-id]"].supported);
    assert_eq!(
        handlers(&results["lightning bolt [hidden-id]"]),
        ["Misparse:wrong_object"]
    );
    assert_eq!(
        db.face_iter()
            .find(|(key, _)| *key == "lightning bolt [hidden-id]")
            .unwrap()
            .1
            .name,
        "Lightning Bolt"
    );
}

#[test]
fn confirmed_misparse_removal_restores_only_marker_induced_support() {
    for key in ["lightning bolt", "a premonition of your demise"] {
        let raw: Map<String, Value> = [(key.to_string(), fixture()[key].clone())]
            .into_iter()
            .collect();
        let baseline = report(&database(&stamped_export(
            &raw,
            &ConfirmedMisparses::empty(),
        )));
        let marked_raw = stamped_export(&raw, &one_row(key, MisparseClass::MissingEffect));
        let marked = report(&database(&marked_raw));
        assert!(!marked[key].supported);
        assert!(handlers(&marked[key]).contains(&"Misparse:missing_effect"));
        let restored = report(&database(&stamped_export(
            &marked_raw,
            &ConfirmedMisparses::empty(),
        )));
        assert_eq!(
            serde_json::to_value(&restored[key]).unwrap(),
            serde_json::to_value(&baseline[key]).unwrap()
        );
        assert_eq!(restored[key].supported, key == "lightning bolt");
        if key != "lightning bolt" {
            assert!(!baseline[key].gap_details.is_empty());
            assert!(handlers(&baseline[key])
                .iter()
                .all(|gap| handlers(&marked[key]).contains(gap)));
        }
    }
}

fn eligible_schemes(db: &CardDatabase) -> usize {
    db.face_iter()
        .filter(|(_, face)| {
            face.card_type.core_types.contains(&CoreType::Scheme) && card_face_gaps(face).is_empty()
        })
        .count()
}

fn assert_default_schemes(db: &CardDatabase) {
    let selected = default_scheme_deck_entries(db);
    assert_eq!(selected.len(), 20);
    for entry in selected {
        assert_eq!(entry.count, 1);
        assert!(entry.card.card_type.core_types.contains(&CoreType::Scheme));
        assert!(
            card_face_gaps(&entry.card).is_empty(),
            "{}",
            entry.card.name
        );
    }
}

#[test]
fn confirmed_misparse_default_schemes_full_export() {
    let Some(raw) = super::support::shared_card_export_json() else {
        return;
    };
    let committed = ConfirmedMisparses::committed();
    assert!(!committed.is_empty(), "committed list must have rows");
    let mut present = 0;
    let mut listed_schemes = 0;
    for row in committed.iter() {
        if let Some(value) = raw.get(&row.key) {
            let face: CardFace = serde_json::from_value(value.clone()).unwrap();
            assert_eq!(
                face.metadata.confirmed_misparse,
                Some(row.class),
                "export must already carry marker: {}",
                row.key
            );
            present += 1;
            if face.card_type.core_types.contains(&CoreType::Scheme) {
                assert!(card_face_gaps(&face)
                    .iter()
                    .any(|gap| gap == row.class.handler()));
                listed_schemes += 1;
            }
        }
    }
    assert!(present > 0, "full export must contain committed markers");
    let baseline = database(&stamped_export(raw, &ConfirmedMisparses::empty()));
    let supported_before = eligible_schemes(&baseline);
    let excluded_schemes: Vec<_> = baseline
        .face_iter()
        .filter(|(key, face)| {
            committed.class_of(key).is_some()
                && face.card_type.core_types.contains(&CoreType::Scheme)
                && card_face_gaps(face).is_empty()
        })
        .map(|(key, _)| key.to_string())
        .collect();
    // Deleting the last listed Scheme row must not fail this test; the constructed row below
    // proves the marker reaches the default scheme deck whatever the census list still holds.
    let witness = baseline
        .get_face_by_name("I Call for Slaughter")
        .expect("real full-export Scheme witness");
    assert!(witness.card_type.core_types.contains(&CoreType::Scheme));
    assert!(
        card_face_gaps(witness).is_empty(),
        "real Scheme must start eligible"
    );
    let constructed = database(&stamped_export(
        raw,
        &one_row("i call for slaughter", MisparseClass::MissingEffect),
    ));
    assert_eq!(eligible_schemes(&constructed), supported_before - 1);
    assert_eq!(
        card_face_gaps(
            constructed
                .get_face_by_name("I Call for Slaughter")
                .unwrap()
        ),
        ["Misparse:missing_effect"]
    );
    assert_default_schemes(&constructed);
    drop(constructed);
    drop(baseline);
    let marked = database(&stamped_export(raw, committed));
    let supported_after = eligible_schemes(&marked);
    assert_eq!(supported_after, supported_before - excluded_schemes.len());
    for key in &excluded_schemes {
        let face = marked.get_face_by_name(key).unwrap();
        let class = committed.class_of(key).unwrap();
        assert_eq!(face.metadata.confirmed_misparse, Some(class));
        assert_eq!(card_face_gaps(face), [class.handler()]);
    }
    assert_default_schemes(&marked);
    eprintln!("confirmed_misparse_default_schemes_full_export executed full export: committed_rows={} present_markers={present} listed_real_schemes={listed_schemes} excluded_eligible_schemes={} supported_before={supported_before} supported_after={supported_after} selected=20", committed.len(), excluded_schemes.len());
}
