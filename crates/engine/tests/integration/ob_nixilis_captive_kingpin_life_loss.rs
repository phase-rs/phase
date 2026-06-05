//! Runtime integration tests for Ob Nixilis, Captive Kingpin's life-loss trigger.
//!
//! Oracle text (relevant trigger line):
//!   "Whenever one or more opponents each lose exactly 1 life, put a +1/+1
//!    counter on Ob Nixilis, Captive Kingpin. Exile the top card of your
//!    library. Until your next end step, you may play that card."
//!
//! These tests prove the wired pipeline: a real life-loss event flows through
//! trigger collection (which re-applies the `life_amount` magnitude filter) and
//! lands the +1/+1 counter on Ob Nixilis — and that a 2-life loss does NOT
//! trigger it.
//!
//! CR 119.3: If an effect causes a player to lose life, that player's life
//!           total is adjusted accordingly.
//! CR 603.2c: An ability triggers only once each time its trigger event occurs.
//!            "One or more … each" is a batched trigger: it fires at most once
//!            per event batch, even if multiple opponents lose life simultaneously.
//!
//! The "exactly 1" magnitude gate (`life_amount: Some((EQ, 1))`) is the
//! negative-test guard: a 2-life loss must NOT satisfy the constraint, so the
//! trigger must not fire.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

/// Printed Oracle text for Ob Nixilis, Captive Kingpin (Wilds of Eldraine).
/// Byte-identical to the card-data export used by the engine's trigger parser.
const OB_NIXILIS_ORACLE: &str = "\
Whenever one or more opponents each lose exactly 1 life, put a +1/+1 counter \
on Ob Nixilis, Captive Kingpin. Exile the top card of your library. Until your \
next end step, you may play that card.";

/// CR 119.3 + CR 603.2c: Ob Nixilis, Captive Kingpin's trigger fires when an
/// opponent loses EXACTLY 1 life.  A free instant "Target opponent loses 1
/// life." is cast, targeting P1.  After full resolution (spell + triggered
/// ability), Ob Nixilis must have exactly one +1/+1 counter and P1 must have
/// lost exactly 1 life.
///
/// This exercises the full wired pipeline: life-loss event → trigger-collection
/// `life_amount` magnitude check → +1/+1 counter placement.
#[test]
fn ob_nixilis_triggers_when_opponent_loses_exactly_one_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Ob Nixilis on P0's battlefield (the trigger source and counter target).
    let ob = scenario
        .add_creature_from_oracle(P0, "Ob Nixilis, Captive Kingpin", 4, 3, OB_NIXILIS_ORACLE)
        .id();

    // P0's library must be non-empty so the trigger's "Exile the top card of
    // your library" step has a card to exile.  An empty library is safe
    // (ExileTop clamps to 0), but having a card avoids any potential empty-set
    // edge in the downstream GrantCastingPermission chain.
    scenario.with_library_top(P0, &["Dummy Card"]);

    // A free instant that causes P1 to lose exactly 1 life.
    // `add_spell_to_hand_from_oracle` produces a ManaCost::zero() spell — no
    // mana payment step surfaces, so the cast driver proceeds directly to the
    // target-selection and priority windows.
    let drain = scenario
        .add_spell_to_hand_from_oracle(P0, "Drain One", true, "Target opponent loses 1 life.")
        .id();

    let mut runner = scenario.build();

    // Preconditions.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&ob)
            .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
            .unwrap_or(0),
        0,
        "precondition: Ob Nixilis starts with no +1/+1 counters"
    );
    assert_eq!(runner.life(P1), 20, "precondition: P1 starts at 20 life");

    // CR 601.2a–h: cast the spell through the full pipeline.  The driver
    // answers the TargetSelection prompt (P1 is the declared player target)
    // and then drives resolution (spell + triggered ability) to completion.
    let outcome = runner.cast(drain).target_player(P1).resolve();

    // CR 119.3: P1 loses exactly 1 life.
    outcome.assert_life_delta(P1, -1);

    // CR 603.2c + life_amount EQ 1: the trigger fires because the magnitude
    // of the life-loss event (1) satisfies the "exactly 1" constraint.
    assert_eq!(
        outcome.counters(ob, CounterType::Plus1Plus1),
        1,
        "CR 603.2c + CR 119.3: Ob Nixilis must receive exactly one +1/+1 counter \
         when an opponent loses exactly 1 life"
    );
}

/// CR 119.3 + CR 603.2c (negative case): Ob Nixilis, Captive Kingpin's trigger
/// does NOT fire when an opponent loses 2 life.  The "exactly 1" magnitude gate
/// (`life_amount: Some((EQ, 1))`) rejects the event because the magnitude (2)
/// does not equal 1.
///
/// Asserts that no +1/+1 counter is placed on Ob Nixilis and that P1 did in
/// fact lose 2 life (confirming the loss happened but did not satisfy the
/// trigger condition).
#[test]
fn ob_nixilis_does_not_trigger_on_two_life_loss() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Ob Nixilis on P0's battlefield.
    let ob = scenario
        .add_creature_from_oracle(P0, "Ob Nixilis, Captive Kingpin", 4, 3, OB_NIXILIS_ORACLE)
        .id();

    // Library card for the exile step (same reasoning as the positive test).
    scenario.with_library_top(P0, &["Dummy Card"]);

    // A free instant that causes P1 to lose 2 life.
    let drain_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Drain Two", true, "Target opponent loses 2 life.")
        .id();

    let mut runner = scenario.build();

    // Preconditions.
    assert_eq!(
        runner
            .state()
            .objects
            .get(&ob)
            .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
            .unwrap_or(0),
        0,
        "precondition: Ob Nixilis starts with no +1/+1 counters"
    );
    assert_eq!(runner.life(P1), 20, "precondition: P1 starts at 20 life");

    // CR 601.2a–h: cast through the full pipeline, resolving the life-loss
    // effect against P1.
    let outcome = runner.cast(drain_two).target_player(P1).resolve();

    // CR 119.3: P1 loses exactly 2 life — the loss happened.
    outcome.assert_life_delta(P1, -2);

    // CR 603.2c + life_amount EQ 1 (negative gate): the trigger does NOT fire
    // because the magnitude of the life-loss event (2) does not equal 1.
    assert_eq!(
        outcome.counters(ob, CounterType::Plus1Plus1),
        0,
        "CR 603.2c: Ob Nixilis must NOT receive a counter when an opponent loses \
         2 life — the 'exactly 1' magnitude constraint rejects this event"
    );
}
