//! A reporter's 2-player Commander save (Ureni, turn 10) could not be restored:
//! the file was refused outright with `v1 resolution state must not contain
//! resolution_stack`.
//!
//! # Root cause
//!
//! A raw-persisted `GameState` carries no resolution-wire discriminator.
//! `GameStateDecodeMode::PersistedRaw` used to stamp `resolution_state_version =
//! 1` whenever the field was absent, and the v1 branch then rejects any payload
//! containing `resolution_stack` — so every unversioned save whose typed frame
//! stack was live got laundered into a wire version that structurally forbids
//! the field it carries. `declare_raw_resolution_wire`
//! (`crates/engine/src/types/resolution.rs`) now infers the shape from the field
//! names actually present instead: a payload carrying `resolution_stack` is
//! necessarily post-#6269 and is declared v2, with the frames moved to
//! `resolution_frames`; a payload without it keeps the legacy v1 treatment.
//!
//! # Fixture provenance
//!
//! The reporter's client debug export, trimmed to the one key the engine reads.
//! `phase_ai::saved_state::Saved` declares only `gameState`, so the wrapper's
//! `turnCheckpoints` (83% of the file) is never read.
//!
//! | artifact | bytes |
//! |---|---|
//! | supplied capture `capture-turn10.json.gz` | 2 800 300 |
//! | member `.gameState` (raw) | 2 466 820 |
//! | derived `ureni_turn10_raw_resolution_stack.json.gz` | 469 616 |
//!
//! Byte-reproducible regeneration — `-n` is load-bearing, since without it gzip
//! stamps an mtime and the byte count never reproduces:
//!
//! ```text
//! gzip -dc capture-turn10.json.gz | jq -c '.gameState' \
//!   > /tmp/ureni_turn10_raw_resolution_stack.json
//! gzip -9 -n -c /tmp/ureni_turn10_raw_resolution_stack.json \
//!   > crates/engine/tests/integration/fixtures/ureni_turn10_raw_resolution_stack.json.gz
//! ```
//!
//! # Why the unmodified capture is still refused, and what that is NOT evidence of
//!
//! This capture is a CLIENT DEBUG EXPORT, so it crossed the WASM boundary
//! through `client_state_wire_value` (`crates/engine/src/game/derived_views.rs`),
//! which removes nine top-level carriers plus a recursive firing sweep. One of
//! the nine is `resolving_trigger_firing` — the exact carrier the restore path
//! requires for the capture's triggered `resolving_stack_entry`. Its absence
//! from these bytes is therefore explained by TRANSPORT, and transport being a
//! sufficient explanation is not an exclusive one: whether the runtime ALSO held
//! no carrier is not measurable from this file. This module asserts nothing in
//! either direction about it.
//!
//! The wire's removal of `resolving_trigger_firing` is a transport/persistence
//! contract mismatch, and repairing it is deliberately out of scope here.
//!
//! # The two blockers, and which one the deferral marker below marks
//!
//! Two independent problems block it:
//!
//! 1. the client-wire redaction above, which no export taken at that moment
//!    could have avoided. It is now refused EXPLICITLY AND BY NAME at the
//!    ingress — `declare_raw_resolution_wire` reads the absence of the three
//!    unconditionally-serialized fields `client_state_wire_value` removes as a
//!    redaction fingerprint and returns a player-facing refusal — rather than
//!    incidentally, via the downstream `resolving triggered entry has no firing
//!    carrier` error that a redacted payload used to reach;
//! 2. a genuine runtime residue — a finished spell's `SpellResolution` frame and
//!    its `resolving_stack_entry` both stranded at a `Priority` rest with an
//!    empty game stack. Both are retained verbatim by the wire, so this one IS
//!    evidenced by the capture.
//!
//! `raw_capture_fails_closed_on_the_stranded_runtime_residue` is the deferral
//! marker for **blocker 2 only**. It is NOT a passing restore, and it does not
//! mark blocker 1 — the test repairs that one test-locally first.
//!
//! Said plainly, because it is the honest worth of the ingress guard: **for this
//! capture the change buys a clearer refusal, not a restore.** Nothing that
//! failed before now succeeds. What would restore a real capture is a write-time
//! version stamp at `client_state_wire_value` plus a transport/persistence
//! repair for the carrier; both are deferred and neither is foreclosed here.
//!
//! What can move that row is narrower than "the residue is fixed". It reads a
//! FROZEN fixture and asserts on `prepare_for_restore`, whose verdict is a pure
//! function of the decoded state, so fixing the runtime writer that strands the
//! residue will NOT move it — neither the fixture bytes nor the restore-boundary
//! code changes. It goes red if and when a restore-boundary repair is extended
//! to cover a triggered carrier at a `Priority` rest.

