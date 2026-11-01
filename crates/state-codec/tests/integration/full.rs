use std::fs::File;
use std::io::BufReader;

use engine::game::scenario::{GameScenario, P0};
use engine::game::visibility::{filter_state_for_unseated_viewer, filter_state_for_viewer};
use engine::types::ability::{
    CastingPermission, Effect, ExileGrantCostProvenance, ResolutionCastCleanup,
    ResolutionCastDelayedTriggerReceipt, ResolutionCastFacePolicy, ResolutionCastSuccessAction,
    ResolutionMvRejectAction, ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastOfferKind, DepartedStackSpell, GameState, PersistedGameState, StackEntry, StackEntryKind,
    WaitingFor,
};
use engine::types::identifiers::{
    CardId, DelayedTriggerInstanceId, DelayedTriggerToken, ObjectId, ResolutionCastOfferId,
};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use serde_json::Value;
use state_codec::{decode, encode, state_hash, Frame, WireState};

fn round_trip(wire: &WireState, baseline: Option<&WireState>) -> WireState {
    let bytes = encode(baseline, wire).unwrap().to_bytes().unwrap();
    let decoded = decode(baseline, &Frame::from_bytes(&bytes).unwrap()).unwrap();
    assert_eq!(
        decoded.as_bytes(),
        wire.as_bytes(),
        "entire JSON value must survive"
    );
    assert_eq!(state_hash(&decoded), state_hash(wire));
    decoded
}

fn audiences(state: &GameState) -> Vec<Option<PlayerId>> {
    let mut result: Vec<_> = state.players.iter().map(|p| Some(p.id)).collect();
    result.extend([Some(PlayerId(u8::MAX)), None]);
    result
}

fn projection(state: &GameState, viewer: Option<PlayerId>) -> GameState {
    match viewer {
        Some(player) => filter_state_for_viewer(state, player),
        None => filter_state_for_unseated_viewer(state),
    }
}

fn assert_allocators_redacted(value: &Value) {
    for field in [
        "next_delayed_trigger_instance",
        "next_delayed_trigger_token",
        "next_resolution_cast_offer_id",
    ] {
        assert_eq!(value[field], 0, "projected {field}");
    }
}

#[test]
fn full_fidelity_populated_state() {
    let mut scenario = GameScenario::new();
    scenario.add_creature(P0, "Populated board", 3, 4);
    scenario.add_creature_to_hand(P0, "Private card", 1, 2);
    let mut state = scenario.build().state().clone();
    state.players[0].life = 17;
    state
        .players_who_created_token_this_turn
        .extend([PlayerId(1), P0]);
    let expected = serde_json::to_vec(&state).unwrap();
    let wire = WireState::from_game_state(&state).unwrap();
    assert_eq!(wire.revision(), Some(state.state_revision));
    assert_eq!(wire.as_bytes(), expected);
    assert_eq!(round_trip(&wire, None).as_bytes(), expected);
}

