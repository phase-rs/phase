use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use combo_table_gen::{
    assert_facts_only, build_combo_table, BuildLimits, GenError, PinnedComboSnapshot,
};
use engine::database::{
    CardDatabase, ComboOutcome, ComboPieceZone, ComboRelevance, ComboResource, ComboSetup,
    ComboTable,
};
use engine::game::bracket_estimate::BracketAxis;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

const FIXTURE: &str = "tests/fixtures/spellbook-fixture.json.gz";
const POOL: &str = "../engine/tests/fixtures/integration_cards.json.gz";

fn pool() -> CardDatabase {
    let file = fs::File::open(POOL).expect("integration card pool exists");
    CardDatabase::from_export_reader(GzDecoder::new(file)).expect("integration card pool loads")
}

fn decompressed_fixture() -> Vec<u8> {
    let mut bytes = Vec::new();
    GzDecoder::new(fs::File::open(FIXTURE).expect("fixture exists"))
        .read_to_end(&mut bytes)
        .expect("fixture decompresses");
    bytes
}

fn fixture_pin() -> PinnedComboSnapshot {
    let bytes = decompressed_fixture();
    let digest = Box::leak(format!("{:x}", Sha256::digest(&bytes)).into_boxed_str());
    PinnedComboSnapshot {
        file: "spellbook-fixture.json.gz",
        sha256: digest,
        document_timestamp: "2026-01-02T03:04:05+00:00",
        builder: "fixture-1",
        variant_count: 20,
    }
}

fn build() -> engine::database::ComboTableDoc {
    build_combo_table(
        fs::File::open(FIXTURE).expect("fixture exists"),
        &pool(),
        &fixture_pin(),
        &BuildLimits::default(),
    )
    .expect("fixture builds")
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(bytes).expect("gzip accepts fixture");
    encoder.finish().expect("gzip finishes")
}

fn unique_output(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "combo-table-gen-{}-{name}.json",
        std::process::id()
    ))
}

fn write_only_on_success(
    source: impl Read,
    pin: &PinnedComboSnapshot,
    output: &Path,
) -> Result<(), GenError> {
    let doc = build_combo_table(source, &pool(), pin, &BuildLimits::default())?;
    fs::write(output, serde_json::to_vec(&doc)?).map_err(GenError::from)
}

#[test]
fn pin_digest_mismatch_refuses_to_write() {
    let mut bytes = decompressed_fixture();
    let offset = bytes
        .iter()
        .position(|byte| *byte == b'9')
        .expect("fixture has a digit");
    bytes[offset] = b'8';
    let output = unique_output("digest");
    let _ = fs::remove_file(&output);
    let error = write_only_on_success(gzip(&bytes).as_slice(), &fixture_pin(), &output)
        .expect_err("mutated fixture must fail");
    assert!(matches!(error, GenError::DigestMismatch { .. }));
    assert!(!output.exists());
}

#[test]
fn pin_builder_and_timestamp_mismatch_refuse() {
    let mut pin = fixture_pin();
    pin.builder = "fixture-other";
    assert!(matches!(
        build_combo_table(
            fs::File::open(FIXTURE).unwrap(),
            &pool(),
            &pin,
            &BuildLimits::default()
        ),
        Err(GenError::BuilderMismatch { .. })
    ));

    let mut pin = fixture_pin();
    pin.document_timestamp = "2026-01-03T03:04:05+00:00";
    assert!(matches!(
        build_combo_table(
            fs::File::open(FIXTURE).unwrap(),
            &pool(),
            &pin,
            &BuildLimits::default()
        ),
        Err(GenError::TimestampMismatch { .. })
    ));
}

#[test]
fn variant_count_mismatch_refuses() {
    let mut pin = fixture_pin();
    pin.variant_count = 19;
    assert!(matches!(
        build_combo_table(
            fs::File::open(FIXTURE).unwrap(),
            &pool(),
            &pin,
            &BuildLimits::default()
        ),
        Err(GenError::VariantCountMismatch { actual: 20, .. })
    ));
}

#[test]
fn empty_pool_refuses() {
    assert!(matches!(
        build_combo_table(
            fs::File::open(FIXTURE).unwrap(),
            &CardDatabase::default(),
            &fixture_pin(),
            &BuildLimits::default()
        ),
        Err(GenError::EmptyPool)
    ));
}

#[test]
fn structural_filter_drop_reasons() {
    let counts = build().provenance.filtered;
    assert_eq!(counts.not_commander_legal, 1);
    assert_eq!(counts.not_ok, 1);
    assert_eq!(counts.template, 1);
    assert_eq!(counts.one_card, 1);
    assert_eq!(counts.three_or_more, 1);
    assert_eq!(counts.unknown_card, 1);
    assert_eq!(counts.irrelevant, 1);
    assert_eq!(counts.duplicate_pair, 1);
    assert_eq!(counts.kept, 12);
    assert_eq!(counts.multi_zone_pieces, 3);
}