use engine::types::game_state::{
    GameState, PersistedGameState, PersistedRestoreError, PersistedRestoreFinalization,
    StackEntryKind,
};
use engine::types::identifiers::ObjectId;

fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

const CAPTURE: &[u8] = include_bytes!("fixtures/ureni_turn10_raw_resolution_stack.json.gz");

const REGENERATE: &str = "regenerate with: gzip -dc capture-turn10.json.gz | jq -c '.gameState' \
     | gzip -9 -n -c > crates/engine/tests/integration/fixtures/ureni_turn10_raw_resolution_stack.json.gz";

/// Every legacy resolution-wire field name a v2 payload must not carry.
/// Mirrors `legacy_resolution_wire_fields()` in
/// `crates/engine/src/types/resolution.rs`, which is private to that module and
/// so cannot be called from an integration test.
const LEGACY_RESOLUTION_WIRE_FIELDS: &[&str] = &[
    "pending_continuation",
    "pending_repeat_iteration",
    "pending_repeat_until",
    "pending_repeated_optional_payment",
    "optional_cost_payments_this_resolution",
    "pending_change_zone_iteration",
    "devour_eligible_snapshot",
    "pending_batch_deliveries",
    "pending_mill_deliveries",
    "pending_counter_moves",
    "pending_counter_removals",
    "pending_counter_additions",
    "pending_copy_token_resolution",
    "pending_each_player_copy_chosen",
    "pending_choose_one_of",
    "pending_vote_ballot_iteration",
    "pending_per_player_zone_choice",
    "pending_per_category_zone_choice",
    "pending_choose_zone_trigger_context",
    "pending_optional_effect",
    "pending_optional_trigger_event",
    "pending_optional_trigger_match_count",
    "pending_coin_flip",
    "pending_proliferate_actions",
    "draw_sequences",
    "pending_multi_draw",
    "pending_connive_reentry",
    "pending_life_total_assignment",
    "pending_spell_resolution",
    "pending_mutate_merge",
    "post_replacement_drains",
    "post_replacement_effect",
    "post_replacement_resolved_effect",
    "post_replacement_continuation",
    "post_replacement_source",
    "post_replacement_applied",
    "post_replacement_event_source",
    "post_replacement_event_target",
];

/// The `GameState` fields that are serialized UNCONDITIONALLY and that
/// `client_state_wire_value` removes anyway. Mirrors
/// `CLIENT_WIRE_UNCONDITIONAL_FIELDS` in
/// `crates/engine/src/types/resolution.rs`, which is private to that module and
/// so cannot be read from an integration test.
const CLIENT_WIRE_UNCONDITIONAL_FIELDS: &[&str] = &[
    "next_delayed_trigger_token",
    "next_delayed_trigger_instance",
    "resolved_rules_journal",
];

/// The distinctive phrase of the client-wire projection refusal raised by
/// `declare_raw_resolution_wire`. Its FIRST clause: the part of that sentence
/// which is a statement about the file, and so is true of both populations the
/// guard refuses — a client-wire projection and a genuine 2026-07-21..22
/// build-window save. The trailing clause is asserted on in exactly ONE row,
/// `raw_capture_is_refused_as_a_client_wire_projection`, and deliberately
/// nowhere else — see that row's doc for why one row is both necessary and
/// sufficient.
const PROJECTION_REFUSAL: &str =
    "missing the private rules record for the resolution it was paused in";

