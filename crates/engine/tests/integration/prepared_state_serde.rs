use engine::game::game_object::{
    BestowFormState, GameObject, MutateFormState, PreparedState, SignatureSpellState,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde_json::{Map, Value};

#[derive(Clone, Copy)]
struct MarkerObjectIds {
    prepared: ObjectId,
    bestow: ObjectId,
    mutate: ObjectId,
    unmarked: ObjectId,
    signature: ObjectId,
}

fn marker_state_fixture() -> (GameState, MarkerObjectIds) {
    let mut state = GameState::new_two_player(42);
    let ids = MarkerObjectIds {
        prepared: ObjectId(10_001),
        bestow: ObjectId(10_002),
        mutate: ObjectId(10_003),
        unmarked: ObjectId(10_004),
        signature: ObjectId(10_005),
    };

    let mut prepared = marker_object(ids.prepared, "Prepared Object");
    prepared.prepared = Some(PreparedState);
    state.objects.insert(ids.prepared, prepared);

    let mut bestow = marker_object(ids.bestow, "Bestow Object");
    bestow.bestow_form = Some(BestowFormState::default());
    state.objects.insert(ids.bestow, bestow);

    let mut mutate = marker_object(ids.mutate, "Mutate Object");
    mutate.mutate_form = Some(MutateFormState);
    state.objects.insert(ids.mutate, mutate);

    state
        .objects
        .insert(ids.unmarked, marker_object(ids.unmarked, "Unmarked Object"));

    let mut signature = marker_object(ids.signature, "Signature Object");
    signature.signature_spell = Some(SignatureSpellState {});
    state.objects.insert(ids.signature, signature);

    (state, ids)
}

fn marker_object(id: ObjectId, name: &str) -> GameObject {
    GameObject::new(
        id,
        CardId(id.0),
        PlayerId(0),
        name.to_string(),
        Zone::Battlefield,
    )
}

fn object_json(value: &Value, id: ObjectId) -> &Map<String, Value> {
    value
        .get("objects")
        .and_then(Value::as_object)
        .and_then(|objects| objects.get(&id.0.to_string()))
        .and_then(Value::as_object)
        .expect("fixture object is present in serialized GameState")
}

fn object_json_mut(value: &mut Value, id: ObjectId) -> &mut Map<String, Value> {
    value
        .get_mut("objects")
        .and_then(Value::as_object_mut)
        .and_then(|objects| objects.get_mut(&id.0.to_string()))
        .and_then(Value::as_object_mut)
        .expect("fixture object is present in serialized GameState")
}

fn assert_fixture_reachable(state: &GameState, ids: MarkerObjectIds) {
    for (id, expected_name) in [
        (ids.prepared, "Prepared Object"),
        (ids.bestow, "Bestow Object"),
        (ids.mutate, "Mutate Object"),
        (ids.unmarked, "Unmarked Object"),
        (ids.signature, "Signature Object"),
    ] {
        assert_eq!(
            state.objects.get(&id).map(|object| object.name.as_str()),
            Some(expected_name),
            "fixture object {id:?} must be reachable with the expected identity"
        );
    }
}

fn assert_marker_isolation(state: &GameState, ids: MarkerObjectIds) {
    let prepared = state
        .objects
        .get(&ids.prepared)
        .expect("prepared object exists");
    assert!(prepared.prepared.is_some());
    assert!(prepared.bestow_form.is_none());
    assert!(prepared.mutate_form.is_none());

    let bestow = state
        .objects
        .get(&ids.bestow)
        .expect("bestow object exists");
    assert!(bestow.prepared.is_none());
    assert!(bestow.bestow_form.is_some());
    assert!(bestow.mutate_form.is_none());

    let mutate = state
        .objects
        .get(&ids.mutate)
        .expect("mutate object exists");
    assert!(mutate.prepared.is_none());
    assert!(mutate.bestow_form.is_none());
    assert!(mutate.mutate_form.is_some());

    let unmarked = state
        .objects
        .get(&ids.unmarked)
        .expect("unmarked object exists");
    assert!(unmarked.prepared.is_none());
    assert!(unmarked.bestow_form.is_none());
    assert!(unmarked.mutate_form.is_none());
}

#[test]
fn game_state_roundtrip_preserves_unit_marker_presence() {
    let (state, ids) = marker_state_fixture();
    assert_fixture_reachable(&state, ids);
    assert_marker_isolation(&state, ids);
    assert!(state
        .objects
        .get(&ids.signature)
        .expect("signature object exists")
        .signature_spell
        .is_some());

    let serialized = serde_json::to_value(&state).expect("marker state serializes");
    assert_eq!(
        object_json(&serialized, ids.prepared).get("prepared"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        object_json(&serialized, ids.bestow).get("bestow_form"),
        Some(&Value::Bool(true))
    );
    assert_eq!(
        object_json(&serialized, ids.mutate).get("mutate_form"),
        Some(&Value::Bool(true))
    );

    let unmarked_json = object_json(&serialized, ids.unmarked);
    assert!(!unmarked_json.contains_key("prepared"));
    assert!(!unmarked_json.contains_key("bestow_form"));
    assert!(!unmarked_json.contains_key("mutate_form"));
    assert_eq!(
        object_json(&serialized, ids.signature).get("signature_spell"),
        Some(&Value::Object(Map::new())),
        "the adjacent braced-empty marker must retain its ordinary object wire shape"
    );

    let restored =
        serde_json::from_value::<GameState>(serialized).expect("marker state deserializes");
    assert_fixture_reachable(&restored, ids);
    assert_marker_isolation(&restored, ids);
    assert!(restored
        .objects
        .get(&ids.signature)
        .expect("signature object exists after restore")
        .signature_spell
        .is_some());
}

#[test]
fn legacy_null_markers_and_absent_none_fields_deserialize_losslessly() {
    let (state, ids) = marker_state_fixture();
    let mut legacy = serde_json::to_value(&state).expect("marker state serializes");

    object_json_mut(&mut legacy, ids.prepared).insert("prepared".into(), Value::Null);
    object_json_mut(&mut legacy, ids.bestow).insert("bestow_form".into(), Value::Null);
    object_json_mut(&mut legacy, ids.mutate).insert("mutate_form".into(), Value::Null);
    let unmarked = object_json_mut(&mut legacy, ids.unmarked);
    unmarked.remove("prepared");
    unmarked.remove("bestow_form");
    unmarked.remove("mutate_form");

    let restored =
        serde_json::from_value::<GameState>(legacy).expect("legacy marker state deserializes");
    assert_fixture_reachable(&restored, ids);
    assert_marker_isolation(&restored, ids);
}

#[test]
fn explicit_false_deserializes_as_absence_with_true_sibling() {
    let (state, ids) = marker_state_fixture();
    let mut serialized = serde_json::to_value(&state).expect("marker state serializes");

    object_json_mut(&mut serialized, ids.prepared).insert("prepared".into(), Value::Bool(false));
    object_json_mut(&mut serialized, ids.unmarked).insert("prepared".into(), Value::Bool(true));

    let restored =
        serde_json::from_value::<GameState>(serialized).expect("boolean marker state deserializes");
    assert_fixture_reachable(&restored, ids);
    assert!(restored
        .objects
        .get(&ids.prepared)
        .expect("false marker object exists")
        .prepared
        .is_none());
    assert!(restored
        .objects
        .get(&ids.unmarked)
        .expect("true sibling object exists")
        .prepared
        .is_some());
}

#[test]
fn invalid_unit_marker_wire_value_fails_closed() {
    let (state, ids) = marker_state_fixture();
    let mut serialized = serde_json::to_value(&state).expect("marker state serializes");

    let valid = serde_json::from_value::<GameState>(serialized.clone())
        .expect("the unmodified marker state must reach and pass GameState deserialization");
    assert_fixture_reachable(&valid, ids);
    assert_marker_isolation(&valid, ids);

    object_json_mut(&mut serialized, ids.prepared).insert(
        "prepared".into(),
        Value::String("not-a-presence-bit".into()),
    );

    assert!(
        serde_json::from_value::<GameState>(serialized).is_err(),
        "a present non-boolean, non-null marker value must be rejected"
    );
}

fn bestow_form_of(state: &GameState, id: ObjectId) -> Option<BestowFormState> {
    state
        .objects
        .get(&id)
        .expect("bestow object exists")
        .bestow_form
        .clone()
}

/// CR 205.1a + CR 702.103b: the bestow form carries the creature subtypes it
/// removed. An empty payload keeps the legacy bool wire shape; a non-empty one
/// round-trips as an object.
#[test]
fn bestow_form_payload_wire_shapes_round_trip() {
    let (mut state, ids) = marker_state_fixture();
    let payload = BestowFormState {
        removed_creature_subtypes: vec!["Insect".into(), "Monk".into()],
    };
    state
        .objects
        .get_mut(&ids.bestow)
        .expect("bestow object exists")
        .bestow_form = Some(payload.clone());

    let serialized = serde_json::to_value(&state).expect("marker state serializes");
    let wire = object_json(&serialized, ids.bestow)
        .get("bestow_form")
        .expect("bestow_form is serialized");
    assert_eq!(
        wire.get("removed_creature_subtypes"),
        Some(&serde_json::json!(["Insect", "Monk"])),
        "a non-empty payload serializes as the struct object"
    );

    let restored =
        serde_json::from_value::<GameState>(serialized).expect("payload state deserializes");
    assert_eq!(bestow_form_of(&restored, ids.bestow), Some(payload));
}

#[test]
fn bestow_form_legacy_and_false_wire_shapes_restore_empty_payload_or_none() {
    let (state, ids) = marker_state_fixture();
    for (wire, expected) in [
        (Value::Null, Some(BestowFormState::default())),
        (Value::Bool(true), Some(BestowFormState::default())),
        (Value::Bool(false), None),
    ] {
        let mut serialized = serde_json::to_value(&state).expect("marker state serializes");
        object_json_mut(&mut serialized, ids.bestow).insert("bestow_form".into(), wire.clone());
        let restored = serde_json::from_value::<GameState>(serialized)
            .unwrap_or_else(|e| panic!("bestow_form {wire} deserializes: {e}"));
        assert_eq!(
            bestow_form_of(&restored, ids.bestow),
            expected,
            "wire {wire}"
        );
    }

    let mut absent = serde_json::to_value(&state).expect("marker state serializes");
    object_json_mut(&mut absent, ids.bestow).remove("bestow_form");
    let restored = serde_json::from_value::<GameState>(absent).expect("absent field deserializes");
    assert_eq!(bestow_form_of(&restored, ids.bestow), None);
}

#[test]
fn populated_bestow_wire_requires_versioned_peer() {
    let (mut state, ids) = marker_state_fixture();
    let satyr_id = ObjectId(10_006);
    let mut satyr = marker_object(satyr_id, "Satyr payload neighbor");
    satyr.bestow_form = Some(BestowFormState {
        removed_creature_subtypes: vec!["Satyr".into()],
    });
    state.objects.insert(satyr_id, satyr);
    state.objects.get_mut(&ids.bestow).unwrap().bestow_form = Some(BestowFormState {
        removed_creature_subtypes: vec!["Insect".into(), "Monk".into()],
    });
    let serialized = serde_json::to_value(&state).expect("full GameState serializes");
    let wire = object_json(&serialized, ids.bestow)["bestow_form"].clone();
    assert_eq!(
        wire,
        serde_json::json!({ "removed_creature_subtypes": ["Insect", "Monk"] })
    );
    // The pre-payload production adapter used precisely Option<bool>.
    assert!(serde_json::from_value::<Option<bool>>(wire).is_err());
    for (wire, expected) in [
        (Value::Bool(true), Some(true)),
        (Value::Bool(false), Some(false)),
        (Value::Null, None),
    ] {
        assert_eq!(
            serde_json::from_value::<Option<bool>>(wire).unwrap(),
            expected
        );
    }
    let restored: GameState =
        serde_json::from_value(serialized.clone()).expect("current populated sibling decodes");
    assert_fixture_reachable(&restored, ids);
    assert_marker_isolation(&restored, ids);
    assert!(restored.objects[&ids.signature].signature_spell.is_some());
    for (id, expected) in [
        (ids.bestow, vec!["Insect", "Monk"]),
        (satyr_id, vec!["Satyr"]),
    ] {
        assert_eq!(restored.objects[&id].id, id);
        assert_eq!(
            restored.objects[&id]
                .bestow_form
                .as_ref()
                .unwrap()
                .removed_creature_subtypes,
            expected
        );
    }
    assert_eq!(restored.objects[&satyr_id].name, "Satyr payload neighbor");
    let mut invalid = serialized;
    object_json_mut(&mut invalid, ids.bestow).insert(
        "bestow_form".into(),
        Value::String("not-a-bestow-form".into()),
    );
    assert!(serde_json::from_value::<GameState>(invalid).is_err());
}