#[test]
fn full_baseline_independence() {
    let mut scenario = GameScenario::new();
    scenario.add_creature_to_hand(P0, "Private baseline distinction", 1, 1);
    let state = scenario.build().state().clone();
    let current = WireState::from_game_state(&filter_state_for_viewer(&state, P0)).unwrap();
    let mut previous = state.clone();
    previous.players[0].life = 3;
    let prior = WireState::from_game_state(&filter_state_for_viewer(&previous, P0)).unwrap();
    let unrelated = WireState::from_json(br#"{"unrelated":true}"#).unwrap();
    let wrong_viewer =
        WireState::from_game_state(&filter_state_for_viewer(&state, PlayerId(1))).unwrap();
    let expected_frame = encode(None, &current).unwrap().to_bytes().unwrap();
    for baseline in [None, Some(&prior), Some(&unrelated), Some(&wrong_viewer)] {
        if let Some(baseline) = baseline {
            assert_ne!(
                baseline.as_bytes(),
                current.as_bytes(),
                "baseline reach guard"
            );
        }
        // FULL independence is a property of an explicitly requested FULL, not
        // of every baseline-taking encode now that compatible inputs can DELTA.
        let frame = Frame::from_bytes(&expected_frame).unwrap();
        for decoder_baseline in [None, Some(&prior), Some(&unrelated), Some(&wrong_viewer)] {
            assert_eq!(
                decode(decoder_baseline, &frame).unwrap().as_bytes(),
                current.as_bytes()
            );
        }
        round_trip(&current, baseline);
    }
}

#[test]
fn envelope_orders_and_type_first_writer() {
    let state = br#"{ "z":2, "a":{"y":1,"x":0} }"#;
    let wire = WireState::from_json(state).unwrap();
    let expected = format!(
        "{{\"type\":\"Full\",\"data\":{{\"state\":{}}}}}",
        wire.as_json()
    );
    assert_eq!(
        encode(None, &wire).unwrap().to_bytes().unwrap(),
        expected.as_bytes()
    );
    let reversed = format!(
        "{{\"data\":{{\"state\":{}}},\"type\":\"Full\"}}",
        wire.as_json()
    );
    for bytes in [expected.as_bytes(), reversed.as_bytes()] {
        assert_eq!(
            decode(None, &Frame::from_bytes(bytes).unwrap())
                .unwrap()
                .as_bytes(),
            state
        );
    }
}

fn rejects_with_valid_control(invalid: &[u8]) {
    let valid = br#"{"type":"Full","data":{"state":{"present":1}}}"#;
    assert_eq!(
        decode(None, &Frame::from_bytes(valid).unwrap())
            .unwrap()
            .as_json(),
        r#"{"present":1}"#
    );
    assert!(
        Frame::from_bytes(invalid).is_err(),
        "invalid frame accepted: {invalid:?}"
    );
}

#[test]
fn envelope_rejects_duplicate_required_fields() {
    for invalid in [
        br#"{"type":"Full","type":"Full","data":{"state":0}}"#.as_slice(),
        br#"{"type":"Full","data":{"state":0},"data":{"state":1}}"#,
        br#"{"type":"Full","data":{"state":0,"state":1}}"#,
    ] {
        rejects_with_valid_control(invalid);
    }
}

#[test]
fn envelope_rejects_missing_required_fields() {
    for invalid in [
        br#"{"data":{"state":0}}"#.as_slice(),
        br#"{"type":"Full"}"#,
        br#"{"type":"Full","data":{}}"#,
    ] {
        rejects_with_valid_control(invalid);
    }
}

#[test]
fn envelope_rejects_unknown_tag() {
    rejects_with_valid_control(br#"{"type":"Unknown","data":{"state":0}}"#);
}
#[test]
fn envelope_rejects_truncated_json() {
    rejects_with_valid_control(br#"{"type":"Full","data":{"state":0}"#);
}
#[test]
fn envelope_rejects_trailing_garbage() {
    rejects_with_valid_control(br#"{"type":"Full","data":{"state":0}}x"#);
}
#[test]
fn envelope_rejects_invalid_utf8() {
    rejects_with_valid_control(b"{\"type\":\"Full\",\"data\":{\"state\":\"\xff\"}}");
    // An ignored unknown envelope value must not hide invalid UTF-8 from validation.
    rejects_with_valid_control(b"{\"type\":\"Full\",\"data\":{\"state\":null},\"extra\":\"\xff\"}");
}

#[test]
fn envelope_rejects_positional_and_non_object_forms() {
    for invalid in [
        br#"["Full",[null]]"#.as_slice(),
        br#"{"type":"Full","data":[null]}"#,
        br#"{"type":"Full","data":null}"#,
        br#"{"type":{"Full":null},"data":{"state":null}}"#,
        br#"[{"type":"Full","data":{"state":null}}]"#,
    ] {
        rejects_with_valid_control(invalid);
    }
    // A JSON-escaped spelling of the tag is the same string and stays accepted, in either
    // key order; an escaped unknown tag and an object-valued tag are still rejected.
    for valid in [
        br#"{"type":"\u0046ull","data":{"state": [1, 2]}}"#.as_slice(),
        br#"{"data":{"state": [1, 2]},"type":"Fu\u006cl"}"#,
    ] {
        assert_eq!(
            decode(None, &Frame::from_bytes(valid).unwrap())
                .unwrap()
                .as_json(),
            "[1, 2]"
        );
    }
    rejects_with_valid_control(br#"{"type":"\u0055nknown","data":{"state":null}}"#);
    // Unknown valid fields stay tolerated, in either key order.
    for valid in [
        br#"{"type":"Full","data":{"state":null,"x":1},"extra":[1]}"#.as_slice(),
        "{\"extra\":\"é\",\"data\":{\"state\":null},\"type\":\"Full\"}".as_bytes(),
    ] {
        assert_eq!(
            decode(None, &Frame::from_bytes(valid).unwrap())
                .unwrap()
                .as_json(),
            "null"
        );
    }
}

#[test]
fn raw_value_lexical_preservation() {
    for raw in [
        r#"{ "z" : [ 1, 2 ], "a" : { "y":0, "x":1 } }"#,
        r#"{"duplicate":1,"duplicate":2}"#,
        r#"{"u64":18446744073709551615,"u128":340282366920938463463374607431768211455}"#,
        r#"{"positive":1e999999,"negative":-1E-999999}"#,
        r#"{"unicode":"Éowyn 🧙 \u0061","empty":{},"array":[],"null":null,"string":""}"#,
        "null",
        "[]",
        "{}",
        "\"\"",
    ] {
        let wire = WireState::from_json(raw.as_bytes()).unwrap();
        assert_eq!(wire.as_json(), raw);
        assert_eq!(round_trip(&wire, None).as_json(), raw);
    }
    let a = WireState::from_json(br#"{"a":1,"b":2}"#).unwrap();
    let b = WireState::from_json(br#"{"b":2,"a":1}"#).unwrap();
    assert_ne!(
        state_hash(&a),
        state_hash(&b),
        "codec must not canonicalize arbitrary JSON"
    );
}

#[test]
fn raw_input_rejects_invalid_json_with_valid_controls() {
    assert_eq!(
        WireState::from_json(b"  null \n").unwrap().as_json(),
        "null"
    );
    for bad in [b"".as_slice(), b"null null", b"[", b"\"\xff\""] {
        assert!(
            WireState::from_json(bad).is_err(),
            "invalid raw state accepted"
        );
    }
}

#[test]
fn sha256_known_answer_and_outer_whitespace() {
    // Independently established with: printf %s '{"answer":42}' | shasum -a 256
    let expected = "ecf59a2696ca44a417e20e2a7eabb1b26e82c779f8546bea354a2cc80e8e1eed";
    for input in [br#"{"answer":42}"#.as_slice(), b" \n\t{\"answer\":42}\r\n "] {
        let wire = WireState::from_json(input).unwrap();
        assert_eq!(wire.as_json(), r#"{"answer":42}"#);
        assert_eq!(state_hash(&wire).to_string(), expected);
        assert_eq!(state_hash(&round_trip(&wire, None)).to_string(), expected);
    }
}

#[test]
fn deterministic_engine_serialization_and_ordered_arrays() {
    let sorted: Vec<_> = (0..32).map(PlayerId).collect();
    let mut pair = None;
    for _ in 0..32 {
        let mut a = GameState::new_two_player(88);
        let mut b = a.clone();
        a.players_who_created_token_this_turn
            .extend(sorted.iter().copied());
        b.players_who_created_token_this_turn
            .extend(sorted.iter().rev().copied());
        let a_native: Vec<_> = a
            .players_who_created_token_this_turn
            .iter()
            .copied()
            .collect();
        let b_native: Vec<_> = b
            .players_who_created_token_this_turn
            .iter()
            .copied()
            .collect();
        if a_native != b_native && (a_native != sorted || b_native != sorted) {
            pair = Some((a, b));
            break;
        }
    }
    let (a, mut b) =
        pair.expect("bounded construction must reach distinct, noncanonical native orders");
    let first = WireState::from_game_state(&a).unwrap();
    let second = WireState::from_game_state(&b).unwrap();
    assert_eq!(
        first.as_bytes(),
        second.as_bytes(),
        "insertion histories must serialize identically"
    );
    assert_eq!(state_hash(&first), state_hash(&second));
    round_trip(&first, Some(&second));
    b.players.reverse();
    let reordered = WireState::from_game_state(&b).unwrap();
    assert_ne!(
        reordered.as_bytes(),
        first.as_bytes(),
        "ordered vectors are not sets"
    );
    assert_ne!(state_hash(&reordered), state_hash(&first));
}

fn restore_dump(name: &str) -> GameState {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../engine/tests/fixtures")
        .join(name);
    let document: Value = serde_json::from_reader(flate2::read::GzDecoder::new(BufReader::new(
        File::open(path).unwrap(),
    )))
    .unwrap();
    let restore = |mut value: Value| {
        // Committed pre-onset capture; onset-independent test (see record_captured_onsets).
        engine::types::game_state::record_captured_onsets(&mut value);
        serde_json::from_value::<PersistedGameState>(value)
            .map_err(|e| e.to_string())
            .and_then(|p| p.into_game_state().map_err(|e| format!("{e:?}")))
    };
    let mut state = restore(document.clone());
    if state.is_err() {
        if let Some(board) = document.get("gameState") {
            state = restore(board.clone());
        }
    }
    state.unwrap_or_else(|e| panic!("{name}: authoritative restoration failed: {e}"))
}

#[test]
fn committed_dumps_preserve_every_audience_and_hidden_identity() {
    for (name, seats) in [
        ("dina_conqueror_4p.json.gz", 4),
        ("codie_turn14.json.gz", 2),
        ("combo_infinite_pile_4p_untapped_precast.json.gz", 4),
        ("combo_infinite_pile_4p_offer.json.gz", 4),
    ] {
        let state = restore_dump(name);
        assert_eq!(state.players.len(), seats);
        assert!(
            state.objects.values().any(|o| o.zone == Zone::Battlefield),
            "{name}: populated board"
        );
        for viewer in audiences(&state) {
            let projected = projection(&state, viewer);
            let hidden: Vec<_> = projected
                .objects
                .iter()
                .filter(|(id, o)| {
                    o.name == "Hidden Card" && state.objects[*id].name != "Hidden Card"
                })
                .map(|(id, _)| *id)
                .collect();
            assert!(
                !hidden.is_empty(),
                "{name} {viewer:?}: named authoritative identities must be hidden"
            );
            let expected = serde_json::to_vec(&projected).unwrap();
            let value: Value = serde_json::from_slice(&expected).unwrap();
            assert_allocators_redacted(&value);
            for id in hidden {
                assert_eq!(value["objects"][id.0.to_string()]["name"], "Hidden Card");
            }
            let wire = WireState::from_game_state(&projected).unwrap();
            assert_eq!(wire.as_bytes(), expected);
            round_trip(&wire, None);
        }
    }
}

fn cleanup(source: ObjectId) -> ResolutionCastCleanup {
    ResolutionCastCleanup {
        source_id: source,
        offer_id: Some(ResolutionCastOfferId(73)),
        face_policy: ResolutionCastFacePolicy::new(TargetFilter::Any, source, P0, None),
        exiled_misses: vec![],
        reject_action: ResolutionMvRejectAction::RemainExiled,
        success_action: ResolutionCastSuccessAction::BottomMisses,
        delayed_trigger_receipts: vec![ResolutionCastDelayedTriggerReceipt {
            offer_id: ResolutionCastOfferId(73),
            token: DelayedTriggerToken(37),
            instance: DelayedTriggerInstanceId(41),
            source_id: source,
        }],
        indicated_action: None,
    }
}

fn assert_cleanup_redacted(before: &Value, after: &Value) {
    assert_eq!(before["offer_id"], 73);
    assert_eq!(
        before["delayed_trigger_receipts"].as_array().unwrap().len(),
        1
    );
    assert!(after.get("offer_id").is_none_or(Value::is_null));
    assert!(after
        .get("delayed_trigger_receipts")
        .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty)));
    assert_eq!(after["source_id"], before["source_id"]);
}

#[test]
fn hostile_cleanup_offer_object_and_departed_stack() {
    // Serialization fixtures only; these synthetic carriers do not claim gameplay validity.
    let mut state = GameState::new(FormatConfig::free_for_all(), 2, 88);
    let source = engine::game::zones::create_object(
        &mut state,
        CardId(9001),
        P0,
        "Cleanup source".into(),
        Zone::Battlefield,
    );
    let object = engine::game::zones::create_object(
        &mut state,
        CardId(9002),
        P0,
        "Cleanup carrier".into(),
        Zone::Exile,
    );
    let permission = CastingPermission::ExileWithAltCost {
        cost: ManaCost::SelfManaCost,
        cost_provenance: ExileGrantCostProvenance::NormalCost,
        cast_transformed: false,
        constraint: None,
        granted_to: Some(P0),
        resolution_cleanup: Some(cleanup(source)),
        duration: None,
        source_id: None,
        graveyard_replacement: None,
        enters_with_counter: None,
        enters_with_modifications: Default::default(),
        mana_spend_permission: None,
        cast_cost_modifier: None,
    };
    state.objects.get_mut(&object).unwrap().casting_permissions = vec![permission];
    let departed = DepartedStackSpell {
        entry: StackEntry {
            id: object,
            source_id: object,
            controller: P0,
            kind: StackEntryKind::ActivatedAbility {
                source_id: object,
                ability: Box::new(ResolvedAbility::new(Effect::NoOp, vec![], object, P0)),
            },
        },
        object: Box::new(state.objects[&object].clone()),
    };
    state
        .departed_stack_spells
        .entry(object)
        .or_default()
        .insert(0, departed);
    state.waiting_for = WaitingFor::CastOffer {
        player: P0,
        kind: CastOfferKind::GraveyardPaidCast {
            hit_card: object,
            mana_spend_permission: None,
            graveyard_replacement: None,
            enters_with_modifications: Default::default(),
            cast_transformed: false,
            additional_cost: None,
            cleanup: cleanup(source),
        },
    };
    let original = serde_json::to_value(&state).unwrap();
    for viewer in audiences(&state) {
        let projected = projection(&state, viewer);
        let after = serde_json::to_value(&projected).unwrap();
        assert_allocators_redacted(&after);
        for pointer in [
            "/waiting_for/data/kind/cleanup".to_owned(),
            format!(
                "/objects/{}/casting_permissions/0/resolution_cleanup",
                object.0
            ),
            format!(
                "/departed_stack_spells/{}/0/object/casting_permissions/0/resolution_cleanup",
                object.0
            ),
        ] {
            assert_cleanup_redacted(
                original
                    .pointer(&pointer)
                    .unwrap_or_else(|| panic!("missing original {pointer}")),
                after
                    .pointer(&pointer)
                    .unwrap_or_else(|| panic!("missing projected {pointer}")),
            );
        }
        round_trip(&WireState::from_game_state(&projected).unwrap(), None);
    }
}

#[test]
fn hostile_triggered_stack_projection_preserves_redacted_firing_authority() {
    // Oracle verified against https://api.scryfall.com/cards/named?exact=Elvish%20Visionary.
    // This tests serialization of a production-created trigger, not its game-rule outcome.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let card = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Elvish Visionary",
            1,
            1,
            "When this creature enters, draw a card.",
        )
        .id();
    let mut runner = scenario.build();
    runner.cast(card).commit();
    let mut reached = None;
    for _ in 0..8 {
        let value = serde_json::to_value(runner.state()).unwrap();
        if value.get("stack_trigger_firings").is_some_and(|v| {
            v.as_object().is_some_and(|m| !m.is_empty())
                || v.as_array().is_some_and(|a| !a.is_empty())
        }) {
            reached = Some(runner.state().clone());
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("production priority pass accepted");
    }
    let state =
        reached.expect("production cast/pass path must reach a real triggered stack firing");
    assert!(state
        .stack
        .iter()
        .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })));
    for viewer in audiences(&state) {
        let projected = projection(&state, viewer);
        let value = serde_json::to_value(&projected).unwrap();
        assert!(value
            .get("stack_trigger_firings")
            .is_none_or(|v| v.as_object().is_some_and(|m| m.is_empty())
                || v.as_array().is_some_and(|a| a.is_empty())));
        assert_allocators_redacted(&value);
        let wire = WireState::from_game_state(&projected).unwrap();
        assert_eq!(
            round_trip(&wire, None).as_bytes(),
            serde_json::to_vec(&projected).unwrap()
        );
    }
}