fn capture_value() -> serde_json::Value {
    serde_json::from_str(&gunzip(CAPTURE)).expect("the capture parses as JSON")
}

/// Re-inserts the three unconditionally-serialized fields
/// `client_state_wire_value` removes, at the values a state that never held any
/// of them would decode to.
///
/// Their values are read from a freshly constructed `GameState`'s own
/// serialization rather than hard-coded, so the helper cannot drift from the
/// serde defaults. The only one that differs from its literal serde default is
/// `next_delayed_trigger_token` (a fresh state holds 1; the serde default is 0),
/// and `normalize_delayed_trigger_allocators` collapses both to 1 for a payload
/// with no install roots — which this capture is, since the same redaction
/// stripped the journal AND every `delayed_triggers` entry's `provenance`. So
/// the decoded state is identical to what the unrepaired capture decoded to
/// before the ingress guard existed, and the rows below keep measuring exactly
/// what they measured.
fn capture_with_fingerprint_restored() -> serde_json::Value {
    let fresh = serde_json::to_value(GameState::new_two_player(42))
        .expect("a freshly constructed GameState serializes");
    let mut value = capture_value();
    let object = value.as_object_mut().expect("the capture is a JSON object");
    for field in CLIENT_WIRE_UNCONDITIONAL_FIELDS {
        let default = fresh
            .get(*field)
            .unwrap_or_else(|| {
                panic!("the bare writer always emits {field}; it carries no skip_serializing_if")
            })
            .clone();
        object.insert((*field).to_string(), default);
    }
    value
}

/// `capture_with_fingerprint_restored()` plus the ONE carrier the rows below
/// already restored test-locally. It restores everything the client wire removed
/// that these rows need, rather than the one key that used to be enough.
///
/// `Ordinary` is what a coherent runtime writes for this entry — CR 603.7a: a
/// delayed triggered ability is one an effect CREATES, and Ureni's attack trigger
/// is printed on the card, so it is an ordinary triggered ability rather than a
/// delayed one. This repair is test-local and is never a production
/// normalization.
fn repaired_capture() -> serde_json::Value {
    let mut value = capture_with_fingerprint_restored();
    value
        .as_object_mut()
        .expect("the capture is a JSON object")
        .insert(
            "resolving_trigger_firing".to_string(),
            serde_json::Value::String("Ordinary".to_string()),
        );
    value
}

/// V10 non-vacuity, asserted BEFORE any decode. Without this arm a green
/// anywhere below could silently mean "the fixture lost the failing shape"
/// rather than "the seam behaves". It runs ahead of the code under test, so no
/// path under test can satisfy it vacuously.
#[test]
fn raw_capture_still_carries_the_failing_wire_shape() {
    let value = capture_value();
    let object = value.as_object().expect("the capture is a JSON object");

    assert!(
        !object.contains_key("resolution_state_version"),
        "the capture must stay UNVERSIONED — that is the whole premise of the \
         inference under test; {REGENERATE}"
    );
    assert!(
        !object.contains_key("resolution_frames"),
        "the capture must not carry the v2 frame carrier; {REGENERATE}"
    );
    assert!(
        object.contains_key("resolution_stack"),
        "the capture must carry a live runtime resolution_stack; {REGENERATE}"
    );

    assert_eq!(
        value["resolution_stack"]["frames"][0]["type"], "SpellResolution",
        "the capture parks a spell resolution; {REGENERATE}"
    );
    assert_eq!(
        value["resolution_stack"]["frames"][0]["data"]["object_id"], 198,
        "the parked frame belongs to object 198; {REGENERATE}"
    );
    assert_eq!(
        value["waiting_for"]["type"], "Priority",
        "the capture rests at a priority window; {REGENERATE}"
    );
    assert_eq!(
        value["waiting_for"]["data"]["player"], 1,
        "the capture rests on player 1's priority; {REGENERATE}"
    );
    assert_eq!(
        value["stack"].as_array().map(Vec::len),
        Some(0),
        "the game stack is empty, which is what makes the parked frame residue; \
         {REGENERATE}"
    );
    assert_eq!(
        value["resolving_stack_entry"]["id"], 209,
        "the stranded resolving entry is object 209; {REGENERATE}"
    );
    assert_eq!(
        value["resolving_stack_entry"]["kind"]["type"], "TriggeredAbility",
        "the stranded resolving entry is a triggered ability; {REGENERATE}"
    );
    assert!(
        !object.contains_key("resolving_trigger_firing"),
        "the client wire strips resolving_trigger_firing unconditionally, and its \
         absence is what the repaired sibling below restores test-locally; \
         {REGENERATE}"
    );

    for field in LEGACY_RESOLUTION_WIRE_FIELDS {
        assert!(
            !object.contains_key(*field),
            "the capture must carry no legacy resolution-wire field, found {field}; \
             {REGENERATE}"
        );
    }
}

