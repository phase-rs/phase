//! A viewer projection announces itself on the wire and is refused at the
//! PERSISTENCE ingest — and only there.
//!
//! `filter_state_for_viewer` blanks ~20 private rules-execution carriers (the
//! Resolve All consent run, the stack-resolution session, every pending-resume
//! cursor, the rules journal, the RNG seed) while deliberately preserving the
//! public `waiting_for` that stands over them. Before this gate, the serialized
//! result was byte-indistinguishable from an authoritative state whose carriers
//! are legitimately absent, so installing one left a prompt no player could
//! answer (#8193 repaired one such shape; this makes the whole class
//! unrepresentable).
//!
//! The gate keys on `GameState::viewer_projection`, NOT on prompt shape — which
//! is what `carrier_less_authoritative_state_still_restores` and
//! `authoritative_consent_with_live_run_still_decodes` exist to prove.
//!
//! It is deliberately NOT hosted on the bare-`GameState` decode path. That path
//! is the multiplayer TRANSPORT: `ServerMessage::GameStarted { state, .. }`
//! carries a projection on purpose, so refusing there rejects the broadcast a
//! viewer is supposed to receive. `projection_decodes_on_the_transport_path_and\
//! _keeps_its_marker` pins that, and the marker surviving transport is what
//! leaves the persistence ingress something to refuse.

use engine::game::visibility::filter_state_for_viewer;
use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreFinalization, WaitingFor,
};
use engine::types::player::PlayerId;

use crate::resolve_all_consent::{pending_consent_with_live_run, pending_consent_without_its_run};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// The server's non-seat spectator broadcast id (`phase-server`'s
/// `SPECTATOR_PLAYER_ID`). A projection stamped with it must be refused exactly
/// like a seat projection — the sentinel is not an escape hatch.
const SPECTATOR: PlayerId = PlayerId(u8::MAX);

/// The exact player-visible refusal. Asserted by equality, never by `contains`:
/// the gate runs AFTER `materialize_prepared`, so reaching this text is itself
/// proof the payload deserialized structurally, and a loose check would accept
/// an earlier structural failure and silently retire that property.
const REFUSAL: &str =
    "This saved game only holds the view that was shown on screen, not the full game record.";

/// A projection of the reporter's shape, with its input reach asserted.
///
/// REACH-GUARD, input side. Every row below is worthless if the fixture is not
/// actually the hazardous shape: a public prompt standing over a blanked
/// private carrier.
fn projection_for(viewer: PlayerId) -> GameState {
    let authoritative = pending_consent_with_live_run();
    assert!(
        matches!(
            authoritative.waiting_for,
            WaitingFor::ResolveAllConsent { .. }
        ),
        "test precondition: the reducer-built fixture is parked at a consent prompt"
    );
    assert!(
        authoritative.resolve_all_consent_run.is_some(),
        "test precondition: the authoritative state still carries its private run"
    );
    assert!(
        authoritative.viewer_projection.is_none(),
        "test precondition: an authoritative state carries no marker"
    );

    let projection = filter_state_for_viewer(&authoritative, viewer);
    assert_eq!(
        projection.viewer_projection,
        Some(viewer),
        "the projection is stamped for the viewer it was computed for"
    );
    assert!(
        matches!(projection.waiting_for, WaitingFor::ResolveAllConsent { .. }),
        "the public prompt survives projection — this is the hazard being gated"
    );
    assert!(
        projection.resolve_all_consent_run.is_none(),
        "the private run does NOT survive projection — the other half of the hazard"
    );
    projection
}

/// The same enumeration for `GameStateDecode::decode_persisted_resolution_state`.
const INGRESS_B_PREDECESSORS: &[&str] = &[
    // `value.as_object_mut()`
    "persisted game state must be a JSON object",
    // the `match mode` arm
    "invalid persisted resolution-state decode mode",
    // `migrate_legacy_batched_zone_change_trigger_fired`
    "batched_zone_change_trigger_fired",
    // `reject_legacy_raw_prompt_authority`, reached inside `ResolutionStateWire::from_value`
    "legacy raw-ID",
    // `validate_restored_zone_change_replay_keys`
    "batched zone-change replay key",
    // `normalize_delayed_trigger_allocators`
    "delayed-trigger provenance",
];

fn assert_is_the_projection_refusal(error: &str, predecessors: &[&str], ingress: &str) {
    assert_eq!(
        error, REFUSAL,
        "{ingress} must refuse the projection as a projection"
    );
    for fragment in predecessors {
        assert!(
            !error.contains(fragment),
            "{ingress} refused for a pre-gate reason ({fragment}), so the row would pass \
             without ever reaching the gate: {error}"
        );
    }
}