#[test]
fn relevance_uses_only_feature_status() {
    let doc = build();
    assert!(!doc.entries.iter().any(|entry| {
        entry.pieces.iter().any(|piece| piece.key == "mox amber")
            || entry
                .pieces
                .iter()
                .any(|piece| piece.key == "thassa's oracle")
    }));
    assert_eq!(doc.provenance.filtered.irrelevant, 1);
}

#[test]
fn helper_only_mass_land_denial_row_is_rescued() {
    let doc = build();
    let entry = doc
        .entries
        .iter()
        .find(|entry| {
            entry
                .pieces
                .iter()
                .any(|piece| piece.key == "lightning bolt")
        })
        .expect("rescued row ships");
    assert_eq!(entry.relevance, ComboRelevance::Helper);
    assert_eq!(
        entry.axes,
        [BracketAxis::MassLandDenial].into_iter().collect()
    );
    assert_eq!(entry.pieces[0].zone, ComboPieceZone::CommandZone);
    assert_eq!(entry.pieces[1].zone, ComboPieceZone::Library);
}

#[test]
fn artifact_carries_no_free_text() {
    let pool = pool();
    let doc = build_combo_table(
        fs::File::open(FIXTURE).unwrap(),
        &pool,
        &fixture_pin(),
        &BuildLimits::default(),
    )
    .unwrap();
    assert_facts_only(&doc, &pool).expect("all serialized strings are typed facts");
    let serialized = serde_json::to_string(&doc).unwrap();
    assert!(!serialized.contains("condition written only for this fixture"));
    assert!(!serialized.contains("deliberately neutral fixture result"));
}

#[test]
fn assemble_cost_is_mana_value_needed_plus_piece_values() {
    let doc = build();
    let entry = doc
        .entries
        .iter()
        .find(|entry| entry.pieces.iter().any(|piece| piece.key == "giant growth"))
        .expect("cost row ships");
    assert_eq!(entry.mana_value_needed, 4);
    assert_eq!(entry.assemble_cost, 4 + 1 + 4);
    assert_eq!(entry.setup, ComboSetup::NotablePrerequisites);
}

#[test]
fn duplicate_pairs_keep_the_strongest_row() {
    let doc = build();
    let entry = doc
        .entries
        .iter()
        .find(|entry| entry.pieces[0].key == "island" && entry.pieces[1].key == "mountain")
        .expect("pair ships once");
    assert_eq!(entry.relevance, ComboRelevance::Standalone);
    assert_eq!(entry.popularity, 10);
    assert_eq!(entry.outcomes, [ComboOutcome::Wins].into_iter().collect());
    assert_eq!(
        doc.entries
            .iter()
            .filter(|candidate| candidate.pieces == entry.pieces)
            .count(),
        1
    );
}

#[test]
fn pieces_are_key_ordered_and_entries_sorted() {
    let doc = build();
    assert!(doc
        .entries
        .iter()
        .all(|entry| entry.pieces[0].key < entry.pieces[1].key));
    assert!(doc.entries.windows(2).all(|rows| {
        (&rows[0].pieces[0].key, &rows[0].pieces[1].key)
            < (&rows[1].pieces[0].key, &rows[1].pieces[1].key)
    }));
}

#[test]
fn built_doc_loads_through_combo_table_from_json_str() {
    let raw = serde_json::to_string(&build()).unwrap();
    ComboTable::from_json_str(&raw).expect("engine table accepts generated artifact");
}

#[test]
fn snapshot_fixture_is_small_and_committed() {
    let compressed = fs::read(FIXTURE).expect("fixture is committed");
    assert!(compressed.len() < 2_048);
    let text = String::from_utf8(decompressed_fixture()).unwrap();
    for forbidden in [
        "description",
        "notes",
        "prices",
        "imageUri",
        "bracketTag",
        "http://",
        "https://",
    ] {
        assert!(
            !text.contains(forbidden),
            "fixture contains forbidden field {forbidden}"
        );
    }
    let document: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(document["variants"].as_array().unwrap().len(), 20);
}

#[test]
fn fixture_covers_every_outcome_class() {
    let outcomes: std::collections::BTreeSet<_> = build()
        .entries
        .into_iter()
        .flat_map(|entry| entry.outcomes)
        .collect();
    let expected = [
        ComboOutcome::Wins,
        ComboOutcome::Unbounded(ComboResource::Mana),
        ComboOutcome::Unbounded(ComboResource::Damage),
        ComboOutcome::Unbounded(ComboResource::LifeLoss),
        ComboOutcome::Unbounded(ComboResource::Lifegain),
        ComboOutcome::Unbounded(ComboResource::Mill),
        ComboOutcome::Unbounded(ComboResource::Draw),
        ComboOutcome::Unbounded(ComboResource::Tokens),
        ComboOutcome::Unbounded(ComboResource::Combat),
        ComboOutcome::Unbounded(ComboResource::Turns),
    ]
    .into_iter()
    .collect();
    assert_eq!(outcomes, expected);
}