/// V3a. On the real capture the v1 shape error is gone — the inference ran and
/// classified the payload by the field names it actually carries.
///
/// Red before this change on the first assertion. The second is the positive
/// reach-guard: for this payload that carrier error is reachable ONLY through
/// the v2 classification, so it proves the inference ran rather than the decode
/// simply failing somewhere earlier.
///
/// The fixture now restores the redaction FINGERPRINT (and only the
/// fingerprint — `resolving_trigger_firing` stays absent), so the payload passes
/// the new ingress projection guard and still reaches the v2 classification.
/// Both assertions are unchanged, and both remain reachable only through that
/// classification. Restoring the fingerprint rather than rewriting the second
/// assertion to expect the projection refusal is deliberate: under the mutation
/// this row exists to catch — stamping `LEGACY_RESOLUTION_STATE_WIRE_VERSION` in
/// the `Some(frames)` arm of `declare_raw_resolution_wire` — a
/// projection-refusal assertion would stay GREEN, because the guard returns
/// before the classification runs at all. With the fingerprint restored that
/// same mutation reddens the FIRST assertion with `v1 resolution state must not
/// contain resolution_frames`. (Hedge: several migrations run ahead of the
/// `resolution_frames` check under that mutation; if one of them errors first
/// the row still reddens, but at the second assertion instead. Either way it
/// sees the regression, which is the property that matters.)
#[test]
fn raw_capture_is_no_longer_refused_as_a_v1_wire() {
    let error = serde_json::from_value::<PersistedGameState>(capture_with_fingerprint_restored())
        .expect_err("the capture is still blocked by the redacted carrier");
    let error = error.to_string();

    assert!(
        !error.contains("v1 resolution state"),
        "the raw ingress must no longer launder an unversioned payload into v1, got {error}"
    );
    assert!(
        error.contains("resolving triggered entry has no firing carrier"),
        "the refusal must now be the accurate carrier error, got {error}"
    );
}

/// V3b. The real-data half of the preservation claim: once the one carrier the
/// client wire removes unconditionally is restored, the parked frame survives
/// the inference intact.
#[test]
fn raw_capture_restores_its_parked_frame_once_the_redacted_carrier_is_replaced() {
    // The test-local repair models "undo what the client wire removed". It was
    // always that model and was merely incomplete: it restored the one carrier
    // this row needs, while the wire also removes the three unconditionally
    // serialized fields the ingress now reads as a redaction fingerprint.
    // `repaired_capture()` widens it to both, which makes the row MORE faithful
    // and leaves the decoded state unchanged (see the helper's doc).
    let decoded = serde_json::from_value::<PersistedGameState>(repaired_capture())
        .expect("the repaired capture decodes through the inferred v2 wire");
    let PersistedGameState::Raw(state) = decoded else {
        panic!("a bare gameState object decodes through the raw ingress");
    };

    assert_eq!(
        state.resolution_stack.len(),
        1,
        "the parked frame must survive the persistence boundary"
    );
    assert_eq!(
        state.active_spell_resolution().map(|p| p.object_id),
        Some(ObjectId(198)),
        "the frame must be PRESERVED, not merely tolerated"
    );

    // The §4 mechanism, documented in code: the stranded entry is a TRIGGERED
    // ability, which is exactly what puts this shape outside
    // `is_orphaned_spell_resolution_at_priority_boundary`'s `StackEntryKind::Spell`
    // clause and so outside the restore boundary's existing repair.
    let entry = state
        .resolving_stack_entry
        .as_ref()
        .expect("the capture strands a resolving entry");
    assert!(
        matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }),
        "the stranded entry is a triggered ability, which no existing terminal-rest \
         repair consumes"
    );
}

