//! Shared card-data fixtures for the engine integration tests.
//!
//! These tests run under `cargo nextest`, which executes every test in its own
//! process. A process-wide `OnceLock` therefore can't be shared *across* tests —
//! each test re-pays whatever it loads. Deserializing the full ~90 MB
//! `client/public/card-data.json` costs tens of seconds per test in a debug
//! build, but the suite only references a few hundred distinct cards.
//!
//! So [`shared_card_db`] loads a small committed fixture
//! (`tests/fixtures/integration_cards.json.gz`, a strict subset of the export) that
//! parses in milliseconds. Regenerate it with `python3 scripts/gen-test-fixture.py`
//! after adding a test that references a new card; set `FORGE_TEST_FULL_DB=1` to
//! force the full export. [`shared_card_export_json`] still loads the full export
//! for the few drift-guard tests that must inspect every card.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use engine::database::card_db::CardDatabase;
use engine::game::triggers::trigger_source_context_for_latch;
use engine::types::card::CardFace;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, NamedChoiceSource, NamedChoiceSourceBinding};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use flate2::read::GzDecoder;
use serde_json::{Map, Value};

/// Builds the exact object-and-resolution authority used by persisted named
/// choice fixtures. Test prompts must not retain a raw object id as authority.
pub fn exact_named_choice_source(state: &GameState, object_id: ObjectId) -> NamedChoiceSource {
    let context = trigger_source_context_for_latch(state, state.objects.get(&object_id).unwrap());
    NamedChoiceSource::from_trigger_source(
        context,
        NamedChoiceSourceBinding::ExactObjectAndResolution,
    )
}

/// Build a card database containing exactly `faces`, and install it on `state`
/// as the resolution-time draw source.
///
/// CR 707.2 + CR 202.3: `Effect::CreateTokenCopyFromPool` (the Momir Basic
/// emblem) draws its random creature from `GameState::card_db` at resolution,
/// so a test that exercises it must install a database rather than pre-seeding
/// a pool. Keeping the database tiny keeps the draw's candidate set exactly the
/// faces the test declared.
pub fn install_synthetic_card_db(state: &mut GameState, faces: &[CardFace]) -> Arc<CardDatabase> {
    let export: Map<String, Value> = faces
        .iter()
        .map(|face| {
            (
                face.name.to_lowercase(),
                serde_json::to_value(face).expect("CardFace serializes"),
            )
        })
        .collect();
    let db = Arc::new(
        CardDatabase::from_json_str(&Value::Object(export).to_string())
            .expect("synthetic export should parse"),
    );
    engine::game::install_card_db(state, Arc::clone(&db));
    db
}

/// Path to the full parsed card-data export, relative to the engine crate root.
fn export_path() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let client_export = root.join("client/public/card-data.json");
    if client_export.exists() {
        client_export
    } else {
        root.join("data/card-data.json")
    }
}

/// Path to the curated integration-test fixture (a subset of the export).
// TWIN-SYNC: keep this fixture path in lockstep with src/test_support.rs.
fn fixture_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/integration_cards.json.gz")
}

fn load_fixture(path: &Path) -> CardDatabase {
    let file = File::open(path).expect("fixture should open");
    let reader = BufReader::new(file);
    let decoder = GzDecoder::new(reader);
    CardDatabase::from_export_reader(decoder).expect("fixture should load")
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
            return Some(load_fixture(&fixture));
        }
        let path = export_path();
        if !path.exists() {
            eprintln!("skipping: full card-data export not generated");
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

/// Every serialized key under which a `GameEvent` (or log segment) can carry an
/// object id: each `GameEvent` field whose type holds `ObjectId`, `TargetRef`,
/// `ObjectIncarnationRef`, `EventObjectSnapshot`, `ObjectIdentityBinding`,
/// `ZoneChangeRecord`, `TriggerSourceContext` or `CardId`, plus `Object` (the
/// externally tagged `TargetRef::Object` key) and the
/// `LogSegment::CardName` key `object_id` (already in the list).
///
/// Regenerate the type-derived part with:
///
/// ```text
/// awk '/^pub enum GameEvent/,/^}/' crates/engine/src/types/events.rs | grep -E "^\s+[a-z_]+: .*(ObjectId|TargetRef|ObjectIncarnationRef|EventObjectSnapshot|ObjectIdentityBinding|ZoneChangeRecord|TriggerSourceContext|CardId)" | sed -E 's/^\s+([a-z_]+):.*/\1/' | sort -u
/// ```
///
/// `card_id` is counted as the same handle: a real card's `CardId` comes from the
/// same counter as its `ObjectId` (`create_object_from_card_face`), and
/// `filter_events_for_viewer` already treats it as identifying.
const IDENTIFYING_ID_KEYS: &[&str] = &[
    "Object",
    "assignments",
    "attachment_id",
    "attacker",
    "attacker_ids",
    "attacks",
    "attraction_id",
    "augmenting_id",
    "blocker",
    "card_id",
    "card_ids",
    "caused_by",
    "chosen_bearer",
    "contraption_id",
    "countered_by",
    "creature_id",
    "creatures",
    "dropped",
    "exploiter",
    "from",
    "involved",
    "merged_id",
    "merging_id",
    "mount_id",
    "object_id",
    "old_target",
    "original_id",
    "partner_id",
    "plane_id",
    "record",
    "returns",
    "sacrificed",
    "saga",
    "scheme_id",
    "source",
    "source_amounts",
    "source_id",
    "source_incarnations",
    "spacecraft_id",
    "stack_entry",
    "subject",
    "tapped",
    "target",
    "to",
    "vehicle_id",
];

fn collect_numbers(value: &Value, out: &mut BTreeSet<u64>) {
    match value {
        Value::Number(n) => out.extend(n.as_u64()),
        Value::Array(items) => items.iter().for_each(|item| collect_numbers(item, out)),
        Value::Object(map) => map.values().for_each(|item| collect_numbers(item, out)),
        Value::Null | Value::Bool(_) | Value::String(_) => {}
    }
}

/// Every number found anywhere under an `IDENTIFYING_ID_KEYS` key of `value`, at any
/// depth (so the tuple-vector keys `assignments`, `attacks` and `source_amounts`,
/// which hold arrays of arrays, are covered). `ObjectId` is `#[serde(transparent)]`
/// over `u64`. A small non-id number inside a subtree can collide with a probed id;
/// that can only false-fail a negative check, never hide a leak (probed ids here are
/// above the small values such subtrees carry).
pub fn identifying_object_ids(value: &Value) -> BTreeSet<u64> {
    let mut ids = BTreeSet::new();
    walk_identifying(value, &mut ids);
    ids
}

fn walk_identifying(value: &Value, ids: &mut BTreeSet<u64>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if IDENTIFYING_ID_KEYS.contains(&key.as_str()) {
                    collect_numbers(child, ids);
                } else {
                    walk_identifying(child, ids);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| walk_identifying(item, ids)),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// The identifying object ids that survive `filter_events_for_viewer` for `viewer`.
pub fn viewer_event_ids(
    events: &[GameEvent],
    state: &GameState,
    viewer: PlayerId,
) -> BTreeSet<u64> {
    let filtered = engine::game::visibility::filter_events_for_viewer(events, state, viewer);
    identifying_object_ids(&serde_json::to_value(filtered).expect("serialize filtered events"))
}
