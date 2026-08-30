// engine-citation-gate: symbol anchors only
//! The turn-50 four-player capture in which the AI at seat 2 halts in
//! `WaitingFor::ManaPayment` while holding Vanquish the Horde on the stack.
//!
//! CR 601.2f: the total cost is the mana cost minus all cost reductions, floored
//! at `{0}`. Vanquish the Horde is `{6}{W}{W}` and reads "This spell costs {1}
//! less to cast for each creature on the battlefield"; the capture has 16
//! creatures in play, so the generic component is fully reduced and the locked-in
//! total is `{W}{W}`.
//!
//! CR 601.2g: mana abilities are activated *before* costs are paid. Seat 2 has
//! already done so — every one of its eleven mana sources is tapped and eight
//! pips are floating, of which exactly two are unrestricted White. There is no
//! untapped source left, so `mana_tap_actions` contributes nothing and the only
//! payment route the enumerator offers is the `PassPriority` that
//! `mana_payment_actions` pushes to finalize.
//!
//! CR 601.2h: the player pays the total cost, and unpayable costs can't be paid.
//! Two floating White against a `{W}{W}` obligation *is* payable, so finalizing
//! must be offered to the seat that has to act.
//!
//! What this row discriminates: `AiDecisionContract::issue` runs the candidate
//! list through `FilterPipeline::default_pipeline()` whenever
//! `decision_contract_requires_reducer_validation` holds, and that predicate is
//! true for every `ManaPayment` state. If that validation rejects the finalize
//! action, the contract hands the AI an empty candidate set for a decision it is
//! obliged to make, and the seat can never advance — the reported hang. The
//! assertions below fail in exactly that case and pass only when the reducer
//! agrees the floating pool can settle the locked-in cost.
//!
//! Note the capture carries `convoke_mode: None`, so
//! `structurally_valid_tap_for_convoke_payment` cannot short-circuit the clone
//! here; this row exercises the full-validation path, not the convoke fast path.

use engine::ai_support::{
    candidate_actions_broad, witness_payment_continuation, AiDecisionContract,
};
use engine::game::engine::{apply_as_current, verified_ai_stack_pass_player};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, PersistedGameState, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::player::PlayerId;

/// The stuck seat, per the capture: active player, priority player, and the
/// `ManaPayment` subject are all seat 2.
const STUCK: PlayerId = PlayerId(2);