/// V3c. Fail-closed deferral marker for BLOCKER 2 ONLY — the genuine runtime
/// residue. It does NOT mark the redacted carrier, which the payload built here
/// has already repaired test-locally.
///
/// CR 608.1: a spell or ability resolves once all players have passed in
/// succession, and CR 117.3b hands the active player priority again AFTER that
/// resolution — the rules place a priority window on either side of a
/// resolution, not inside one. CR 608.2g closes the one opening: where an
/// effect lets a player cast a spell DURING a resolution, no player receives
/// priority after it is cast. (CR 117.3a grants the active player priority at
/// the beginning of most steps and phases as well, so CR 117.3b is not the only
/// grant. This argument does not need it to be.) So a `Priority` rest with an
/// empty game stack has, by the rules, no live resolution owner — the parked
/// `SpellResolution` frame and the stranded `resolving_stack_entry` are runtime
/// residue, and the restore boundary refuses rather than silently normalizing
/// them into a playable board.
///
/// What can move this row is narrower than "the residue is fixed": the fixture
/// is FROZEN and `prepare_for_restore`'s verdict is a pure function of the
/// decoded state, so fixing the runtime writer that strands the residue will NOT
/// move it. It goes red if and when a restore-boundary repair is extended to
/// cover a triggered carrier at a `Priority` rest.
#[test]
fn raw_capture_fails_closed_on_the_stranded_runtime_residue() {
    let error = serde_json::from_value::<PersistedGameState>(repaired_capture())
        .expect("the repaired capture decodes")
        .prepare_for_restore(PersistedRestoreFinalization::Immediate)
        .expect_err("the stranded resolution residue must not restore silently");

    assert!(
        matches!(error, PersistedRestoreError::UnsettledPriorityResolution),
        "the restore boundary must refuse the unsettled priority resolution, got {error:?}"
    );
}

/// V13. Fail-open guard at the raw ingress: an UNVERSIONED payload declaring
/// BOTH carriers is refused, not silently resolved.
///
/// Inference alone would move `resolution_stack` onto the `resolution_frames`
/// key and DISCARD the payload's own `resolution_frames` with no error, then
/// decode. That is a fail-open at an ingress which takes user-uploaded save
/// files, so `declare_raw_resolution_wire` refuses the ambiguity instead.
///
/// The first assertion is the paired positive reach-guard: the same payload
/// WITHOUT the conflicting carrier decodes. Without it, a green here could mean
/// the payload failed for an unrelated reason — the redacted trigger carrier, a
/// legacy field, a malformed frame — rather than because of the conflict.
///
/// Mutation that makes this row red: delete the both-present guard in
/// `declare_raw_resolution_wire` (`crates/engine/src/types/resolution.rs`). The
/// conflicting payload then decodes and the `expect_err` panics.
///
/// Its payloads are built by `repaired_capture()` rather than by restoring the
/// one carrier alone. That widening is REQUIRED, not cosmetic: the narrower
/// repair leaves the three fingerprint fields absent, so the new ingress
/// projection guard refuses the reach-guard payload and its `.expect(…)` panics.
/// The assertions are unchanged.
#[test]
fn raw_ingress_refuses_an_unversioned_payload_declaring_both_carriers() {
    // REACH-GUARD, asserted FIRST: with only one carrier declared this exact
    // payload decodes, so the refusal below is attributable to the conflict.
    let reached = serde_json::from_value::<PersistedGameState>(repaired_capture())
        .expect("the payload decodes when resolution_stack is the only declared carrier");
    assert!(
        matches!(reached, PersistedGameState::Raw(_)),
        "the reach-guard must decode through the raw ingress, which is the seam under test"
    );

    let mut conflicting = repaired_capture();
    conflicting
        .as_object_mut()
        .expect("the capture is a JSON object")
        .insert(
            "resolution_frames".to_string(),
            serde_json::json!({ "frames": [] }),
        );

    let error = serde_json::from_value::<PersistedGameState>(conflicting)
        .expect_err("an unversioned payload declaring two carriers must not decode")
        .to_string();

    assert!(
        error.contains(
            "unversioned raw resolution state must not contain both resolution_stack and \
             resolution_frames"
        ),
        "the refusal must name the two-carrier conflict rather than any later shape error, \
         got {error}"
    );
    assert!(
        !error.contains("v1 resolution state"),
        "the conflict must be refused on its own terms, not laundered into v1, got {error}"
    );
}