/// Restores an AUTHORITATIVE state through the PERSISTENCE ingress — both arms —
/// and returns the restored state from each.
///
/// The positive controls must exercise THIS path, not a bare `GameState` decode.
/// The gate lives on persistence, so a regression that refused every unmarked
/// persisted state would leave a transport-path control green while real saves
/// stopped loading. Reported by review on #8202.
fn persisted_restore_both_arms(state: GameState) -> Vec<GameState> {
    let arms = [
        ("Raw", PersistedGameState::Raw(Box::new(state.clone()))),
        ("Trusted", PersistedGameState::capture(state)),
    ];
    arms.into_iter()
        .map(|(arm, persisted)| {
            let wire = serde_json::to_value(persisted)
                .unwrap_or_else(|e| panic!("{arm} persisted state serializes: {e}"));
            serde_json::from_value::<PersistedGameState>(wire)
                .unwrap_or_else(|e| panic!("{arm} authoritative save must restore: {e}"))
                .prepare_for_restore(PersistedRestoreFinalization::Immediate)
                .unwrap_or_else(|e| panic!("{arm} prepared restore: {e:?}"))
                .finalize_immediately()
                .unwrap_or_else(|e| panic!("{arm} finalized restore: {e:?}"))
        })
        .collect()
}

/// Decodes a projection through the PERSISTENCE ingress — the only one that
/// guards — and returns the refusal text.
fn persisted_decode_error(projection: GameState) -> String {
    let persisted = serde_json::to_value(PersistedGameState::Raw(Box::new(projection)))
        .expect("a raw persisted projection serializes");
    serde_json::from_value::<PersistedGameState>(persisted)
        .expect_err("a viewer projection must not restore through persistence")
        .to_string()
}

/// Matrix row 1 — the TRANSPORT decode path must KEEP WORKING.
///
/// REGRESSION: guarding this path refused the multiplayer broadcast.
/// `ServerMessage::GameStarted { state, .. }` (`server-core/src/protocol.rs`)
/// carries a `filter_state_for_viewer` projection BY DESIGN, and a client
/// receives it through exactly this bare-`GameState` deserialize. Refusing here
/// broke `phase-server`'s
/// `stale_websocket_cannot_retire_or_disconnect_a_replaced_full_seat`, which
/// failed on the real refusal text.
///
/// So a projection MUST decode here, and MUST keep its marker — the marker
/// surviving transport is precisely what leaves the persistence ingress
/// something to refuse later.
#[test]
fn projection_decodes_on_the_transport_path_and_keeps_its_marker() {
    let projection = projection_for(P0);
    let json = serde_json::to_string(&projection).expect("a projection serializes");

    let decoded = serde_json::from_str::<GameState>(&json)
        .expect("a viewer projection must decode on the transport path");
    assert_eq!(
        decoded.viewer_projection,
        Some(P0),
        "the marker must survive transport decode, or the persistence gate has \
         nothing left to read"
    );
}

/// Matrix row 2 — constraint 1: the marker survives a JSON round-trip.
///
/// REVERT PROBE: change the field's attribute to `#[serde(skip)]` and the key
/// vanishes from the wire, the state decodes `Ok`, and every other row here
/// becomes vacuous. The wire-level assertion catches that at the wire, not just
/// at the API.
#[test]
fn marker_survives_json_round_trip() {
    let projection = projection_for(P0);
    let wire = serde_json::to_value(&projection).expect("a projection serializes");

    assert_eq!(
        wire["viewer_projection"],
        serde_json::json!(P0.0),
        "the marker must appear on the wire, since the confusion it prevents crosses a \
         JSON boundary"
    );

    let decoded = serde_json::from_value::<GameState>(wire)
        .expect("the round-tripped projection decodes on the transport path");
    assert_eq!(
        decoded.viewer_projection,
        Some(P0),
        "the marker that survived serialization is the one the persistence gate reads"
    );
}

