//! Runtime + parser tests for the repeated-optional-payment + reflexive-modal
//! driver (Hawkeye, Master Marksman — "Trick Arrows"). CR 603.12a / CR 700.2d.
//!
//! These drive the real production seams:
//! - the parser (`parse_oracle_text`) for the modal AST (B4);
//! - the production trigger-resolution function (`resolve_ability_chain` at
//!   depth 0, exactly as the engine resolves a stack trigger) to reach the
//!   per-iteration `WaitingFor::OptionalEffectChoice`;
//! - the real action handler (`apply`) for every `DecideOptionalEffect` and
//!   `SelectModes`, so the changed `WaitingFor`/`GameAction` route is exercised.

#![cfg(test)]

use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::game::scenario::GameScenario;
use crate::parser::oracle::{parse_oracle_text, ParsedAbilities};
use crate::types::ability::{ModalChoice, QuantityExpr, QuantityRef, TargetRef};
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::mana::{ManaType, ManaUnit};
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;

const HAWKEYE_ORACLE: &str = "First strike, reach\n\
     Trick Arrows — Whenever Hawkeye becomes tapped, you may pay {1} up to three times. \
     When you do, choose up to that many.\n\
     • Net — Target creature can't block this turn.\n\
     • Explosive — Hawkeye deals 2 damage to target player.\n\
     • Boomerang — Discard a card, then draw a card.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn parse_hawkeye() -> ParsedAbilities {
    parse_oracle_text(
        HAWKEYE_ORACLE,
        "Hawkeye, Master Marksman",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    )
}

/// The reflexive sub-ability's modal (`when you do, choose up to that many`).
fn hawkeye_reflexive_modal(parsed: &ParsedAbilities) -> ModalChoice {
    let taps = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Taps)
        .expect("Hawkeye has a Taps trigger");
    let execute = taps.execute.as_ref().expect("Taps trigger has an execute");
    let reflexive = execute
        .sub_ability
        .as_ref()
        .expect("PayCost has a reflexive sub-ability");
    reflexive
        .modal
        .clone()
        .expect("reflexive sub carries a modal")
}

// ── B4 parser: the modal carries the resolution-local dynamic cap ────────

/// Revert discriminator: dropping the `"choose up to that many"` parser arm (or
/// the `ModalCountSpec::Dynamic { qty }` wiring) leaves the modal at the fixed
/// `(1, 1)` default with `dynamic_max_choices == None`, failing every assertion.
#[test]
fn hawkeye_modal_parses_with_resolution_local_dynamic_cap() {
    let parsed = parse_hawkeye();
    let modal = hawkeye_reflexive_modal(&parsed);

    assert_eq!(modal.min_choices, 0, "min is 0 — may choose no modes");
    assert_eq!(modal.mode_count, 3, "Net / Explosive / Boomerang");
    assert_eq!(
        modal.dynamic_max_choices,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::TimesCostPaidThisResolution
        }),
        "the cap binds to the resolution-local repeated-payment count"
    );
}

// ── Runtime harness ─────────────────────────────────────────────────────

/// Hawkeye on P0's battlefield with `mana` colorless mana in pool, the Taps
/// trigger resolved through the production resolver (depth 0). Leaves the engine
/// at the first per-iteration `WaitingFor::OptionalEffectChoice`.
fn hawkeye_runtime(mana: usize, p0_hand: &[&str]) -> crate::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.with_life(P1, 20);
    if !p0_hand.is_empty() {
        scenario.with_cards_in_hand(P0, p0_hand);
    }
    let hawkeye = scenario
        .add_creature_from_oracle(P0, "Hawkeye, Master Marksman", 3, 3, HAWKEYE_ORACLE)
        .id();
    if mana > 0 {
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(
                    ManaType::Colorless,
                    crate::types::identifiers::ObjectId(9_999),
                    false,
                    vec![]
                );
                mana
            ],
        );
    }
    let mut runner = scenario.build();

    // Resolve the Taps trigger exactly as the engine resolves a stack trigger:
    // `resolve_ability_chain` at depth 0 (the production trigger-resolution
    // path). This builds the per-iteration `OptionalEffectChoice` from the real
    // parsed ability — no hand-built `WaitingFor`.
    let parsed = parse_hawkeye();
    let taps = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::Taps)
        .unwrap();
    let execute = taps.execute.as_ref().unwrap();
    let resolved = build_resolved_from_def(execute, hawkeye, P0);

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0)
        .expect("trigger resolution");
    let _ = hawkeye;
    runner
}

fn decide(runner: &mut crate::game::scenario::GameRunner, accept: bool) {
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("DecideOptionalEffect accepted");
}

fn k_count(state: &GameState) -> u32 {
    state.optional_cost_payments_this_resolution
}

fn modal_cap(state: &GameState) -> Option<(usize, usize)> {
    match &state.waiting_for {
        WaitingFor::AbilityModeChoice { modal, .. } => Some((modal.min_choices, modal.max_choices)),
        _ => None,
    }
}

// ── B-i / B-ii / B-iii: K accounting, dynamic cap, reflexive K-gate ─────