/// V20. The reporter's real bytes are refused as a CLIENT-WIRE PROJECTION, by
/// name, at the unversioned raw ingress.
///
/// The whole first half runs BEFORE any decode, so no path under test can
/// satisfy it vacuously: the capture still carries `resolution_stack`, still
/// declares no version, still carries the three unconditionally-serialized
/// siblings the redactor does NOT remove, and still lacks all three that it
/// does. That last group is the fingerprint the guard reads.
///
/// Mutation — delete the fingerprint guard in `declare_raw_resolution_wire`.
/// These bytes then fall through to the v2 classification and are refused there
/// instead, at `resolving triggered entry has no firing carrier`. The payload is
/// STILL REFUSED, so the `expect_err` still passes and this row reddens at the
/// MESSAGE assertion.
///
/// That is the opposite of how the same mutation reddens the two
/// `raw_ingress_refuses_a_redacted_*` unit rows, which redden at their
/// `expect_err` because their payloads DECODE once the guard is gone. Predicting
/// one row's reddening assertion from another's is the error this note exists to
/// prevent. The string above is measured against SHIPPED behaviour: it is
/// exactly what `raw_capture_is_no_longer_refused_as_a_v1_wire` asserts today on
/// these same bytes.
///
/// SECOND mutation, and the reason this row carries an assertion no other row
/// does — revert the refusal's WORDING to its pre-hedge form. The payload stays
/// refused (`expect_err` passes) and `PROJECTION_REFUSAL` stays present, because
/// that constant is a substring of the old sentence. So the first message
/// assertion ALSO passes, and this row reddens at the SECOND one, on the hedge
/// clause. The two message assertions therefore split the two mutations cleanly:
/// guard deletion reddens the first, a wording revert reddens the second, and
/// neither pre-empts the other.
#[test]
fn raw_capture_is_refused_as_a_client_wire_projection() {
    let value = capture_value();
    let object = value.as_object().expect("the capture is a JSON object");

    for sibling in [
        "next_object_id",
        "next_pip_id",
        "next_logical_zone_change_group_id",
    ] {
        assert!(
            object.contains_key(sibling),
            "the capture must still carry {sibling} — an unconditionally serialized \
             field the client redactor does NOT remove, which is what discriminates \
             a redaction from a truncated file; {REGENERATE}"
        );
    }
    assert!(
        object.contains_key("resolution_stack"),
        "the capture must carry a live runtime resolution_stack — the guard's first \
         conjunct; {REGENERATE}"
    );
    assert!(
        !object.contains_key("resolution_state_version"),
        "the capture must stay UNVERSIONED, or it takes the ingress's early return \
         and never reaches the guard; {REGENERATE}"
    );
    for field in CLIENT_WIRE_UNCONDITIONAL_FIELDS {
        assert!(
            !object.contains_key(*field),
            "the capture must still lack {field} — the three together are the \
             client-wire redaction fingerprint; {REGENERATE}"
        );
    }

    let error = serde_json::from_str::<PersistedGameState>(&gunzip(CAPTURE))
        .expect_err("a client debug export must not decode at the unversioned raw ingress")
        .to_string();
    assert!(
        error.contains(PROJECTION_REFUSAL),
        "the refusal must name the projection rather than the downstream carrier \
         error, got {error}"
    );

    // V22. The ONLY assertion in the workspace that reddens if the refusal's
    // wording is reverted to its pre-hedge form, and the reason it is written
    // out verbatim instead of reusing `PROJECTION_REFUSAL`: that constant is a
    // SUBSTRING of the sentence it replaced ("…so it is missing the private
    // rules record for the resolution it was paused in"), so every `contains`
    // on it stays green under that revert. The clause below is absent from the
    // old sentence and present in the new, so it discriminates in the one
    // direction the other rows cannot.
    //
    // Pinned HERE and nowhere else on purpose. This row is the only one that
    // runs the reporter's real bytes, and pinning refusal prose verbatim across
    // the suite would make every future wording improvement a seven-row edit.
    // One row is sufficient: any revert or reword of the hedge reddens it.
    //
    // The hedge is what makes the sentence true of BOTH populations the guard
    // refuses (see `declare_raw_resolution_wire`): it reports what a debug
    // export looks like instead of asserting this file is one, which a genuine
    // 2026-07-21..22 build-window save is not. Reverting it re-introduces a
    // false statement of fact about that player's file. Do not delete this.
    assert!(
        error.contains("this is what a debug export of the on-screen state looks like"),
        "the refusal must keep its hedge clause — without it the sentence asserts \
         as fact that the file is a debug export, which is false for a genuine \
         2026-07-21..22 build-window save the same guard refuses; got {error}"
    );
}

