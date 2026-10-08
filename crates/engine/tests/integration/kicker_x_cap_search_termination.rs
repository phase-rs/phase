//! Kicker {X} — declaring the kicker must reach the X announcement with a
//! finite cap (CR 601.2b: X is announced as part of determining the total cost).
//!
//! Before the fix the announcement was never reached at all. Declaring the
//! kicker merges its `{X}` into `pending.cost`, so `enter_payment_step` asks
//! `max_x_value_excluding` for the cap; that function refines the arithmetic
//! bound by recomputing the spell's concrete total per X — from `base_cost`,
//! which carries the spell's own mana cost and NOT the additional cost's `{X}`.
//! For a spell whose printed mana cost has no X (Thieving Skydiver `{1}{U}`,
//! Toxic Deluge, Hatred, …) that recomputed total does not move with X, so the
//! monotone predicate is true for every X and the exponential probe in
//! `largest_x_satisfying` doubled `hi` until `saturating_mul` pinned it at
//! `u32::MAX` and it stopped growing — an endless loop. In the browser the
//! engine worker simply never answered ("Engine took too long", 60 s watchdog).
//!
//! Built via the `/card-test` recipe: `GameScenario` + the real cast pipeline
//! (`GameAction::CastSpell` -> `DecideOptionalCost`). The cast runs on its own
//! thread with a large stack and a receive deadline, so a regression fails this
//! test with a message instead of hanging CI or aborting the process on a
//! stack overflow.
//!
//! REVERT DISCRIMINATOR: `paying_a_kicker_x_reaches_a_finite_x_cap`. Route
//! additional-cost-only X through the unbounded probe and this test times out
//! on the deadline below.

use std::sync::mpsc;
use std::time::Duration;

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{AbilityCost, AdditionalCost, AdditionalCostRepeatability};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::phase::Phase;

/// A spell shaped like Thieving Skydiver (`data/card-data.json`): printed mana
/// cost `{1}{U}`, plus the optional additional cost `Kicker {X}`. The oracle
/// line is the flying keyword alone so the fixture isolates the cost path.
fn cast_and_decide_kicker(lands: usize, pay_kicker: bool) -> WaitingFor {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let skydiver = scenario
        .add_creature_to_hand_from_oracle(P0, "Kicker X Flier", 1, 1, "Flying")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        // CR 702.33a: "Kicker [cost]" is an optional additional cost.
        .with_additional_cost(AdditionalCost::Kicker {
            costs: vec![AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![ManaCostShard::X],
                },
            }],
            repeatability: AdditionalCostRepeatability::Once,
        })
        .id();

    for _ in 0..lands {
        scenario.add_basic_land(P0, ManaColor::Blue);
    }

    let mut runner = scenario.build();
    let card = runner.state().objects[&skydiver].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: skydiver,
            card_id: card,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("P0 casts the kicker-{X} spell");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalCostChoice { .. }
        ),
        "the kicker must be offered before X is announced (CR 601.2b)",
    );

    runner
        .act(GameAction::DecideOptionalCost { pay: pay_kicker })
        .expect("P0 decides the kicker");
    runner.state().waiting_for.clone()
}

/// Run the cast off-thread so a non-terminating cap search is reported as a
/// failed deadline rather than hanging the suite. The generous stack keeps a
/// regression from aborting the whole test process with a stack overflow before
/// the deadline can fire.
fn decide_kicker_within(deadline: Duration, lands: usize, pay_kicker: bool) -> WaitingFor {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let _ = tx.send(cast_and_decide_kicker(lands, pay_kicker));
        })
        .expect("spawn the cast thread");
    rx.recv_timeout(deadline)
        .expect("declaring the kicker must terminate: the X cap search did not return")
}

/// PRIMARY REVERT DISCRIMINATOR. Eight Islands against a `{1}{U}` spell leave
/// six mana for the kicker's `{X}` (CR 601.2f: the cap is what the caster can
/// actually pay), and the announcement must be reached to offer it.
#[test]
fn paying_a_kicker_x_reaches_a_finite_x_cap() {
    match decide_kicker_within(Duration::from_secs(30), 8, true) {
        WaitingFor::ChooseXValue { max, .. } => assert_eq!(
            max, 6,
            "the kicker's X cap must be the mana left after the spell's own {{1}}{{U}}",
        ),
        other => panic!("expected the X announcement, got {other:?}"),
    }
}

/// The cap tracks the actual pool, so it is not a constant the fix could have
/// hard-coded: four Islands leave two.
#[test]
fn the_kicker_x_cap_tracks_available_mana() {
    match decide_kicker_within(Duration::from_secs(30), 4, true) {
        WaitingFor::ChooseXValue { max, .. } => assert_eq!(max, 2),
        other => panic!("expected the X announcement, got {other:?}"),
    }
}

/// Positive direction: declining the kicker never reached the cap search and
/// must keep working exactly as before.
#[test]
fn declining_the_kicker_x_needs_no_x_announcement() {
    assert!(
        matches!(
            decide_kicker_within(Duration::from_secs(30), 8, false),
            WaitingFor::Priority { .. }
        ),
        "an undeclared kicker announces no X",
    );
}
