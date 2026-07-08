//! Shared card-data fixtures for the engine integration tests.
//!
//! These tests run under `cargo nextest`, which executes every test in its own
//! process. A process-wide `OnceLock` therefore can't be shared *across* tests —
//! each test re-pays whatever it loads. Deserializing the full ~90 MB
//! `client/public/card-data.json` costs tens of seconds per test in a debug
//! build, but the suite only references a few hundred distinct cards.
//!
//! So [`shared_card_db`] loads a small committed fixture
//! (`tests/fixtures/integration_cards.json`, a strict subset of the export) that
//! parses in milliseconds. Regenerate it with `python3 scripts/gen-test-fixture.py`
//! after adding a test that references a new card; set `FORGE_TEST_FULL_DB=1` to
//! force the full export. [`shared_card_export_json`] still loads the full export
//! for the few drift-guard tests that must inspect every card.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use engine::database::card_db::CardDatabase;
use serde_json::{Map, Value};

/// Path to the full parsed card-data export, relative to the engine crate root.
fn export_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json")
}

/// Path to the curated integration-test fixture (a subset of the export).
// TWIN-SYNC: keep this fixture path in lockstep with src/test_support.rs.
fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration_cards.json")
}

/// Returns the shared, process-wide card database, loading it on first use.
///
/// Loads the curated fixture by default (fast). Falls back to the full export
/// when `FORGE_TEST_FULL_DB=1` is set or the fixture is absent. Returns `None`
/// only when neither file exists (e.g. CI without the card-data pipeline and no
/// committed fixture); the decision is cached for the whole binary.
pub fn shared_card_db() -> Option<&'static CardDatabase> {
    static DB: OnceLock<Option<CardDatabase>> = OnceLock::new();
    DB.get_or_init(|| {
        let fixture = fixture_path();
        if std::env::var_os("FORGE_TEST_FULL_DB").is_none() && fixture.exists() {
            return Some(CardDatabase::from_export(&fixture).expect("fixture should load"));
        }
        let path = export_path();
        if !path.exists() {
            eprintln!("skipping: client/public/card-data.json not generated");
            return None;
        }
        Some(CardDatabase::from_export(&path).expect("export should load"))
    })
    .as_ref()
}

/// Returns the raw card-data export as a JSON object, loading it on first use.
///
/// Drift-guard tests inspect the *serialized* ability structure (raw tags)
/// rather than the parsed [`CardDatabase`], so they need the untyped JSON. These
/// are the few tests that must scan every card, so they parse the full ~90 MB
/// export (~13 s in debug); the decision is cached for the whole binary. Returns
/// `None` when the export is absent.
pub fn shared_card_export_json() -> Option<&'static Map<String, Value>> {
    static JSON: OnceLock<Option<Map<String, Value>>> = OnceLock::new();
    JSON.get_or_init(|| {
        let path = export_path();
        if !path.exists() {
            eprintln!("skipping: client/public/card-data.json not generated");
            return None;
        }
        let raw = std::fs::read_to_string(&path).expect("export should be readable");
        match serde_json::from_str(&raw).expect("export should be valid JSON") {
            Value::Object(map) => Some(map),
            _ => panic!("export root should be a JSON object"),
        }
    })
    .as_ref()
}
