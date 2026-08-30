//! Two reported hangs (#8047) in which an AI seat parks in
//! `WaitingFor::ManaPayment` holding a spell it can already afford, and the
//! game stops advancing. Reported symptom: the "Engine Connection Lost" modal,
//! which `aiController.ts` raises by calling `notifyEngineLost` with
//! `"ai-controller-stuck:"` joined to `waitingFor.type`, once the AI has failed
//! `MAX_TOTAL_FAILURES` times on the same prompt.
//!
//! ROOT CAUSE (both AI dispatch paths: `auto_play::run_ai_actions_with_limit`
//! and engine-wasm's `submit_ai_action_proposal`). Their shared routing
//! predicate used to read:
//!
//! ```ignore
//! let is_stack_recheck_pass =
//!     matches!(&action, GameAction::PassPriority) && !state.stack.is_empty();
//! ```
//!
//! CR 601.2a: announcing a spell moves it onto the stack, so `!stack.is_empty()`
//! is true throughout every cast and cannot discriminate anything on its own.
//! (Most casts never surface this prompt at all — `auto_tap_mana_sources_*`
//! settles them — which is why AI play mostly worked; the wedge needs a seat
//! that reaches an explicit `ManaPayment` with its pool already covering the
//! cost and nothing left to tap.)
//! CR 601.2h: at a payment prompt `PassPriority` means "pay the total cost" —
//! the reducer routes it to `casting_costs::finalize_mana_payment` — whereas at
//! `WaitingFor::Priority` the same variant means an ordinary priority pass.
//! Testing only the action variant plus the stack conflated the two, sending
//! every AI payment finalize into `apply_verified_ai_priority_pass`, whose first
//! guard demands `WaitingFor::Priority` and rejected it outright. The action
//! never reached the boundary that would have settled the payment.
//!
//! The classification now lives in one place — `verified_ai_stack_pass_player`
//! in the engine — which both routers and that callee share, so they cannot
//! drift apart again.
//!
//! Only AI seats were affected: a human's `PassPriority` never crosses that
//! AI-only router.
//!
//! The engine layer is NOT at fault, and `vanquish_horde_ai_manapayment_wedge`
//! (engine integration) pins that: `AiDecisionContract::issue` returns a
//! non-empty domain containing `PassPriority`, so reducer validation is not
//! eating the finalize action.
//!
//! The two rows below split proposal from application, which is what localized
//! the defect: the AI *proposes* correctly and the *submission* was refused.

use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType};
use engine::types::player::PlayerId;
use phase_ai::auto_play::run_ai_actions_bounded;
use phase_ai::choose_action;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use phase_ai::session::AiSession;
use rand::rngs::SmallRng;
use rand::SeedableRng;
use std::collections::{HashMap, HashSet};
use std::io::Read;

/// The stuck seat in the turn-4 capture: active player, priority holder, and
/// `ManaPayment` subject.
const STUCK: PlayerId = PlayerId(1);

