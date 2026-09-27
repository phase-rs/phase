use std::collections::BTreeSet;

use engine::analysis::ResourceAxis;
use engine::database::combo_table::{
    combo_cardinality, detect_combos, ComboCardinality, ComboCoverage, ComboEntry,
    ComboFilterCounts, ComboMatch, ComboOmission, ComboOutcome, ComboPiece, ComboPieceZone,
    ComboProvenance, ComboRelevance, ComboResource, ComboSetup, ComboTable, ComboTableDoc,
    ComboTableError, SignalSource,
};
use engine::game::bracket_estimate::BracketAxis;
use engine::game::deck_loading::PlayerDeckList;
use serde_json::Value;

fn provenance() -> ComboProvenance {
    ComboProvenance {
        snapshot_date: "2026-09-07".to_owned(),
        table_version: "1".to_owned(),
        attribution: "Commander Spellbook".to_owned(),
        card_pool_version: "2026-09-01".to_owned(),
        filtered: ComboFilterCounts {
            kept: 1,
            ..ComboFilterCounts::default()
        },
        omitted: vec![
            ComboOmission::PrerequisiteText,
            ComboOmission::ResultText,
            ComboOmission::UnmodeledResultClasses,
        ],
    }
}

fn piece(key: &str, display: &str, zone: ComboPieceZone) -> ComboPiece {
    ComboPiece {
        key: key.to_owned(),
        display: display.to_owned(),
        zone,
    }
}

fn entry(first: ComboPiece, second: ComboPiece) -> ComboEntry {
    ComboEntry {
        pieces: [first, second],
        relevance: ComboRelevance::Standalone,
        setup: ComboSetup::AsPrinted,
        mana_value_needed: 2,
        assemble_cost: 7,
        popularity: 42,
        outcomes: BTreeSet::from([
            ComboOutcome::Unbounded(ComboResource::Mana),
            ComboOutcome::Unbounded(ComboResource::Draw),
        ]),
        axes: BTreeSet::from([BracketAxis::MassLandDenial]),
    }
}

fn doc(entries: Vec<ComboEntry>) -> ComboTableDoc {
    ComboTableDoc {
        provenance: provenance(),
        entries,
    }
}

fn deck(commander: &[&str], main_deck: &[&str]) -> PlayerDeckList {
    PlayerDeckList {
        commander: commander.iter().map(|name| (*name).to_owned()).collect(),
        main_deck: main_deck.iter().map(|name| (*name).to_owned()).collect(),
        ..PlayerDeckList::default()
    }
}

fn table_entries(table: &ComboTable) -> Vec<ComboEntry> {
    (0..table.len())
        .map(|index| {
            table
                .entry(u32::try_from(index).expect("test table index fits in u32"))
                .expect("entry exists")
                .clone()
        })
        .collect()
}

#[test]
fn combo_table_roundtrips_through_serde() {
    let original = doc(vec![entry(
        piece(
            "basalt monolith",
            "Basalt Monolith",
            ComboPieceZone::Anywhere,
        ),
        piece(
            "rings of brighthearth",
            "Rings of Brighthearth",
            ComboPieceZone::Anywhere,
        ),
    )]);
    let raw = serde_json::to_string(&original).expect("document serializes");
    let table = ComboTable::from_json_str(&raw).expect("document loads");

    assert_eq!(table.provenance(), Some(&original.provenance));
    assert_eq!(table_entries(&table), original.entries);
    assert_eq!(
        serde_json::to_string(&ComboOutcome::Unbounded(ComboResource::Mana))
            .expect("outcome serializes"),
        r#"{"kind":"unbounded","resource":"mana"}"#
    );
}

#[test]
fn absent_table_reports_unmeasured() {
    let table = ComboTable::default();

    assert_eq!(table.coverage(), ComboCoverage::Unmeasured);
    assert_eq!(table.provenance(), None);
    assert!(table.entries_for("anything").is_empty());
    assert!(table.is_empty());
}