/// Inflate a gzipped capture fixture to its UTF-8 JSON text.
fn gunzip(gz: &[u8]) -> String {
    use std::io::Read;
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// Load the capture through the real production restore chokepoint
/// `PersistedGameState::into_game_state`, which both the server's
/// `from_persisted` and WASM's `decode_restored_game_state` funnel through.
/// Decoding as `PersistedGameState` (rather than a bare `GameState`) is what
/// runs `reject_legacy_raw_prompt_authority` and
/// `decode_persisted_resolution_state`, so the state under test is the one
/// production would actually reconstruct.
fn load_capture() -> GameState {
    let json = gunzip(include_bytes!(
        "../fixtures/vanquish_the_horde_manapayment_4p.json.gz"
    ));
    let envelope: serde_json::Value =
        serde_json::from_str(&json).expect("capture envelope parses as JSON");
    serde_json::from_value::<PersistedGameState>(envelope["gameState"].clone())
        .expect("the capture's gameState deserializes through the production decoder")
        .into_game_state()
        .expect("the capture satisfies the checked restore contract")
}

/// Non-vacuity guard. Every assertion in this file is meaningless unless the
/// loaded state really is the reported wedge, so pin the shape that makes the
/// finalize action both necessary and sufficient: the `ManaPayment` subject, the
/// locked-in `{W}{W}` cost, two unrestricted floating White, and — the part that
/// makes `PassPriority` the *only* route — zero untapped mana sources.
fn assert_capture_is_the_reported_wedge(state: &GameState) {
    let WaitingFor::ManaPayment {
        player,
        convoke_mode,
    } = &state.waiting_for
    else {
        panic!(
            "capture must be parked in ManaPayment, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, STUCK, "the ManaPayment subject is seat 2");
    assert!(
        convoke_mode.is_none(),
        "this row must exercise the full-validation path, not the convoke fast \
         path in structurally_valid_tap_for_convoke_payment"
    );

    let pending = state
        .pending_cast
        .as_ref()
        .expect("a pending cast is what the seat owes payment for");
    let ManaCost::Cost { shards, generic } = &pending.cost else {
        panic!(
            "locked-in cost must be a concrete Cost, got {:?}",
            pending.cost
        );
    };
    // CR 601.2f: 16 creatures reduce {6} to {0}; the colored pips never reduce.
    assert_eq!(*generic, 0, "the generic component is fully reduced");
    assert_eq!(
        shards.as_slice(),
        &[ManaCostShard::White, ManaCostShard::White],
        "the locked-in remainder is exactly {{W}}{{W}}"
    );

    let stuck_player = state
        .players
        .iter()
        .find(|player| player.id == STUCK)
        .expect("seat 2 is in the capture");
    let white_available = stuck_player
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.color == ManaType::White && unit.restrictions.is_empty())
        .count();
    assert!(
        white_available >= 2,
        "the floating pool must be able to settle {{W}}{{W}} for this row to \
         mean anything; found {white_available} unrestricted White"
    );

    let untapped_sources = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == STUCK && !obj.tapped && !obj.available_mana_pips.is_empty())
        .count();
    assert_eq!(
        untapped_sources, 0,
        "with any untapped source the enumerator would offer a tap action and \
         PassPriority would not be the sole payment route"
    );
}

/// The seat must be offered the finalize action it needs to escape the payment
/// window. `candidate_actions_broad` is the pre-validation enumeration; the
/// contract is what the AI actually consumes.
#[test]
fn ai_contract_offers_finalize_for_a_payable_floating_pool() {
    let state = load_capture();
    assert_capture_is_the_reported_wedge(&state);

    // Establish that the enumerator itself produces the finalize action, so a
    // failure below is attributable to the validation pass and not to a gap in
    // enumeration. `mana_payment_actions` pushes PassPriority unconditionally.
    let enumerated = candidate_actions_broad(&state);
    assert!(
        enumerated
            .iter()
            .any(|c| matches!(c.action, GameAction::PassPriority)),
        "the enumerator must offer PassPriority to finalize payment; without it \
         this row cannot attribute a failure to validation"
    );

    let contract = AiDecisionContract::issue(&state, STUCK);
    assert!(
        !contract.candidates.is_empty(),
        "the AI was handed an empty candidate set for a decision it is obliged \
         to make — the seat can never advance, which is the reported hang"
    );
    // CR 601.2h: two floating White settle a locked-in {W}{W}; the payment is not
    // an unpayable cost, so reducer validation must not drop the finalize action.
    assert!(
        contract
            .candidates
            .iter()
            .any(|c| matches!(c.action, GameAction::PassPriority)),
        "reducer validation dropped PassPriority even though the floating pool \
         covers the locked-in cost; candidates were {:?}",
        contract
            .candidates
            .iter()
            .map(|c| &c.action)
            .collect::<Vec<_>>()
    );
}

/// Being *offered* an action and that action *working* are different claims,
/// and only the first was tested above. The projection layer runs a separate
/// authority — `witness_payment_continuation` — which must prove a candidate
/// actually completes the payment root; when nothing is witnessed,
/// `projection.rs` bails with `NoLegalManaPayment`. So apply the finalize
/// action for real and require the seat to leave the payment window.
///
/// CR 601.2h: two floating White settle a locked-in {W}{W}. If the reducer
/// refuses that, or accepts it without advancing, the defect is engine-side and
/// no amount of AI-layer work can fix it.
#[test]
fn applying_the_offered_finalize_actually_settles_the_payment() {
    let mut state = load_capture();
    assert_capture_is_the_reported_wedge(&state);

    assert!(
        witness_payment_continuation(&state, &GameAction::PassPriority).is_some(),
        "the payment oracle could not witness PassPriority as completing the \
         payment root; projection.rs turns exactly this into \
         BailReason::NoLegalManaPayment"
    );

    apply_as_current(&mut state, GameAction::PassPriority)
        .expect("finalizing a payment the floating pool covers must be accepted");
    assert!(
        !matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
        "the reducer accepted the finalize but left the seat parked at {:?}",
        state.waiting_for
    );
}

/// Direct test of the routing decision, so the predicate has a guard of its own
/// rather than only the end-to-end row that exercises it.
///
/// The two halves are a discriminating pair over ONE state: the same capture,
/// with only `waiting_for` changed. Stack contents, pool, and objects are held
/// identical, so the difference is attributable to the prompt alone — and the
/// `Priority` half doubles as the positive control proving the predicate can
/// return `Some` at all (without it, a `None` below could just mean "this
/// function never matches anything").
#[test]
fn a_payment_finalize_is_not_classified_as_a_stack_continuation_pass() {
    let mut state = load_capture();
    assert_capture_is_the_reported_wedge(&state);
    assert!(
        !state.stack.is_empty(),
        "CR 601.2a: the announced spell is on the stack, which is exactly why \
         the old stack-only predicate could not discriminate here"
    );

    // CR 601.2h: at this prompt `PassPriority` means "pay the total cost", so it
    // must go to the ordinary boundary and reach `finalize_mana_payment`.
    assert_eq!(
        verified_ai_stack_pass_player(&state, &GameAction::PassPriority),
        None,
        "a ManaPayment finalize must not be routed to the priority-pass boundary"
    );

    // Positive control: flip ONLY the prompt on this same state.
    state.waiting_for = WaitingFor::Priority { player: STUCK };
    assert_eq!(
        verified_ai_stack_pass_player(&state, &GameAction::PassPriority),
        Some(STUCK),
        "at a live priority window over a nonempty stack this IS a stack \
         continuation pass; without this the None above would be vacuous"
    );

    // A non-pass action is never this boundary's business, at either prompt.
    assert_eq!(
        verified_ai_stack_pass_player(&state, &GameAction::CancelCast),
        None,
        "only PassPriority can be a stack continuation pass"
    );
}