/// Inflate a gzipped capture fixture to its UTF-8 JSON text.
fn gunzip_fixture(gz: &[u8]) -> String {
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

/// The turn-4 Galvanic Blast capture. Preferred over the turn-50 Vanquish the
/// Horde capture for this row because Galvanic Blast carries no cost-reduction
/// static: `base_cost` and the locked-in `cost` are both `{R}`, so a failure
/// cannot be blamed on CR 601.2f reduction arithmetic. It is also ~73 KB rather
/// than ~800 KB.
fn load_turn4_capture() -> GameState {
    let raw = gunzip_fixture(include_bytes!(
        "../fixtures/scenarios/galvanic-blast-manapayment-turn4.json.gz"
    ));
    load_saved_game_state(&raw).expect("the turn-4 capture deserializes and restores")
}

/// Non-vacuity guard: pin the shape that makes finalizing both necessary and
/// sufficient. Without this, a green below could mean the AI answered some
/// unrelated prompt.
fn assert_capture_is_the_reported_wedge(state: &GameState) {
    let WaitingFor::ManaPayment {
        player,
        convoke_mode,
    } = &state.waiting_for
    else {
        panic!(
            "capture must restore at the ManaPayment prompt, got {:?}",
            state.waiting_for
        );
    };
    assert_eq!(*player, STUCK, "seat 1 owes the payment");
    assert!(
        convoke_mode.is_none(),
        "no convoke is involved; this is the plain payment path"
    );

    let pending = state
        .pending_cast
        .as_ref()
        .expect("a pending cast is what the seat owes payment for");
    let ManaCost::Cost { shards, generic } = &pending.cost else {
        panic!("locked-in cost must be concrete, got {:?}", pending.cost);
    };
    assert_eq!(*generic, 0, "Galvanic Blast has no generic component");
    assert_eq!(
        shards.as_slice(),
        &[ManaCostShard::Red],
        "the locked-in cost is exactly {{R}}"
    );
    // No cost-reduction static applies, so the locked-in cost must equal the
    // printed one. This is what buys the row its independence from CR 601.2f.
    // `base_cost` is `Option` because `NoCost` is itself a real base and cannot
    // double as the absent sentinel; the capture carries `Some({R})`.
    assert_eq!(
        pending.base_cost.as_ref(),
        Some(&pending.cost),
        "no reduction applies to Galvanic Blast; base and locked-in cost agree"
    );

    let stuck_player = state
        .players
        .iter()
        .find(|player| player.id == STUCK)
        .expect("seat 1 is in the capture");
    let red_available = stuck_player
        .mana_pool
        .mana
        .iter()
        .filter(|unit| unit.color == ManaType::Red && unit.restrictions.is_empty())
        .count();
    assert!(
        red_available >= 1,
        "the floating pool must cover {{R}} or this row proves nothing; found \
         {red_available} unrestricted Red"
    );

    let untapped_sources = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == STUCK && !obj.tapped && !obj.available_mana_pips.is_empty())
        .count();
    assert_eq!(
        untapped_sources, 0,
        "with an untapped source available the seat would have a tap action and \
         finalizing would not be the only route"
    );
}

/// The AI must return a submittable action for a payment it can already make.
/// Returning `None` here is what the controller counts as a failure, and three
/// of them raise "Engine Connection Lost" and stop the game.
#[test]
fn ai_advances_a_manapayment_it_can_already_afford() {
    let state = load_turn4_capture();
    assert_capture_is_the_reported_wedge(&state);

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(4);
    let action = choose_action(&state, STUCK, &config, &mut rng);

    assert!(
        action.is_some(),
        "the AI returned no action for a ManaPayment it can settle from its \
         floating pool; three of these raise ai-controller-stuck:ManaPayment \
         and halt the game"
    );

    // CR 601.2h: finalizing pays the locked-in cost from the floating pool.
    // `CancelCast` also escapes the prompt, but it is the documented
    // `fallback_action` recovery for "the AI entered a cast it cannot complete"
    // — which is false here, so taking it would abandon an affordable spell.
    assert_eq!(
        action,
        Some(GameAction::PassPriority),
        "the affordable payment should be finalized, not abandoned"
    );
}

/// Proposal is only half the controller's loop. `aiController.ts` counts a
/// failure both when nothing is proposed *and* when the proposal is dispatched
/// and the reducer refuses it — the seat is equally stuck either way, and the
/// user-visible symptom (the game stops advancing) is identical. So drive the
/// real bounded controller and assert the prompt is actually left behind.
#[test]
fn the_controller_loop_leaves_the_manapayment_prompt() {
    let mut state = load_turn4_capture();
    assert_capture_is_the_reported_wedge(&state);

    let ai_players = HashSet::from([STUCK]);
    let ai_configs = HashMap::from([(
        STUCK,
        create_config(AiDifficulty::VeryHard, Platform::Native),
    )]);
    let mut rng = SmallRng::seed_from_u64(4);
    let session = AiSession::arc_from_game(&state);
    let run = run_ai_actions_bounded(&mut state, &ai_players, &ai_configs, &mut rng, &session, 1);

    assert_eq!(
        run.len(),
        1,
        "the bounded controller submitted nothing for a payment the seat can \
         afford; this is the failure aiController.ts counts toward \
         ai-controller-stuck:ManaPayment. stop reason: {:?}",
        run.stop
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::ManaPayment { .. }),
        "the submitted action did not advance past the payment prompt; the \
         seat is still parked at {:?}",
        state.waiting_for
    );
}