/// Matrix row 3 — the persisted ingress
/// (`GameStateDecode::decode_persisted_resolution_state`), both arms.
///
/// REVERT PROBE: delete the guard call in
/// `decode_persisted_resolution_state` ONLY and this row flips to `Ok` while
/// `projection_json_is_refused_as_authority` still passes.
#[test]
fn projection_is_refused_through_persisted_game_state() {
    let projection = projection_for(P0);

    // Both `PersistedGameState` arms: `Raw` decodes through the free
    // `decode_persisted_resolution_state`, `Trusted` through
    // `TrustedGameStateEnvelope::deserialize`. Both reach the same gate.
    let raw = serde_json::to_value(PersistedGameState::Raw(Box::new(projection.clone())))
        .expect("a raw persisted projection serializes");
    let trusted = serde_json::to_value(PersistedGameState::capture(projection))
        .expect("a trusted persisted projection serializes");

    // REACH-GUARD: the two arms really are the two arms — `Trusted` nests the
    // state under a `state` key, `Raw` does not, and that key is exactly what
    // `PersistedGameState::deserialize` branches on.
    assert!(
        raw.get("state").is_none(),
        "test precondition: the Raw arm is not a trusted envelope"
    );
    assert!(
        trusted.get("state").is_some(),
        "test precondition: the Trusted arm is a trusted envelope"
    );
    assert_eq!(
        trusted["state"]["viewer_projection"],
        serde_json::json!(P0.0),
        "test precondition: the marker rides inside the trusted envelope too"
    );

    for (arm, persisted) in [("Raw", raw), ("Trusted", trusted)] {
        let error = serde_json::from_value::<PersistedGameState>(persisted)
            .expect_err("a viewer projection must not restore through persistence");
        assert_is_the_projection_refusal(
            &error.to_string(),
            INGRESS_B_PREDECESSORS,
            &format!("the persisted ingress ({arm})"),
        );
    }
}

/// Matrix row 4 — constraint 3: a genuine carrier-less authority is unaffected.
///
/// This is the fixture that makes the gate's key load-bearing. It is the SAME
/// prompt as row 1's, with the SAME carrier absent — differing only in that no
/// filter produced it, so it carries no marker. A gate keyed on `waiting_for`
/// shape instead of the marker turns this red.
#[test]
fn carrier_less_authoritative_state_still_restores() {
    let state = pending_consent_without_its_run();
    assert!(
        state.viewer_projection.is_none(),
        "test precondition: a reducer-built authority carries no marker"
    );
    assert!(
        state.resolve_all_consent_run.is_none(),
        "test precondition: this authority is genuinely carrier-less, like a projection"
    );

    // Through the PERSISTENCE ingress, both arms — this is the path the gate is on,
    // so this is the path a positive control has to cover.
    for restored in persisted_restore_both_arms(state) {
        assert!(
            restored.viewer_projection.is_none(),
            "an authoritative state restores as authoritative"
        );
        assert!(
            restored.resolve_all_consent_run.is_none(),
            "the carrier-less shape survives the restore unchanged"
        );
    }
}

/// Matrix row 5 — constraint 2: authoritative payloads stay byte-identical.
///
/// Serializing the same state before and after the change is impossible in one
/// build, so the negative is asserted directly: `skip_serializing_if` must keep
/// the key ABSENT from an authoritative wire while it is PRESENT on a
/// projection's.
#[test]
fn authoritative_state_wire_omits_the_key() {
    let authoritative = pending_consent_with_live_run();
    let wire = serde_json::to_value(&authoritative).expect("an authoritative state serializes");
    assert!(
        wire.get("viewer_projection").is_none(),
        "an authoritative payload must stay literally byte-identical to what it was, so \
         the key may not appear even as null"
    );

    let projection = filter_state_for_viewer(&authoritative, P0);
    let projected_wire = serde_json::to_value(&projection).expect("a projection serializes");
    assert!(
        projected_wire.get("viewer_projection").is_some(),
        "the pair discriminates: the key is absent on an authority and present on a projection"
    );
}

/// Matrix row 6 — identity binding, multi-authority.
///
/// REVERT PROBE: stamp a constant `PlayerId(0)` instead of `viewer` and the P1
/// half fails. A gate keyed on a bare `bool` would pass a weaker version of
/// this test; asserting the seat proves the value is bound to the projection
/// that created it rather than being a constant.
///
/// The marker is read in-process and off the wire, NOT from the error text: the
/// refusal string is player-visible copy and deliberately names no seat, and it
/// cannot be read from a decoded state because decoding is exactly what the
/// gate refuses.
#[test]
fn projection_binds_its_own_viewer() {
    let authoritative = pending_consent_with_live_run();

    let for_p0 = filter_state_for_viewer(&authoritative, P0);
    let for_p1 = filter_state_for_viewer(&authoritative, P1);

    assert_eq!(for_p0.viewer_projection, Some(P0));
    assert_eq!(for_p1.viewer_projection, Some(P1));
    assert_ne!(
        for_p0.viewer_projection, for_p1.viewer_projection,
        "two projections of one authority carry two different viewers"
    );

    assert_eq!(
        serde_json::to_value(&for_p0).expect("serialize")["viewer_projection"],
        serde_json::json!(P0.0),
        "the binding survives to the wire, which is where the gate reads it"
    );
    assert_eq!(
        serde_json::to_value(&for_p1).expect("serialize")["viewer_projection"],
        serde_json::json!(P1.0),
    );

    // Binding and refusal are separate claims, asserted separately. Refusal is a
    // PERSISTENCE property, so it is asserted through that ingress.
    for (viewer, projection) in [(P0, for_p0), (P1, for_p1)] {
        assert_eq!(
            persisted_decode_error(projection),
            REFUSAL,
            "each viewer's projection is refused when restored: {viewer:?}"
        );
    }
}

