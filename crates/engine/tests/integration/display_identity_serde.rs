//! Wire compatibility of the `DisplayIdentity` bundle carried by
//! `ContinuousModification::CopyValues` and `LatchedCopiableSnapshot`.
//!
//! `CopyValues` is serialized inside `GameState`, so saved games written before
//! the four display fields were bundled (flat keys at the variant level) must
//! still load, and a payload that omits the keys entirely must default.

use engine::game::game_object::{DisplayIdentity, DisplaySource};
use engine::game::printed_cards::intrinsic_copiable_values;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{ContinuousModification, CopiableValues, LatchedCopiableSnapshot};
use engine::types::card::{PrintedCardRef, TokenArtDescriptor, TokenImageRef};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::player::PlayerId;
use engine::types::proposed_event::CopyTokenSpec;
use serde_json::{json, Value};

fn sample_values() -> CopiableValues {
    let mut scenario = GameScenario::new();
    let id = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let runner = scenario.build();
    intrinsic_copiable_values(&runner.state().objects[&id])
}

fn sample_display() -> DisplayIdentity {
    DisplayIdentity {
        display_source: DisplaySource::Token,
        printed_ref: Some(PrintedCardRef {
            oracle_id: "oracle-1".to_string(),
            face_name: "Bear".to_string(),
        }),
        token_image_ref: Some(TokenImageRef {
            scryfall_id: "tok-1".to_string(),
            scryfall_oracle_id: None,
            face_name: None,
            preset_id: "bear_2_2".to_string(),
        }),
        token_art: Some(TokenArtDescriptor {
            power: Some(2),
            toughness: Some(2),
            colors: vec![ManaColor::Green],
            subtypes: vec!["Bear".to_string()],
            keywords: vec![],
            has_abilities: false,
        }),
    }
}

/// The JSON of a `CopyValues` as the pre-bundle engine wrote it: the display
/// keys flat at the variant level.
fn legacy_copy_values_json(display_keys: Value) -> Value {
    let modification = ContinuousModification::CopyValues {
        values: Box::new(sample_values()),
        display: DisplayIdentity::default(),
    };
    let mut json = serde_json::to_value(&modification).unwrap();
    let object = json.as_object_mut().unwrap();
    for key in [
        "display_source",
        "printed_ref",
        "token_image_ref",
        "token_art",
    ] {
        object.remove(key);
    }
    if let Value::Object(keys) = display_keys {
        object.extend(keys);
    }
    json
}

#[test]
fn legacy_flat_copy_values_json_deserializes_into_display_identity() {
    let display = sample_display();
    let json = legacy_copy_values_json(json!({
        "display_source": serde_json::to_value(display.display_source).unwrap(),
        "printed_ref": serde_json::to_value(&display.printed_ref).unwrap(),
        "token_image_ref": serde_json::to_value(&display.token_image_ref).unwrap(),
        "token_art": serde_json::to_value(&display.token_art).unwrap(),
    }));
    let parsed: ContinuousModification = serde_json::from_value(json).unwrap();
    let ContinuousModification::CopyValues { display: got, .. } = parsed else {
        panic!("expected CopyValues");
    };
    assert_eq!(got, display);
}

#[test]
fn copy_values_json_without_display_keys_defaults() {
    let json = legacy_copy_values_json(json!({}));
    let object = json.as_object().unwrap();
    // Reach-guard: the payload really lacks every display key.
    assert!(!object.contains_key("display_source") && !object.contains_key("token_art"));
    let parsed: ContinuousModification = serde_json::from_value(json).unwrap();
    let ContinuousModification::CopyValues { display, .. } = parsed else {
        panic!("expected CopyValues");
    };
    assert_eq!(display, DisplayIdentity::default());
}

#[test]
fn copy_values_round_trips_and_stays_flat() {
    let modification = ContinuousModification::CopyValues {
        values: Box::new(sample_values()),
        display: sample_display(),
    };
    let json = serde_json::to_value(&modification).unwrap();
    assert!(
        json.get("display").is_none() && json.get("token_art").is_some(),
        "the bundle is flattened: display keys sit at the variant level"
    );
    let back: ContinuousModification = serde_json::from_value(json).unwrap();
    assert_eq!(back, modification);
}

#[test]
fn legacy_latched_snapshot_without_token_art_deserializes() {
    let snapshot = LatchedCopiableSnapshot {
        values: sample_values(),
        display: DisplayIdentity::default(),
    };
    let mut json = serde_json::to_value(&snapshot).unwrap();
    let object = json.as_object_mut().unwrap();
    object.remove("token_art");
    object.remove("printed_ref");
    object.remove("token_image_ref");
    object.insert("display_source".to_string(), json!("Token"));
    let parsed: LatchedCopiableSnapshot = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.display.display_source, DisplaySource::Token);
    assert_eq!(parsed.display.token_art, None);

    let full = LatchedCopiableSnapshot {
        values: sample_values(),
        display: sample_display(),
    };
    let back: LatchedCopiableSnapshot =
        serde_json::from_value(serde_json::to_value(&full).unwrap()).unwrap();
    assert_eq!(back, full);
}

/// A printed-card-sourced `CopyTokenSpec` whose display fields are all absent,
/// so stripping their keys reproduces the pre-bundle skip-serialized shape.
fn printed_card_copy_token_spec() -> CopyTokenSpec {
    CopyTokenSpec {
        values: Box::new(sample_values()),
        display: DisplayIdentity {
            display_source: DisplaySource::Card,
            printed_ref: None,
            token_image_ref: None,
            token_art: None,
        },
        extra_keywords: Vec::new(),
        additional_modifications: Vec::new(),
        tapped: false,
        enters_attacking: false,
        sacrifice_at: None,
        source_id: ObjectId(1),
        controller: PlayerId(0),
        // Synthetic spec: no donor in scope (serde shape fixture only).
        donor: None,
    }
}

#[test]
fn legacy_copy_token_spec_json_without_display_keys_round_trips() {
    let spec = printed_card_copy_token_spec();
    let mut json = serde_json::to_value(&spec).unwrap();
    let object = json.as_object_mut().unwrap();
    // Reach-guard: the current shape writes every display key, so each removal is real.
    for key in ["printed_ref", "token_image_ref", "token_art"] {
        assert!(object.remove(key).is_some(), "{key} must be serialized");
    }
    let parsed: CopyTokenSpec = serde_json::from_value(json).unwrap();
    assert_eq!(parsed, spec);
}

#[test]
fn legacy_copy_token_spec_json_without_display_source_defaults_to_card() {
    let spec = printed_card_copy_token_spec();
    let mut json = serde_json::to_value(&spec).unwrap();
    let object = json.as_object_mut().unwrap();
    // Reach-guard: every display key, including `display_source`, is present before removal.
    for key in [
        "display_source",
        "printed_ref",
        "token_image_ref",
        "token_art",
    ] {
        assert!(object.remove(key).is_some(), "{key} must be serialized");
    }
    let parsed: CopyTokenSpec = serde_json::from_value(json).unwrap();
    assert_eq!(parsed.display.display_source, DisplaySource::Card);
    assert_eq!(parsed, spec);
}
