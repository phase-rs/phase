use serde::Deserialize;
use serde_json::{json, Value};
use state_codec::{decode, encode, state_hash, CodecError, Frame, FrameKind, WireState};

const REVISION: u64 = 9_007_199_254_740_993;

fn wire(text: &str, revision: u64) -> WireState {
    WireState::from_json(text.as_bytes())
        .unwrap()
        .with_revision(revision)
}

fn objects(extra: &str) -> String {
    // Large unchanged object content keeps mutation tests DELTA-reachable under
    // strict serialized-size selection.
    format!(
        r#"{{"17":{{"name":"Stable permanent","oracle_text":"{}"}},"23":{{"power":2{extra}}}}}"#,
        "A reusable unchanged object description. ".repeat(100)
    )
}

fn message(root: &str, data: &str, state: &str) -> String {
    let definitions = objects("");
    format!(
        r#"{{"type":"StateUpdate","definitions":{definitions},{root}"data":{{{data}"state":{{{state}}}}}}}"#
    )
}

fn tag(bytes: &[u8]) -> String {
    #[derive(Deserialize)]
    struct Tag {
        #[serde(rename = "type")]
        tag: String,
    }
    serde_json::from_slice::<Tag>(bytes).unwrap().tag
}

fn round_trip(base: Option<&WireState>, target: &WireState, expected_tag: &str) -> Vec<u8> {
    let bytes = encode(base, target).unwrap().to_bytes().unwrap();
    assert_eq!(tag(&bytes), expected_tag, "frame selection reach guard");
    let parsed = Frame::from_bytes(&bytes).unwrap();
    let actual = decode(base, &parsed).unwrap();
    assert_eq!(actual.as_bytes(), target.as_bytes());
    assert_eq!(state_hash(&actual), state_hash(target));
    assert_eq!(actual.revision(), target.revision());
    bytes
}