/// Matrix row 7 — re-projection latches last-writer.
///
/// REVERT PROBE: move the stamp behind an `is_none()` guard and this goes red.
/// The latch semantics are pinned here rather than left incidental.
#[test]
fn reprojection_relatches_viewer() {
    let authoritative = pending_consent_with_live_run();
    let for_p0 = filter_state_for_viewer(&authoritative, P0);
    assert_eq!(
        for_p0.viewer_projection,
        Some(P0),
        "test precondition: the input is already a P0 projection"
    );

    let reprojected = filter_state_for_viewer(&for_p0, P1);
    assert_eq!(
        reprojected.viewer_projection,
        Some(P1),
        "last-writer-wins: re-projecting re-latches to the new viewer"
    );

    // The stamp is a plain overwrite, never an accumulator. `derived_views`'
    // "filtering must be idempotent" assertion compares two projections through
    // `impl PartialEq for GameState`, which now includes this field.
    let same_viewer_twice = filter_state_for_viewer(&for_p0, P0);
    assert_eq!(
        same_viewer_twice.viewer_projection,
        Some(P0),
        "re-projecting for the same viewer is idempotent, not accumulating"
    );
}

/// Matrix row 8 — the negative sibling, and the hostile row.
///
/// REVERT PROBE: key the gate on the prompt instead of the marker and this goes
/// red. An authoritative state at `ResolveAllConsent` with a POPULATED,
/// reducer-built run must decode `Ok` — this is the row that proves the gate
/// discriminates a projection from the prompt a projection happens to carry.
#[test]
fn authoritative_consent_with_live_run_still_decodes() {
    let state = pending_consent_with_live_run();
    assert!(
        state.resolve_all_consent_run.is_some(),
        "test precondition: the run is LIVE, which is what distinguishes this from row 4"
    );

    let json = serde_json::to_string(&state).expect("an authoritative state serializes");
    let restored = serde_json::from_str::<GameState>(&json)
        .expect("an authoritative consent state must decode regardless of its prompt");
    assert!(
        restored.resolve_all_consent_run.is_some(),
        "the private run survives an authoritative round-trip"
    );
    assert!(restored.viewer_projection.is_none());
}

/// Matrix row 9 — legacy saves still load. MANDATORY backward-compatibility
/// guard.
///
/// REVERT PROBE: drop `#[serde(default)]` and every pre-existing save fails to
/// decode with a missing-field error.
#[test]
fn absent_marker_key_decodes_as_authoritative() {
    let state = pending_consent_with_live_run();
    let mut wire = serde_json::to_value(&state).expect("serialize");
    assert!(
        wire.as_object_mut()
            .expect("a GameState serializes as an object")
            .remove("viewer_projection")
            .is_none(),
        "test precondition: an authoritative wire has no key to remove — this IS the \
         pre-change save shape, byte for byte"
    );

    // Restore the key-less payload through PERSISTENCE, which is where a legacy save
    // actually arrives. `Raw` is the legacy on-disk shape, so it is asserted directly
    // on the mutated wire rather than via a re-serialized value.
    let legacy_raw = serde_json::from_value::<PersistedGameState>(wire)
        .expect("a pre-change save must still load through persistence");
    let restored = legacy_raw
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect("legacy prepared restore")
        .finalize_immediately()
        .expect("legacy finalized restore");
    assert_eq!(
        restored.viewer_projection, None,
        "an absent key means authoritative, which is correct: it IS authoritative"
    );
}

/// Hostile fixture — the spectator sentinel is not an escape hatch.
///
/// `phase-server` broadcasts spectator views projected for
/// `PlayerId(u8::MAX)`, a non-seat id that identifies no player. That
/// projection has had exactly the same carriers blanked, so it is refused
/// identically.
#[test]
fn spectator_sentinel_projection_is_refused_identically() {
    assert_eq!(
        persisted_decode_error(projection_for(SPECTATOR)),
        REFUSAL,
        "the sentinel must not be an accidental bypass"
    );
}

/// Hostile fixture — the empty path: a fresh authority with no prompt and no
/// carriers at all decodes `Ok`.
///
/// This is row 4's sibling at the opposite extreme: row 4 covers "the prompt
/// stands but the carrier is gone"; this covers "neither exists".
#[test]
fn fresh_authoritative_state_decodes() {
    let state = GameState::new_two_player(42);
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert!(state.viewer_projection.is_none());

    let json = serde_json::to_string(&state).expect("serialize");
    let restored = serde_json::from_str::<GameState>(&json).expect("a fresh authority must decode");
    assert!(restored.viewer_projection.is_none());
}