#[test]
fn by_card_index_finds_both_pieces() {
    let table = ComboTable::from_doc(doc(vec![
        entry(
            piece("alpha", "Alpha", ComboPieceZone::Anywhere),
            piece("beta", "Beta", ComboPieceZone::Anywhere),
        ),
        entry(
            piece("gamma", "Gamma", ComboPieceZone::Anywhere),
            piece("delta", "Delta", ComboPieceZone::Anywhere),
        ),
    ]));

    assert_eq!(table.entries_for("alpha"), &[0]);
    assert_eq!(table.entries_for("beta"), &[0]);
    assert!(detect_combos(&deck(&["Commander"], &["Alpha"]), &table).is_empty());
    assert_eq!(
        detect_combos(&deck(&["Commander"], &["Alpha", "Beta"]), &table).len(),
        1
    );
}

#[test]
fn detect_combos_is_deterministic() {
    let table = ComboTable::from_doc(doc(vec![
        entry(
            piece("alpha", "Alpha", ComboPieceZone::Anywhere),
            piece("beta", "Beta", ComboPieceZone::Anywhere),
        ),
        entry(
            piece("alpha", "Alpha", ComboPieceZone::Anywhere),
            piece("gamma", "Gamma", ComboPieceZone::Anywhere),
        ),
    ]));
    let ordered = deck(&["Commander"], &["Alpha", "Beta", "Gamma"]);
    let shuffled = deck(&["Commander"], &["Gamma", "Alpha", "Beta"]);

    let first = detect_combos(&ordered, &table);
    assert_eq!(first, detect_combos(&ordered, &table));
    assert_eq!(first, detect_combos(&shuffled, &table));
}

#[test]
fn cardinality_skips_this_decks_commander() {
    let mut combo = entry(
        piece("commander", "Commander", ComboPieceZone::CommandZone),
        piece("other", "Other", ComboPieceZone::Anywhere),
    );
    combo.setup = ComboSetup::NotablePrerequisites;

    assert_eq!(
        combo_cardinality(&combo, &BTreeSet::from(["commander".to_owned()])),
        ComboCardinality::DefinitelyTwoCard
    );
    assert_eq!(
        combo_cardinality(&combo, &BTreeSet::new()),
        ComboCardinality::ArguablyTwoCard
    );
}

#[test]
fn cardinality_seeds_arguable_from_setup_and_relevance() {
    let base = entry(
        piece("alpha", "Alpha", ComboPieceZone::Anywhere),
        piece("beta", "Beta", ComboPieceZone::Anywhere),
    );
    let cases = [
        (
            ComboSetup::AsPrinted,
            ComboRelevance::Standalone,
            ComboCardinality::DefinitelyTwoCard,
        ),
        (
            ComboSetup::NotablePrerequisites,
            ComboRelevance::Standalone,
            ComboCardinality::ArguablyTwoCard,
        ),
        (
            ComboSetup::AsPrinted,
            ComboRelevance::Contextual,
            ComboCardinality::ArguablyTwoCard,
        ),
        (
            ComboSetup::NotablePrerequisites,
            ComboRelevance::Contextual,
            ComboCardinality::More,
        ),
    ];

    for (setup, relevance, expected) in cases {
        let candidate = ComboEntry {
            setup,
            relevance,
            ..base.clone()
        };
        assert_eq!(combo_cardinality(&candidate, &BTreeSet::new()), expected);
    }
}

#[test]
fn library_piece_is_skipped() {
    let mut combo = entry(
        piece("library", "Library Piece", ComboPieceZone::Library),
        piece("other", "Other", ComboPieceZone::Anywhere),
    );
    combo.setup = ComboSetup::NotablePrerequisites;
    combo.relevance = ComboRelevance::Contextual;

    assert_eq!(
        combo_cardinality(&combo, &BTreeSet::new()),
        ComboCardinality::ArguablyTwoCard
    );
}