/// V21. Pins the ORDER of the two refusals in `declare_raw_resolution_wire`: the
/// two-carrier conflict is raised first, so a payload that is both ambiguous AND
/// redacted is named by the ambiguity.
///
/// This is a message-accuracy boundary, not a fail-open one — reversing the two
/// statements produces the OTHER refusal, never an admission. It is pinned here
/// because no other row can pin it: every other payload in this module either
/// bears the fingerprint without a second carrier or carries a second carrier
/// with the fingerprint restored, and only a payload with BOTH can distinguish
/// the two placements.
///
/// The reach-guard is asserted FIRST and is a REFUSAL: these exact bytes,
/// without the conflicting carrier, are refused BY THE PROJECTION GUARD. That is
/// what makes the subject a genuine choice between two reachable refusals rather
/// than a green produced by a guard that could never have fired.
///
/// Mutation — move the fingerprint guard ABOVE the two-carrier refusal. The
/// payload is still refused, so the `expect_err` still passes; this row reddens
/// at the conflict-message assertion, and the string it then produces is the
/// projection refusal.
#[test]
fn two_carrier_conflict_outranks_the_projection_refusal() {
    // REACH-GUARD, asserted FIRST: the projection guard is live for these bytes.
    let projection = serde_json::from_str::<PersistedGameState>(&gunzip(CAPTURE))
        .expect_err("the unmodified capture is refused as a client-wire projection")
        .to_string();
    assert!(
        projection.contains(PROJECTION_REFUSAL),
        "without the conflicting carrier these bytes must be refused as a projection, \
         or this row cannot distinguish the two guard placements; got {projection}"
    );

    let mut conflicting = capture_value();
    conflicting
        .as_object_mut()
        .expect("the capture is a JSON object")
        .insert(
            "resolution_frames".to_string(),
            serde_json::json!({ "frames": [] }),
        );

    let error = serde_json::from_value::<PersistedGameState>(conflicting)
        .expect_err("an unversioned payload declaring two carriers must not decode")
        .to_string();
    assert!(
        error.contains(
            "unversioned raw resolution state must not contain both resolution_stack and \
             resolution_frames"
        ),
        "the two-carrier conflict must outrank the projection refusal, got {error}"
    );
}
