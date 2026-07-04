//! Runtime companion to issue #735 (Edit B) — Lily Bowen, Raging Grandma is the
//! OTHER player/phase-subject card whose "if its power is …" clause must bind
//! the ability SOURCE, so the same `strip_property_conditional` fix that repairs
//! Amalia must also drive Lily correctly.
//!
//! Lily Bowen: "At the beginning of your upkeep, double the number of +1/+1
//! counters on Lily Bowen if its power is 16 or less. Otherwise, remove all but
//! one +1/+1 counter from it, …"
//!
//! The upkeep trigger's subject is the phase actor ("your upkeep" → the ability
//! controller), so Edit B binds "its power" to Lily herself (SOURCE). The
//! double therefore fires exactly when LILY's own power is ≤ 16.
//!
//! The upkeep trigger is fired directly with a `PhaseChanged { Upkeep }` event
//! (the `Phase` trigger matcher keys off it), avoiding the untap/draw/SBA turn
//! machinery — Lily is a base 0/0 that would otherwise die to a state-based
//! action mid-advance before her counters settle.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 608.2h: the power comparison reads Lily's current power once, when the
//!     doubling effect is applied.
//!   - CR 208.1: Lily's power equals her +1/+1 counter count (base 0/0).
//!   - CR 503: the beginning-of-upkeep trigger is put on the stack during the
//!     upkeep step.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const LILY_ORACLE: &str = "Vigilance\nLily Bowen enters with two +1/+1 counters on it.\nAt the beginning of your upkeep, double the number of +1/+1 counters on Lily Bowen if its power is 16 or less. Otherwise, remove all but one +1/+1 counter from it, then you gain 1 life for each +1/+1 counter removed this way.";

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Build Lily (base 0/0) with `counters` +1/+1 counters (power == counters),
/// parked at P0's upkeep so a `PhaseChanged { Upkeep }` event fires her
/// beginning-of-upkeep trigger.
fn setup(counters: u32) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let lily = scenario
        .add_creature_from_oracle(P0, "Lily Bowen, Raging Grandma", 0, 0, LILY_ORACLE)
        .with_plus_counters(counters)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    engine::game::layers::evaluate_layers(runner.state_mut());
    (runner, lily)
}

/// Fire P0's beginning-of-upkeep trigger and resolve it.
fn fire_upkeep(runner: &mut GameRunner) {
    let events = vec![GameEvent::PhaseChanged {
        phase: Phase::Upkeep,
    }];
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();
}

/// POSITIVE repair (power ≤ 16): with Lily's power at 2 (two +1/+1 counters),
/// the upkeep trigger DOUBLES the counters — 2 → 4. This is the Source-bound
/// condition firing: `Power{Source} LE 16` is true, so `MultiplyCounter` runs.
#[test]
fn lily_power_at_most_16_doubles_counters() {
    let (mut runner, lily) = setup(2);
    assert_eq!(
        p1p1(&runner, lily),
        2,
        "precondition: Lily has 2 counters (power 2)"
    );

    fire_upkeep(&mut runner);

    assert_eq!(
        p1p1(&runner, lily),
        4,
        "with Lily's power ≤ 16 the upkeep must DOUBLE her +1/+1 counters (2 -> 4)"
    );
}

/// NEGATIVE (power > 16): with 17 counters (power 17) the `Power{Source} LE 16`
/// condition is FALSE, so the doubling does NOT fire and the counter count is
/// not doubled (it must not become 34). This proves the condition is bound to
/// Lily's own power, not treated as always-true.
#[test]
fn lily_power_above_16_does_not_double() {
    let (mut runner, lily) = setup(17);
    assert_eq!(
        p1p1(&runner, lily),
        17,
        "precondition: Lily has 17 counters (power 17)"
    );

    fire_upkeep(&mut runner);

    assert_ne!(
        p1p1(&runner, lily),
        34,
        "with Lily's power > 16 the doubling must NOT fire (count must not double to 34)"
    );
}