/// Paying three times captures K = 3 and the reflexive is offered exactly once
/// with a modal capped at `min(K, mode_count) == 3`, `min_choices == 0`.
/// Revert discriminator: dropping the K increment in
/// `resolve_repeated_optional_payment_choice` leaves K = 0 ⇒ cap 0; dropping
/// the B4 dynamic-max wiring pins the cap at the fixed (1,1).
#[test]
fn pay_three_times_caps_modal_at_three_and_offers_reflexive_once() {
    let mut runner = hawkeye_runtime(3, &[]);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "first payment prompt is offered"
    );
    decide(&mut runner, true);
    decide(&mut runner, true);
    // Still a payment prompt before the third accept (not yet the modal).
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    decide(&mut runner, true);

    assert_eq!(k_count(runner.state()), 3, "three successful payments");
    assert_eq!(
        modal_cap(runner.state()),
        Some((0, 3)),
        "reflexive modal offered once, capped at min(K, 3) with min 0"
    );
    // CR 118.3a: the three {1} payments drained the mana pool.
    let pool_left: usize = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.mana_pool.total())
        .unwrap_or(0);
    assert_eq!(pool_left, 0, "all three {{1}} payments were made");
}

/// Paying twice then declining ends the loop early at K = 2; the reflexive modal
/// is offered once capped at 2. Revert discriminator: dropping the B4 dynamic
/// cap pins it at (1,1); not stopping on decline would prompt a third payment.
#[test]
fn pay_twice_then_decline_caps_modal_at_two() {
    let mut runner = hawkeye_runtime(3, &[]);
    decide(&mut runner, true);
    decide(&mut runner, true);
    decide(&mut runner, false); // decline the third — stop early

    assert_eq!(k_count(runner.state()), 2);
    assert_eq!(modal_cap(runner.state()), Some((0, 2)));
}

/// CR 603.12a: declining every payment leaves K = 0 — the reflexive NEVER fires
/// and the modal is never offered. Revert discriminator: removing the `K >= 1`
/// guard in `finish_repeated_optional_payment` resolves the reflexive at K = 0,
/// surfacing an `AbilityModeChoice` and failing this assertion.
#[test]
fn decline_immediately_skips_reflexive_at_k_zero() {
    let mut runner = hawkeye_runtime(3, &[]);
    decide(&mut runner, false);

    assert_eq!(k_count(runner.state()), 0);
    assert!(
        modal_cap(runner.state()).is_none(),
        "no modal is offered when K == 0: {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_repeated_optional_payment.is_none(),
        "the repeated-payment continuation is cleared"
    );
}

/// K counts only SUCCESSFUL payments, AND a failed payment ENDS the repeated
/// sequence. CR 118.3: a player can't pay a cost without the resources to pay it
/// fully. CR 603.12a: the "you may pay {1} [again]" rider is satisfied only by a
/// payment that actually happened. With funding for a single {1}, the first
/// accept pays (K=1); the second accept fails for lack of mana, so the loop
/// terminates immediately and the reflexive modal is offered exactly once, capped
/// at K=1 — NO third payment prompt is offered.
///
/// Discriminators: (a) dropping the `if !cost_payment_failed_flag` K-guard counts
/// the failed payment, yielding K=2 and a cap of 2; (b) the pre-fix behavior of
/// offering another prompt after a failed payment (the `if remaining > 0` branch
/// running regardless of `cost_payment_failed_flag`) leaves
/// `pending_repeated_optional_payment` Some with no modal offered, failing the
/// termination + cap assertions below.
#[test]
fn failed_payment_ends_sequence_and_offers_reflexive_once() {
    let mut runner = hawkeye_runtime(1, &[]);
    decide(&mut runner, true); // pays {1} — K = 1
    decide(&mut runner, true); // accept, but no mana left — payment FAILS → loop ends

    assert_eq!(
        k_count(runner.state()),
        1,
        "only the funded payment counts toward K"
    );
    assert!(
        runner.state().pending_repeated_optional_payment.is_none(),
        "a failed payment terminates the repeated-payment sequence — no further \
         payment prompt is offered: {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        modal_cap(runner.state()),
        Some((0, 1)),
        "the reflexive modal is offered exactly once, capped at K=1"
    );
}

// ── B-iv: a chosen mode resolves; reflexive resolves exactly once ───────

/// Choosing the Boomerang mode once resolves it exactly once (discard a card,
/// then draw a card — net hand size unchanged), and the resolution settles
/// without offering a second modal. Drives `SelectModes` through `apply`.
/// Revert discriminator: resolving the reflexive per payment (the old
/// `repeated_full_chain` path) would offer the modal three times.
#[test]
fn boomerang_mode_resolves_once_through_apply() {
    let mut runner = hawkeye_runtime(3, &["Mountain", "Forest"]);
    decide(&mut runner, true);
    decide(&mut runner, true);
    decide(&mut runner, true);

    // The reflexive modal is offered once. Boomerang is mode index 2.
    assert_eq!(modal_cap(runner.state()), Some((0, 3)));
    let hand_before = p0_hand_size(runner.state());

    runner
        .act(GameAction::SelectModes { indices: vec![2] })
        .expect("SelectModes accepted");

    // Boomerang = discard 1, then draw 1 → net hand size unchanged, and no
    // second modal is offered (reflexive resolved exactly once).
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "no second modal after the single reflexive resolves"
    );
    assert_eq!(
        p0_hand_size(runner.state()),
        hand_before,
        "Boomerang discards one then draws one (net 0)"
    );
}