#[test]
fn to_resource_axis_is_lossy_and_one_way() {
    let cases = [
        (ComboResource::Mana, None),
        (ComboResource::Damage, None),
        (ComboResource::LifeLoss, None),
        (ComboResource::Lifegain, None),
        (ComboResource::Mill, None),
        (ComboResource::Draw, Some(ResourceAxis::CardsDrawn)),
        (ComboResource::Tokens, Some(ResourceAxis::TokensCreated)),
        (ComboResource::Combat, Some(ResourceAxis::CombatPhases)),
        (ComboResource::Turns, Some(ResourceAxis::ExtraTurns)),
    ];

    for (resource, expected) in cases {
        assert_eq!(
            ComboOutcome::Unbounded(resource).to_resource_axis(),
            expected
        );
    }
    assert_eq!(ComboOutcome::Wins.to_resource_axis(), None);
}

#[test]
fn self_pair_and_duplicate_rows_are_refused_at_load() {
    let self_pair = doc(vec![entry(
        piece("same", "Same", ComboPieceZone::Anywhere),
        piece("same", "Same", ComboPieceZone::Anywhere),
    )]);
    let self_pair_raw = serde_json::to_string(&self_pair).expect("document serializes");
    assert!(matches!(
        ComboTable::from_json_str(&self_pair_raw),
        Err(ComboTableError::SelfPair { key }) if key == "same"
    ));

    let duplicate = doc(vec![
        entry(
            piece("alpha", "Alpha", ComboPieceZone::Anywhere),
            piece("beta", "Beta", ComboPieceZone::Anywhere),
        ),
        entry(
            piece("beta", "Beta", ComboPieceZone::Anywhere),
            piece("alpha", "Alpha", ComboPieceZone::Anywhere),
        ),
    ]);
    let duplicate_raw = serde_json::to_string(&duplicate).expect("document serializes");
    assert!(matches!(
        ComboTable::from_json_str(&duplicate_raw),
        Err(ComboTableError::DuplicateEntry { first, second })
            if first == "alpha" && second == "beta"
    ));
}

fn strings_in(value: &Value) -> Vec<&str> {
    match value {
        Value::String(value) => vec![value],
        Value::Array(values) => values.iter().flat_map(strings_in).collect(),
        Value::Object(values) => values.values().flat_map(strings_in).collect(),
        Value::Null | Value::Bool(_) | Value::Number(_) => Vec::new(),
    }
}

#[test]
fn entries_carry_no_free_text() {
    let artifact = doc(vec![entry(
        piece("alpha", "Alpha", ComboPieceZone::Anywhere),
        piece("beta", "Beta", ComboPieceZone::CommandZone),
    )]);
    let value = serde_json::to_value(&artifact).expect("document serializes");
    let allowed = BTreeSet::from([
        "2026-09-07",
        "1",
        "Commander Spellbook",
        "2026-09-01",
        "prerequisite_text",
        "result_text",
        "unmodeled_result_classes",
        "alpha",
        "Alpha",
        "beta",
        "Beta",
        "anywhere",
        "command_zone",
        "standalone",
        "as_printed",
        "unbounded",
        "mana",
        "draw",
        "mass_land_denial",
    ]);

    assert!(
        strings_in(&value)
            .into_iter()
            .all(|value| allowed.contains(value)),
        "artifact contained a string outside provenance, card names, or typed vocabulary"
    );
}

#[test]
fn matches_report_combo_pair_source() {
    let table = ComboTable::from_doc(doc(vec![entry(
        piece("alpha", "Alpha", ComboPieceZone::Anywhere),
        piece("beta", "Beta", ComboPieceZone::Anywhere),
    )]));

    assert_eq!(
        detect_combos(&deck(&["Commander"], &["Alpha", "Beta"]), &table),
        vec![ComboMatch {
            pieces: [
                piece("alpha", "Alpha", ComboPieceZone::Anywhere),
                piece("beta", "Beta", ComboPieceZone::Anywhere),
            ],
            relevance: ComboRelevance::Standalone,
            cardinality: ComboCardinality::DefinitelyTwoCard,
            assemble_cost: 7,
            popularity: 42,
            outcomes: BTreeSet::from([
                ComboOutcome::Unbounded(ComboResource::Mana),
                ComboOutcome::Unbounded(ComboResource::Draw),
            ]),
            axes: BTreeSet::from([BracketAxis::MassLandDenial]),
            source: SignalSource::ComboPair,
        }]
    );
}
