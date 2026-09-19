//! #7968 — a `GameObject` whose field holds its own default no longer spends that field's
//! bytes on the wire. The contract the legs below pin is serde's, not a rules one: an
//! absent key means "the value `#[serde(default)]` will rebuild", so every skip predicate
//! must agree exactly with the default its own field declares.
//!
//! `wire_round_trip_is_a_fixpoint` is corpus-bounded, not exhaustive: a skipped field that
//! no fixture object holds at a non-default value is never exercised by the walk. Nor does
//! construction close the class: which predicate guards which field is a hand-maintained
//! pairing nothing checks, and `mismatched_default_and_predicate_breaks_the_fixpoint` is
//! the leg that names what a diverged pair costs.

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};

use engine::game::game_object::GameObject;
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::TriggerBaseSetInstanceRef;
use engine::types::game_state::{GameState, PersistedGameState};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde::{Deserialize, Serialize};

const HIDDEN_CARD_NAME: &str = "Hidden Card";

/// Every key a hidden library stub emits on at least one object of the seat-0 projection:
/// the fields that serialize unconditionally, plus the four a stub still holds at a
/// non-default value — `base_controller` (the owner), `base_name` ("Hidden Card"),
/// `base_characteristics_initialized` and `incarnation`.
const STUB_WIRE_KEY_UNION: &[&str] = &[
    "abilities",
    "attached_to",
    "attachments",
    "base_characteristics_initialized",
    "base_color",
    "base_controller",
    "base_keywords",
    "base_name",
    "base_power",
    "base_toughness",
    "card_id",
    "card_types",
    "color",
    "controller",
    "counters",
    "damage_marked",
    "dealt_deathtouch_damage",
    "entered_battlefield_turn",
    "face_down",
    "flipped",
    "id",
    "incarnation",
    "keywords",
    "loyalty",
    "mana_cost",
    "name",
    "owner",
    "power",
    "replacement_definitions",
    "static_definitions",
    "tapped",
    "timestamp",
    "toughness",
    "transformed",
    "trigger_definitions",
    "zone",
];
/// Every key a hidden library stub emits on *all* of them. It is the union minus
/// `incarnation`, which only a minority of stubs holds at a non-default value — which is
/// why neither equality alone is exact.
const STUB_WIRE_KEY_CORE: &[&str] = &[
    "abilities",
    "attached_to",
    "attachments",
    "base_characteristics_initialized",
    "base_color",
    "base_controller",
    "base_keywords",
    "base_name",
    "base_power",
    "base_toughness",
    "card_id",
    "card_types",
    "color",
    "controller",
    "counters",
    "damage_marked",
    "dealt_deathtouch_damage",
    "entered_battlefield_turn",
    "face_down",
    "flipped",
    "id",
    "keywords",
    "loyalty",
    "mana_cost",
    "name",
    "owner",
    "power",
    "replacement_definitions",
    "static_definitions",
    "tapped",
    "timestamp",
    "toughness",
    "transformed",
    "trigger_definitions",
    "zone",
];

/// Keys every battlefield object of that projection holds at a non-default value.
const BATTLEFIELD_UNCONDITIONAL_KEYS: &[&str] = &[
    "printed_ref",
    "base_printed_ref",
    "base_name",
    "base_mana_cost",
    "base_card_types",
    "base_characteristics_initialized",
    "incarnation",
];

/// `GameObject`'s `#[serde(skip_deserializing)]` fields. Serde discards whatever the wire
/// carried for them and the engine re-derives them after a load, so they are not
/// round-trippable by construction — the fixpoint below is taken over every other key.
/// An unlisted member of the class makes the comparison fail loudly, not silently pass.
const DERIVED_NOT_DESERIALIZED: &[&str] = &[
    "available_mana_pips",
    "blocked_abilities",
    "commander_tax",
    "devotion",
    "has_mana_ability",
    "has_summoning_sickness",
    "loyalty_activations_this_turn",
    "mana_ability_index",
    "unimplemented_mechanics",
];

fn gunzip(bytes: &[u8]) -> String {
    let mut json = String::new();
    flate2::read::GzDecoder::new(bytes)
        .read_to_string(&mut json)
        .expect("fixture .json.gz inflates to UTF-8 JSON");
    json
}

fn seat_zero_projection() -> GameState {
    let json = gunzip(include_bytes!("../fixtures/dina_conqueror_4p.json.gz"));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("dump envelope parses as JSON");
    let state = serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("gameState deserializes through the production decoder")
        .into_game_state()
        .expect("persisted snapshot satisfies the checked restore contract");
    filter_state_for_viewer(&state, PlayerId(0))
}