fn control() -> (WireState, WireState, Vec<u8>) {
    let objects = objects("");
    let base = wire(
        &format!(r#"{{"counter":1,"old":null,"objects":{objects}}}"#),
        REVISION,
    );
    let target = wire(
        &format!(r#"{{"counter":2,"old":null,"objects":{objects}}}"#),
        REVISION,
    );
    assert_ne!(base.as_bytes(), target.as_bytes());
    // The receiver advances from decoded output, independently of the sender.
    let full = round_trip(None, &base, "Full");
    let receiver = decode(None, &Frame::from_bytes(&full).unwrap()).unwrap();
    let bytes = round_trip(Some(&receiver), &target, "Delta");
    (receiver, target, bytes)
}

fn with_patch(bytes: &[u8], patch: &str) -> Vec<u8> {
    let value: Value = serde_json::from_slice(bytes).unwrap();
    let data = &value["data"];
    format!(r#"{{"type":"Delta","data":{{"base_revision":{},"revision":{},"base_hash":{},"result_hash":{},"patch":{patch}}}}}"#,
        data["base_revision"], data["revision"], data["base_hash"], data["result_hash"]).into_bytes()
}

#[test]
fn frame_kind_tracks_payload_through_serialization() {
    let (base, target, _) = control();
    let full = encode(None, &target).unwrap();
    let delta = encode(Some(&base), &target).unwrap();
    let full_bytes = full.to_bytes().unwrap();
    let delta_bytes = delta.to_bytes().unwrap();
    assert_eq!(tag(&full_bytes), "Full");
    assert_eq!(tag(&delta_bytes), "Delta", "delta selection reach guard");
    assert!(delta_bytes.len() < full_bytes.len());
    assert_eq!(full.kind(), FrameKind::Full);
    assert_eq!(delta.kind(), FrameKind::Delta);
    for (frame, bytes) in [(full, full_bytes), (delta, delta_bytes)] {
        assert_eq!(Frame::from_bytes(&bytes).unwrap().kind(), frame.kind());
    }
}

#[test]
fn revision_metadata_full_spelling_and_shared_content() {
    let original = WireState::from_json(br#"{"state_revision":3}"#).unwrap();
    assert_eq!(
        original.revision(),
        None,
        "JSON fields are not revision authority"
    );
    let revised = original.clone().with_revision(REVISION);
    assert_eq!(original.as_bytes().as_ptr(), revised.as_bytes().as_ptr());
    assert_eq!(state_hash(&original), state_hash(&revised));
    assert_eq!(original.revision(), None);
    let bytes = round_trip(None, &revised, "Full");
    assert_eq!(
        String::from_utf8(bytes).unwrap(),
        format!(
            r#"{{"type":"Full","data":{{"state":{{"state_revision":3}},"revision":{REVISION}}}}}"#
        )
    );
    round_trip(None, &original, "Full");
    let (_, target, _) = control();
    round_trip(Some(&original), &target, "Full");
    round_trip(
        Some(&revised),
        &WireState::from_json(target.as_bytes()).unwrap(),
        "Full",
    );
    for revision in ["null", "-1", "1.5", "18446744073709551616"] {
        round_trip(None, &revised, "Full");
        let invalid = format!(r#"{{"type":"Full","data":{{"state":0,"revision":{revision}}}}}"#);
        assert!(Frame::from_bytes(invalid.as_bytes()).is_err());
    }
}

#[test]
fn d2_removals_at_every_scope_and_present_null_transitions() {
    let before_objects = objects(r#", "obsolete":null"#);
    let after_objects = objects("");
    let before = wire(
        &message(
            r#""command_id":null,"#,
            r#""log_entries":null,"#,
            &format!(r#""optional_state_key":null,"objects":{before_objects}"#),
        ),
        REVISION,
    );
    let after = wire(
        &message("", "", &format!(r#""objects":{after_objects}"#)),
        REVISION + 1,
    );
    let initial: Value = serde_json::from_str(before.as_json()).unwrap();
    assert!(initial.get("command_id").is_some());
    assert!(initial["data"].get("log_entries").is_some());
    assert!(initial["data"]["state"].get("optional_state_key").is_some());
    assert!(initial["data"]["state"]["objects"]["23"]
        .get("obsolete")
        .is_some());
    assert_ne!(before.as_bytes(), after.as_bytes());
    let bytes = round_trip(Some(&before), &after, "Delta");
    let frame: Value = serde_json::from_slice(&bytes).unwrap();
    let patch = &frame["data"]["patch"];
    assert_eq!(patch["message"]["remove"], json!(["command_id"]));
    assert_eq!(patch["data"]["remove"], json!(["log_entries"]));
    assert_eq!(patch["state"]["remove"], json!(["optional_state_key"]));
    assert!(patch["objects"]["upsert"]["23"].get("obsolete").is_none());
    let restored = round_trip(Some(&after), &before, "Delta");
    let patch: Value = serde_json::from_slice(&restored).unwrap();
    assert!(patch["data"]["patch"]["message"]["set"]
        .get("command_id")
        .unwrap()
        .is_null());
    assert!(patch["data"]["patch"]["data"]["set"]
        .get("log_entries")
        .unwrap()
        .is_null());
    assert!(patch["data"]["patch"]["state"]["set"]
        .get("optional_state_key")
        .unwrap()
        .is_null());
}

#[test]
fn d2_structural_children_missing_null_empty_and_populated() {
    let populated = objects("");
    let base = wire(
        &format!(r#"{{"counter":1,"definitions":{populated},"objects":{populated}}}"#),
        REVISION,
    );
    let changed = wire(
        &format!(r#"{{"counter":2,"definitions":{populated},"objects":{populated}}}"#),
        REVISION,
    );
    round_trip(Some(&base), &changed, "Delta");
    assert!(serde_json::from_str::<Value>(base.as_json()).unwrap()["objects"]["17"].is_object());
    for content in [
        r#""counter":2"#.to_owned(),
        r#""counter":2,"objects":null"#.to_owned(),
        r#""counter":2,"objects":{}"#.to_owned(),
    ] {
        let target = wire(
            &format!(r#"{{"definitions":{populated},{content}}}"#),
            REVISION + 1,
        );
        round_trip(Some(&base), &target, "Delta");
        round_trip(Some(&target), &base, "Delta");
    }
    let base = wire(
        &message("", "", &format!(r#""counter":1,"objects":{populated}"#)),
        REVISION,
    );
    let changed = wire(
        &message("", "", &format!(r#""counter":2,"objects":{populated}"#)),
        REVISION,
    );
    round_trip(Some(&base), &changed, "Delta");
    for raw in [
        r#"{"type":"StateUpdate"}"#,
        r#"{"type":"StateUpdate","data":null}"#,
        r#"{"type":"StateUpdate","data":{}}"#,
        r#"{"type":"StateUpdate","data":{"state":null}}"#,
    ] {
        let target = wire(raw, REVISION);
        round_trip(Some(&base), &target, "Full");
        round_trip(Some(&target), &base, "Full");
    }
    let empty_state = wire(&message("", "", ""), REVISION);
    round_trip(Some(&base), &empty_state, "Delta");
    round_trip(Some(&empty_state), &base, "Delta");
}

#[test]
fn d4_opaque_lexical_values_survive_delta() {
    let objects = objects("");
    let base = wire(
        &format!(r#"{{"value":0,"definitions":{objects},"objects":{objects}}}"#),
        REVISION,
    );
    for value in [
        "1",
        "1.0",
        "1e0",
        "-0",
        "340282366920938463463374607431768211455",
        "1e999999",
        "-1E-999999",
        "[ 1,  2 ]",
        r#"{"a":1,"\u0061":2}"#,
        r#""Éowyn \u0061""#,
    ] {
        let target = wire(
            &format!(r#"{{"value":{value},"definitions":{objects},"objects":{objects}}}"#),
            REVISION + 1,
        );
        assert_ne!(base.as_bytes(), target.as_bytes());
        round_trip(Some(&base), &target, "Delta");
    }
    let nested = format!(
        r#"{{"value":1,"definitions":{objects},"objects":{{"17":{{ "a":1, "a":2, "n":1.0 }},"23":{{"text":"{}"}}}}}}"#,
        "An unchanged sibling. ".repeat(200)
    );
    let target = wire(&nested, REVISION);
    round_trip(Some(&base), &target, "Delta");
}

#[test]
fn d4_structural_losslessness_boundaries_force_exact_full() {
    let populated = objects("");
    let bare = format!(r#"{{"counter":2,"objects":{populated}}}"#);
    let state = format!(r#""counter":2,"objects":{populated}"#);
    let valid = message("", "", &state);
    let compact = wire(&valid, REVISION);
    let changed = wire(
        &message("", "", &format!(r#""counter":3,"objects":{populated}"#)),
        REVISION,
    );
    round_trip(Some(&compact), &changed, "Delta");
    let malformed_shapes = [
        // Root, data, state and objects structural spacing.
        valid.replacen("{", "{ ", 1),
        valid.replacen(r#""data":{"#, r#""data":{ "#, 1),
        valid.replacen(r#""state":{"#, r#""state":{ "#, 1),
        valid.replacen(r#""objects":{"#, r#""objects":{ "#, 1),
        // Decoded key spelling cannot be reproduced by serde's map serializer.
        valid.replacen(r#""type""#, r#""\u0074ype""#, 1),
        valid.replacen(r#""state""#, r#""\u0073tate""#, 1),
        valid.replacen(r#""counter""#, r#""\u0063ounter""#, 1),
        valid.replacen(r#""objects":{"17""#, r#""objects":{"\u00317""#, 1),
        // Literal and escape-alias duplicates at every expanded scope.
        message(r#""x":0,"x":1,"#, "", &state),
        message(r#""x":0,"\u0078":1,"#, "", &state),
        message("", r#""x":0,"x":1,"#, &state),
        message("", r#""x":0,"\u0078":1,"#, &state),
        message("", "", &format!(r#""x":0,"x":1,{state}"#)),
        message("", "", &format!(r#""x":0,"\u0078":1,{state}"#)),
        message("", "", r#""objects":{"17":{},"17":{}}"#),
        message("", "", r#""objects":{"17":{},"\u00317":{}}"#),
    ];
    for raw in malformed_shapes {
        // Recheck the live DELTA control alongside each fallback boundary.
        round_trip(Some(&compact), &changed, "Delta");
        let target = wire(&raw, REVISION);
        round_trip(Some(&compact), &target, "Full");
        round_trip(Some(&target), &compact, "Full");
    }
    for raw in ["null", "[]", "1.0", &bare.replace("{", "{ ")] {
        let target = wire(raw, REVISION);
        round_trip(Some(&compact), &target, "Full");
    }
}

#[test]
fn d5_integrity_errors_are_typed_transactional_and_ordered() {
    let (receiver, target, bytes) = control();
    assert_eq!(
        receiver.revision(),
        target.revision(),
        "equal revisions may carry distinct payloads"
    );
    assert!(receiver.revision().unwrap() > (1 << 53));
    let frame = Frame::from_bytes(&bytes).unwrap();
    let snapshot = receiver.as_bytes().to_vec();
    assert!(matches!(
        decode(None, &frame),
        Err(CodecError::MissingBaseline)
    ));
    for candidate in [
        receiver.clone().with_revision(REVISION - 1),
        WireState::from_json(receiver.as_bytes()).unwrap(),
    ] {
        assert!(matches!(decode(Some(&candidate), &frame),
            Err(CodecError::BaseRevisionMismatch { expected: REVISION, actual }) if actual == candidate.revision()));
        assert_eq!(candidate.as_bytes(), snapshot);
    }
    for raw in [
        receiver
            .as_json()
            .replace(r#""counter":1"#, r#""counter":0"#),
        receiver
            .as_json()
            .replace("Stable permanent", "Another recipient"),
        target.as_json().to_owned(),
    ] {
        let wrong = wire(&raw, REVISION);
        assert_ne!(state_hash(&wrong), state_hash(&receiver));
        assert!(
            matches!(decode(Some(&wrong), &frame), Err(CodecError::BaseHashMismatch { expected, actual })
            if expected == state_hash(&receiver) && actual == state_hash(&wrong))
        );
    }
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains(r#""counter":2"#));
    let corrupted = text.replacen(r#""counter":2"#, r#""counter":3"#, 1);
    let corrupt_frame = Frame::from_bytes(corrupted.as_bytes()).unwrap();
    assert!(
        matches!(decode(Some(&receiver), &corrupt_frame), Err(CodecError::ResultHashMismatch { expected, actual })
        if expected == state_hash(&target) && actual != expected)
    );
    assert_eq!(receiver.as_bytes(), snapshot);
    assert_eq!(
        decode(Some(&receiver), &frame).unwrap().as_bytes(),
        target.as_bytes()
    );
}

#[test]
fn d6_reversed_and_escaped_delta_envelope_controls() {
    let (base, target, bytes) = control();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    for text in [
        format!(r#"{{"data":{},"type":"Delta"}}"#, value["data"]),
        format!(r#"{{"data":{},"type":"\u0044elta"}}"#, value["data"]),
    ] {
        let frame = Frame::from_bytes(text.as_bytes()).unwrap();
        assert_eq!(
            decode(Some(&base), &frame).unwrap().as_bytes(),
            target.as_bytes()
        );
        assert_eq!(
            frame.to_bytes().unwrap(),
            bytes,
            "deterministic type-first writer"
        );
    }
}

#[test]
fn d6_rejects_hostile_named_operation_grammar() {
    for patch in [
        r#"["State",{}]"#,
        r#"{"layout":"State","state":[]}"#,
        r#"{"layout":"State","state":null}"#,
        r#"{"layout":"State","objects":[]}"#,
        r#"{"layout":"State","objects":{"upsert":[]}}"#,
        r#"{"layout":"State","state":{"order":null}}"#,
        r#"{"layout":"State","objects":{"order":null}}"#,
        r#"{"layout":"State","state":{"set":[]}}"#,
        r#"{"layout":"State","state":{"set":{"counter":2,"counter":3}}}"#,
        r#"{"layout":"State","state":{"set":{"counter":2,"\u0063ounter":3}}}"#,
        r#"{"layout":"State","objects":{"upsert":{"17":{},"17":{}}}}"#,
        r#"{"layout":"State","objects":{"upsert":{"17":{},"\u00317":{}}}}"#,
        r#"{"layout":"State","objects":{"upsert":{"17":null}}}"#,
        r#"{"layout":"State","state":{"remove":["old","old"]}}"#,
        r#"{"layout":"State","objects":{"remove":["17","17"]}}"#,
        r#"{"layout":"State","state":{"set":{"counter":2},"remove":["counter"]}}"#,
        r#"{"layout":"State","objects":{"upsert":{"17":{}},"remove":["17"]}}"#,
        r#"{"layout":"State","state":{"order":["counter","old"]}}"#,
        r#"{"layout":"State","state":{"order":["counter","old","objects","extra"]}}"#,
        r#"{"layout":"State","state":{"order":["counter","counter","objects"]}}"#,
        r#"{"layout":"State","objects":{"order":["17"]}}"#,
        r#"{"layout":"State","objects":{"order":["17","23","extra"]}}"#,
        r#"{"layout":"State","objects":{"order":["17","17"]}}"#,
        r#"{"layout":"State","state":{"remove":["missing"]}}"#,
        r#"{"layout":"State","objects":{"remove":["missing"]}}"#,
        r#"{"layout":"State","state":{"set":{"objects":{}}},"objects":{}}"#,
        r#"{"layout":"State","state":{"remove":["objects"]},"objects":{}}"#,
        r#"{"layout":"State","message":{}}"#,
        r#"{"layout":"Message","state":{}}"#,
        r#"{"layout":"State","unknown":{}}"#,
        r#"{"layout":"State","state":{"unknown":{}}}"#,
        r#"{"layout":"State","objects":{"unknown":{}}}"#,
        r#"{"layout":"State","layout":"State"}"#,
        r#"{"layout":"State","state":{},"state":{}}"#,
        r#"{"layout":"State","state":{"set":{},"set":{}}}"#,
    ] {
        let (base, _, valid) = control();
        let invalid = with_patch(&valid, patch);
        match Frame::from_bytes(&invalid) {
            Err(_) => {}
            Ok(frame) => assert!(
                matches!(
                    decode(Some(&base), &frame),
                    Err(CodecError::InvalidDelta(_))
                ),
                "operation must fail validation, not merely result hashing: {patch}"
            ),
        }
    }
}

#[test]
fn d6_rejects_metadata_hash_and_utf8_corruption() {
    let (_, _, valid) = control();
    let value: Value = serde_json::from_slice(&valid).unwrap();
    let hash = value["data"]["base_hash"].as_str().unwrap();
    for invalid in [
        String::from_utf8(valid.clone())
            .unwrap()
            .replacen(
                &format!(r#""base_revision":{REVISION}"#),
                &format!(r#""base_revision":{REVISION},"base_revision":{REVISION}"#),
                1,
            )
            .into_bytes(),
        String::from_utf8(valid.clone())
            .unwrap()
            .replacen(
                &format!(r#""revision":{REVISION}"#),
                &format!(r#""revision":{REVISION},"revision":{REVISION}"#),
                1,
            )
            .into_bytes(),
        String::from_utf8(valid.clone())
            .unwrap()
            .replacen(hash, &hash.to_uppercase(), 1)
            .into_bytes(),
        String::from_utf8(valid.clone())
            .unwrap()
            .replacen(hash, "abc", 1)
            .into_bytes(),
        String::from_utf8(valid.clone())
            .unwrap()
            .replacen(hash, &format!("g{}", &hash[1..]), 1)
            .into_bytes(),
        {
            let mut value = value.clone();
            value["data"]["revision"] = json!(-1);
            serde_json::to_vec(&value).unwrap()
        },
        {
            let mut value = value.clone();
            value["data"]["base_revision"] = json!(1.5);
            serde_json::to_vec(&value).unwrap()
        },
        {
            let mut value = value.clone();
            value["data"]["extra_operation"] = json!(true);
            serde_json::to_vec(&value).unwrap()
        },
        {
            let mut bytes = valid.clone();
            bytes.insert(1, 0xff);
            bytes
        },
        {
            // Invalid UTF-8 inside an otherwise ignored envelope value.
            let mut bytes = valid.clone();
            bytes.pop();
            bytes.extend_from_slice(b",\"ignored\":\"\xff\"}");
            bytes
        },
    ] {
        control();
        assert!(
            Frame::from_bytes(&invalid).is_err(),
            "hostile metadata accepted"
        );
    }
}

#[test]
fn d6_parent_child_conflicts_and_missing_targets() {
    let (base, _, valid) = control();
    for patch in [
        r#"{"layout":"State","objects":{}}"#,
        r#"{"layout":"State","objects":{"remove":["17"]}}"#,
    ] {
        control();
        // Bind this syntactically valid operation to a matching hash/revision of
        // a baseline whose objects child is absent, null, or a non-map value.
        for raw in [
            r#"{"counter":1}"#,
            r#"{"counter":1,"objects":null}"#,
            r#"{"counter":1,"objects":[]}"#,
        ] {
            let missing = wire(raw, REVISION);
            let mut frame: Value = serde_json::from_slice(&with_patch(&valid, patch)).unwrap();
            frame["data"]["base_hash"] = json!(state_hash(&missing).to_string());
            let frame = Frame::from_bytes(&serde_json::to_vec(&frame).unwrap()).unwrap();
            assert!(matches!(
                decode(Some(&missing), &frame),
                Err(CodecError::InvalidDelta(_))
            ));
            assert_eq!(missing.as_json(), raw);
        }
    }
    let populated = objects("");
    let source = wire(
        &message("", "", &format!(r#""counter":1,"objects":{populated}"#)),
        REVISION,
    );
    let target = wire(
        &message("", "", &format!(r#""counter":2,"objects":{populated}"#)),
        REVISION,
    );
    for patch in [
        r#"{"layout":"Message","message":{"set":{"data":{}}},"state":{}}"#,
        r#"{"layout":"Message","message":{"remove":["data"]},"objects":{}}"#,
        r#"{"layout":"Message","data":{"set":{"state":{}}},"objects":{}}"#,
        r#"{"layout":"Message","data":{"remove":["state"]},"state":{}}"#,
        r#"{"layout":"Message","message":{"set":{"type":"GameStarted"}}}"#,
        r#"{"layout":"Message","message":{"remove":["type"]}}"#,
    ] {
        let valid = round_trip(Some(&source), &target, "Delta");
        let invalid = with_patch(&valid, patch);
        match Frame::from_bytes(&invalid) {
            Err(_) => {}
            Ok(frame) => assert!(matches!(
                decode(Some(&source), &frame),
                Err(CodecError::InvalidDelta(_))
            )),
        }
        assert_eq!(source.revision(), Some(REVISION));
    }
    assert_ne!(base.as_bytes(), target.as_bytes());
}

#[test]
fn d1_multilevel_whole_field_replacements() {
    let stable = objects("");
    let before = wire(
        &message(
            r#""command_id":"a","#,
            r#""events":[1],"#,
            &format!(r#""players":[{{"life":20,"old":true}}],"objects":{stable}"#),
        ),
        REVISION,
    );
    let after = wire(
        &message(
            r#""command_id":"b","#,
            r#""events":[2,3],"#,
            &format!(r#""players":[{{"life":19}}],"objects":{stable}"#),
        ),
        REVISION + 1,
    );
    assert_ne!(before.as_bytes(), after.as_bytes());
    let bytes = round_trip(Some(&before), &after, "Delta");
    let frame: Value = serde_json::from_slice(&bytes).unwrap();
    let patch = &frame["data"]["patch"];
    assert_eq!(patch["message"]["set"]["command_id"], "b");
    assert_eq!(patch["data"]["set"]["events"], json!([2, 3]));
    assert_eq!(patch["state"]["set"]["players"], json!([{"life":19}]));
    assert!(patch["objects"].get("upsert").is_none());
}

#[test]
fn d3_objects_replace_remove_add_and_preserve_explicit_order() {
    let stable = objects("");
    let before = wire(
        &format!(
            r#"{{"definitions":{stable},"objects":{{"a":{{"name":"A","old":1}},"b":{{"name":"B"}},"c":{{"name":"C"}}}}}}"#
        ),
        REVISION,
    );
    let after = wire(
        &format!(
            r#"{{"definitions":{stable},"objects":{{"c":{{"name":"C"}},"a":{{"name":"changed"}},"d":{{"name":"D"}}}}}}"#
        ),
        REVISION + 1,
    );
    let initial: Value = serde_json::from_str(before.as_json()).unwrap();
    assert_eq!(initial["objects"]["a"]["old"], 1);
    assert_eq!(initial["objects"]["b"]["name"], "B");
    assert_eq!(initial["objects"]["c"]["name"], "C");
    let bytes = round_trip(Some(&before), &after, "Delta");
    let patch: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(patch["data"]["patch"]["objects"]["remove"], json!(["b"]));
    assert_eq!(
        patch["data"]["patch"]["objects"]["order"],
        json!(["c", "a", "d"])
    );
    assert_eq!(
        patch["data"]["patch"]["objects"]["upsert"]["a"],
        json!({"name":"changed"})
    );
    assert_eq!(
        patch["data"]["patch"]["objects"]["upsert"]["d"],
        json!({"name":"D"})
    );
    let reordered = wire(
        &format!(
            r#"{{"definitions":{stable},"objects":{{"c":{{"name":"C"}},"b":{{"name":"B"}},"a":{{"name":"A","old":1}}}}}}"#
        ),
        REVISION,
    );
    let bytes = round_trip(Some(&before), &reordered, "Delta");
    let patch: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        patch["data"]["patch"]["objects"]["order"],
        json!(["c", "b", "a"])
    );
    assert!(patch["data"]["patch"]["objects"].get("upsert").is_none());
    let empty = wire(
        &format!(r#"{{"definitions":{stable},"objects":{{}}}}"#),
        REVISION,
    );
    round_trip(Some(&before), &empty, "Delta");
    round_trip(Some(&empty), &before, "Delta");
}

#[test]
fn d3_field_order_at_all_three_levels() {
    let stable = objects("");
    let before = wire(
        &message(
            r#""a":1,"b":2,"#,
            r#""a":1,"b":2,"#,
            &format!(r#""a":1,"b":2,"objects":{stable}"#),
        ),
        REVISION,
    );
    let after = wire(
        &format!(
            r#"{{"data":{{"state":{{"objects":{stable},"b":2,"a":1}},"b":2,"a":1}},"b":2,"a":1,"definitions":{stable},"type":"StateUpdate"}}"#
        ),
        REVISION,
    );
    assert_ne!(before.as_bytes(), after.as_bytes());
    let bytes = round_trip(Some(&before), &after, "Delta");
    let patch: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        patch["data"]["patch"]["message"]["order"],
        json!(["data", "b", "a", "definitions", "type"])
    );
    assert_eq!(
        patch["data"]["patch"]["data"]["order"],
        json!(["state", "b", "a"])
    );
    assert_eq!(
        patch["data"]["patch"]["state"]["order"],
        json!(["objects", "b", "a"])
    );
}

#[test]
fn d7_recipient_reset_integrity_recovery_and_game_started() {
    let (first, second, delta) = control();
    let mut sender = [Some(first.clone()), Some(first.clone())];
    let mut receiver = [
        Some(
            decode(
                None,
                &Frame::from_bytes(&round_trip(None, &first, "Full")).unwrap(),
            )
            .unwrap(),
        ),
        Some(
            decode(
                None,
                &Frame::from_bytes(&round_trip(None, &first, "Full")).unwrap(),
            )
            .unwrap(),
        ),
    ];
    for seat in 0..2 {
        let bytes = round_trip(sender[seat].as_ref(), &second, "Delta");
        receiver[seat] =
            Some(decode(receiver[seat].as_ref(), &Frame::from_bytes(&bytes).unwrap()).unwrap());
        sender[seat] = Some(second.clone());
    }
    sender[0] = None;
    receiver[0] = None;
    let third = wire(
        &second.as_json().replace(r#""counter":2"#, r#""counter":3"#),
        REVISION + 1,
    );
    for (seat, expected) in ["Full", "Delta"].into_iter().enumerate() {
        let bytes = round_trip(sender[seat].as_ref(), &third, expected);
        receiver[seat] =
            Some(decode(receiver[seat].as_ref(), &Frame::from_bytes(&bytes).unwrap()).unwrap());
        sender[seat] = Some(third.clone());
    }
    let fourth = wire(
        &third.as_json().replace(r#""counter":3"#, r#""counter":4"#),
        REVISION + 2,
    );
    for baseline in &receiver {
        round_trip(baseline.as_ref(), &fourth, "Delta");
    }
    assert!(matches!(
        decode(Some(&third), &Frame::from_bytes(&delta).unwrap()),
        Err(CodecError::BaseRevisionMismatch { .. })
    ));
    let wrong = wire(
        &first.as_json().replace("Stable permanent", "Wrong viewer"),
        REVISION,
    );
    assert!(matches!(
        decode(Some(&wrong), &Frame::from_bytes(&delta).unwrap()),
        Err(CodecError::BaseHashMismatch { .. })
    ));
    let full = round_trip(None, &second, "Full");
    let recovered = decode(Some(&wrong), &Frame::from_bytes(&full).unwrap()).unwrap();
    round_trip(Some(&recovered), &third, "Delta");
    let stable = objects("");
    let update1 = wire(
        &message("", "", &format!(r#""counter":1,"objects":{stable}"#)),
        REVISION,
    );
    let update2 = wire(
        &message("", "", &format!(r#""counter":2,"objects":{stable}"#)),
        REVISION + 1,
    );
    round_trip(Some(&update1), &update2, "Delta");
    let started1 = wire(
        &update1.as_json().replace("StateUpdate", "GameStarted"),
        REVISION,
    );
    let started2 = wire(
        &update2.as_json().replace("StateUpdate", "GameStarted"),
        REVISION + 1,
    );
    round_trip(Some(&started1), &started2, "Full");
}

#[test]
fn d8_size_bound_and_large_change_fallback() {
    let (before, after, bytes) = control();
    assert!(bytes.len() < round_trip(None, &after, "Full").len());
    for (base, target) in [
        (
            wire(r#"{"value":1}"#, REVISION),
            wire(r#"{"value":2}"#, REVISION),
        ),
        (
            before,
            wire(r#"{"objects":{},"value":"entirely replaced"}"#, REVISION),
        ),
    ] {
        assert_ne!(base.as_bytes(), target.as_bytes());
        let selected = round_trip(Some(&base), &target, "Full");
        assert_eq!(selected.len(), round_trip(None, &target, "Full").len());
    }
}

#[test]
fn baseline_memory_accounts_shared_index_and_object_digests() {
    let (base, _, _) = control();
    let memory = base.baseline_memory_usage();
    assert_eq!(memory.json_bytes, base.as_bytes().len());
    assert!(memory.index_key_bytes > 0);
    assert!(memory.object_hash_bytes >= 2 * 32);
    assert_eq!(memory.object_hash_bytes % 32, 0);
    let clone = base.clone().with_revision(1);
    assert_eq!(base.as_bytes().as_ptr(), clone.as_bytes().as_ptr());
    assert_eq!(
        memory.total_bytes(),
        clone.baseline_memory_usage().total_bytes()
    );
}