/// CR 700.2b + CR 120.3: choosing a TARGETED reflexive mode (Explosive — "Hawkeye
/// deals 2 damage to target player") resolves exactly once with correct targeting
/// after the K-payment sweep. Drives `SelectModes` then `ChooseTarget` through
/// `apply`; closes the targeted-mode coverage gap (Boomerang is the no-target
/// case). Revert discriminator: resolving the reflexive per payment (the old
/// `repeated_full_chain` path) would deal 2 damage three times (P1 → 14) and/or
/// re-offer the modal; the single post-loop reflexive deals exactly 2 (P1 → 18).
#[test]
fn explosive_targeted_mode_resolves_once_through_apply() {
    let mut runner = hawkeye_runtime(3, &[]);
    decide(&mut runner, true);
    decide(&mut runner, true);
    decide(&mut runner, true);

    assert_eq!(modal_cap(runner.state()), Some((0, 3)));
    assert_eq!(
        p1_life(runner.state()),
        20,
        "no damage before the mode resolves"
    );

    // Explosive is mode index 1 (Net = 0, Explosive = 1, Boomerang = 2).
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("SelectModes accepted");

    // A targeted triggered mode surfaces a per-target prompt.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "Explosive needs a target player, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("ChooseTarget accepted");

    // The reflexive Explosive ability is on the stack with its target bound;
    // resolve it through the real priority/stack machinery.
    runner.advance_until_stack_empty();

    assert_eq!(
        p1_life(runner.state()),
        18,
        "Explosive deals exactly 2 damage once (not 2×K)"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::AbilityModeChoice { .. }
        ),
        "no second modal after the single reflexive resolves"
    );
}

/// Fix 1 (HIGH) — CR 603.12a + CR 700.2d: at the per-iteration
/// `OptionalEffectChoice` pause, K (`optional_cost_payments_this_resolution`) is
/// already nonzero and the paired `pending_repeated_optional_payment` continuation
/// is `Some`. That pause spans separate `apply()` calls and is a serde boundary
/// (server crash/restart via `to_persisted`/`from_persisted`, single-player
/// save/load, multiplayer host-resume). K must survive it — a roundtrip restoring
/// K = 0 collapses the reflexive modal cap below the payments actually made,
/// denying the player modes they paid for.
///
/// Non-vacuity (MEASURED): with the field at `#[serde(skip, default)]` the
/// roundtrip restores K = 0, so resuming with a decline runs
/// `finish_repeated_optional_payment` at K = 0 → the reflexive is SKIPPED and no
/// modal is offered. Measured left/right under that revert: `restored.K` = `0`
/// vs expected `2`, and the resumed cap = `None` vs expected `Some((0, 2))`.
#[test]
fn k_counter_survives_serde_roundtrip_mid_payment_loop() {
    let mut runner = hawkeye_runtime(3, &[]);
    decide(&mut runner, true);
    decide(&mut runner, true);
    // Paused at the third payment prompt: K = 2, continuation pending.
    assert_eq!(k_count(runner.state()), 2);
    assert!(runner.state().pending_repeated_optional_payment.is_some());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));

    // Serialize across the pause (the persistence boundary) and restore.
    let json = serde_json::to_string(runner.state()).expect("serialize paused state");
    let restored: GameState = serde_json::from_str(&json).expect("deserialize paused state");
    assert_eq!(
        restored.optional_cost_payments_this_resolution, 2,
        "K must survive the mid-payment-loop serde boundary"
    );
    assert!(
        restored.pending_repeated_optional_payment.is_some(),
        "the paired continuation also survives"
    );
    // K is eq-INCLUDED: a state differing only in K is no longer equal. Revert
    // discriminator: removing K from `PartialEq` makes these compare equal, so an
    // AI-search dedup or save-equality check would treat two different payment
    // counts as identical.
    let mut k_perturbed = restored.clone();
    k_perturbed.optional_cost_payments_this_resolution = 99;
    assert_ne!(
        k_perturbed, restored,
        "K participates in PartialEq (states differing only in K are unequal)"
    );

    // Resume from the RESTORED state and decline the third payment. The reflexive
    // modal cap must reflect the two payments already made (CR 700.2d), proving K
    // survived: a lost K would skip the reflexive entirely.
    *runner.state_mut() = restored;
    decide(&mut runner, false);
    assert_eq!(
        modal_cap(runner.state()),
        Some((0, 2)),
        "resumed reflexive cap = min(K = 2, mode_count = 3)"
    );
}

fn p0_hand_size(state: &GameState) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == P0)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

fn p1_life(state: &GameState) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == P1)
        .map(|p| p.life)
        .unwrap_or(0)
}