fn wire_keys(object: &GameObject) -> BTreeSet<String> {
    let serde_json::Value::Object(map) =
        serde_json::to_value(object).expect("a projected object serializes")
    else {
        panic!("a GameObject serializes as a JSON object");
    };
    map.keys().cloned().collect()
}

#[test]
fn hidden_stub_wire_key_set_is_exact() {
    let projection = seat_zero_projection();
    let mut union = BTreeSet::new();
    let mut core: Option<BTreeSet<String>> = None;
    let mut stubs = 0;
    let mut battlefield = 0;
    for object in projection.objects.values() {
        if object.zone == Zone::Battlefield {
            battlefield += 1;
        }
        if object.zone != Zone::Library || object.name != HIDDEN_CARD_NAME {
            continue;
        }
        let keys = wire_keys(object);
        union.extend(keys.iter().cloned());
        core = Some(match core {
            Some(core) => core.intersection(&keys).cloned().collect(),
            None => keys,
        });
        stubs += 1;
    }
    // Paired reach guard: an empty or mis-filtered `objects` map must fail rather than
    // satisfy two set equalities over nothing.
    assert!(stubs > 0, "the projection yields hidden library stubs");
    assert!(battlefield > 0, "the projection yields battlefield objects");

    let expected_union: BTreeSet<String> =
        STUB_WIRE_KEY_UNION.iter().map(|k| k.to_string()).collect();
    let expected_core: BTreeSet<String> =
        STUB_WIRE_KEY_CORE.iter().map(|k| k.to_string()).collect();
    assert_eq!(union, expected_union, "stub key union");
    assert_eq!(
        core.expect("a non-empty stub population has a core"),
        expected_core,
        "stub key intersection"
    );
}

#[test]
fn non_default_values_still_serialize() {
    let projection = seat_zero_projection();
    let mut objects = 0;
    for object in projection.objects.values() {
        if object.zone != Zone::Battlefield {
            continue;
        }
        let keys = wire_keys(object);
        for key in BATTLEFIELD_UNCONDITIONAL_KEYS {
            assert!(
                keys.contains(*key),
                "battlefield object {:?} dropped {key}",
                object.id
            );
        }
        // The two keys this board holds non-default on some objects and default on
        // others: presence must track the value object by object, not merely exist
        // somewhere in the population.
        assert_eq!(
            keys.contains("base_abilities"),
            !object.base_abilities.is_empty(),
            "base_abilities presence on {:?}",
            object.id
        );
        assert_eq!(
            keys.contains("summoning_sick"),
            object.summoning_sick,
            "summoning_sick presence on {:?}",
            object.id
        );
        objects += 1;
    }
    assert!(objects > 0, "the projection yields battlefield objects");
}

fn collect_gz(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
    for entry in entries {
        let path = entry.expect("read dir entry").path();
        if path.is_dir() {
            collect_gz(&path, out);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".json.gz"))
        {
            out.push(path);
        }
    }
}

/// `None` when `to_string(from_str(to_string(obj))) == to_string(obj)`, otherwise the keys
/// whose value the round trip changed — a predicate that does not name its field's own
/// default shows up here as the key it rewrote.
fn fixpoint_delta(object: &GameObject) -> Option<String> {
    let once = serde_json::to_string(object).expect("a loaded object serializes");
    let restored: GameObject = serde_json::from_str(&once)
        .unwrap_or_else(|error| panic!("a serialized object deserializes: {error}\n{once}"));
    let twice = serde_json::to_string(&restored).expect("a restored object serializes");
    if twice == once {
        return None;
    }
    let before: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&once).expect("the first pass is a JSON object");
    let after: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&twice).expect("the second pass is a JSON object");
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let delta: Vec<String> = keys
        .into_iter()
        .filter(|key| !DERIVED_NOT_DESERIALIZED.contains(&key.as_str()))
        .filter(|key| before.get(*key) != after.get(*key))
        .map(|key| {
            let render = |value: Option<&serde_json::Value>| {
                let text = value.map_or_else(|| "<absent>".to_string(), |v| v.to_string());
                text.chars().take(120).collect::<String>()
            };
            format!(
                "{key}: {} -> {}",
                render(before.get(key)),
                render(after.get(key))
            )
        })
        .collect();
    (!delta.is_empty()).then(|| delta.join("; "))
}

#[test]
fn wire_round_trip_is_a_fixpoint() {
    let mut files = Vec::new();
    collect_gz(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
        &mut files,
    );
    files.sort();
    let (mut wrapped, mut bare, mut objects) = (0, 0, 0);
    for path in &files {
        let json = gunzip(&std::fs::read(path).expect("fixture reads"));
        let envelope: serde_json::Value =
            serde_json::from_str(&json).expect("fixture parses as JSON");
        let persisted = if let Some(state) = envelope.get("gameState") {
            wrapped += 1;
            state.clone()
        } else if envelope.get("objects").is_some() && envelope.get("turn_number").is_some() {
            bare += 1;
            envelope.clone()
        } else {
            // A dump whose shape drifted must panic with its path rather than be
            // silently skipped into an absence-shaped pass.
            assert!(
                envelope.get("gameState").is_none()
                    && envelope.get("objects").is_none()
                    && envelope.get("turn_number").is_none(),
                "{} carries dump keys but matches no dump shape",
                path.display()
            );
            continue;
        };
        // Through the production restore chokepoint, so the objects under test are the ones
        // a reconnect or a session load actually holds: a raw per-object decode mints the
        // legacy `Unmaterialized` trigger marker that no observable state carries.
        let state = serde_json::from_value::<PersistedGameState>(persisted)
            .unwrap_or_else(|error| {
                panic!(
                    "{}: decodes through the production decoder: {error}",
                    path.display()
                )
            })
            .into_game_state()
            .unwrap_or_else(|error| {
                panic!(
                    "{}: satisfies the checked restore contract: {error}",
                    path.display()
                )
            });
        for object in state.objects.values() {
            if let Some(delta) = fixpoint_delta(object) {
                panic!(
                    "{}: object {:?} is not a wire fixpoint: {delta}",
                    path.display(),
                    object.id
                );
            }
            objects += 1;
        }
    }
    // Both shape classes must be walked: dropping the bare-`GameState` arm goes red here,
    // where a numeric floor would still pass on the wrapped majority.
    assert!(wrapped > 0 && bare > 0, "both dump shapes are walked");
    assert!(objects > 0, "the walk reached objects");
}

fn two() -> u64 {
    2
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// A predicate that does not name its field's own default. Establishes only that the
/// fixpoint comparison can fail — the type under test is closed by the leg below.
#[derive(Serialize, Deserialize)]
struct MismatchedPair {
    #[serde(default = "two", skip_serializing_if = "is_zero_u64")]
    n: u64,
}

#[test]
fn mismatched_default_and_predicate_breaks_the_fixpoint() {
    let once = serde_json::to_string(&MismatchedPair { n: 0 }).unwrap();
    let restored: MismatchedPair = serde_json::from_str(&once).unwrap();
    assert_ne!(
        serde_json::to_string(&restored).unwrap(),
        once,
        "a predicate divergent from its default must break the fixpoint"
    );
}

#[test]
fn custom_default_fields_round_trip_at_zero() {
    // No committed dump object holds `0` in either field, so the corpus cannot supply
    // this member: a zero-valued predicate on either is green on every other leg and red
    // only here.
    let mut object = GameObject::new(
        ObjectId(1),
        CardId(1),
        PlayerId(0),
        "probe".to_string(),
        Zone::Battlefield,
    );
    object.trigger_base_set_instance = TriggerBaseSetInstanceRef(0);
    object.next_trigger_base_set_instance = 0;
    let json = serde_json::to_string(&object).expect("a constructed object serializes");
    assert!(json.contains("trigger_base_set_instance"));
    let restored: GameObject =
        serde_json::from_str(&json).expect("a zero-valued object deserializes");
    assert_eq!(
        restored.trigger_base_set_instance,
        TriggerBaseSetInstanceRef(0)
    );
    assert_eq!(restored.next_trigger_base_set_instance, 0);
    assert_eq!(serde_json::to_string(&restored).unwrap(), json);

    // Paired reach guard: the untouched object holds the two initial values, so both keys
    // must be ABSENT. Without it this leg passes unchanged when no predicate was wired on.
    let untouched = GameObject::new(
        ObjectId(2),
        CardId(1),
        PlayerId(0),
        "probe".to_string(),
        Zone::Battlefield,
    );
    let keys = wire_keys(&untouched);
    assert!(!keys.contains("trigger_base_set_instance"));
    assert!(!keys.contains("next_trigger_base_set_instance"));
}
