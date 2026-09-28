//! Shelinda, Yevon Acolyte: a trailing "if its power is less than ~'s power"
//! gates the first counter placement, and "Otherwise" is its else branch.
//!
//! The comparison reads the entering creature's power against Shelinda's at
//! resolution. Exactly one of the two branches must place a counter, and the
//! equal-power case must take the "Otherwise" branch because "less than" is
//! strict.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const SHELINDA_ORACLE: &str = "Lifelink\n\
    Whenever another creature you control enters, put a +1/+1 counter on that creature if its power is less than Shelinda's power. Otherwise, put a +1/+1 counter on Shelinda.";

fn p1p1_counters(state: &engine::types::game_state::GameState, object: ObjectId) -> u32 {
    state
        .objects
        .get(&object)
        .and_then(|card| card.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

/// Cast a vanilla creature of the given power while a 3/3 Shelinda is on the
/// battlefield, resolve Shelinda's trigger, and return
/// `(counters on the entering creature, counters on Shelinda)`.
fn resolve_entering_creature_with_power(power: i32) -> (u32, u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let shelinda = scenario
        .add_creature_from_oracle(P0, "Shelinda, Yevon Acolyte", 3, 3, SHELINDA_ORACLE)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let entering = scenario
        .add_creature_to_hand(P0, "Entering Creature", power, power)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(entering).resolve();

    (
        p1p1_counters(outcome.state(), entering),
        p1p1_counters(outcome.state(), shelinda),
    )
}

/// CR 208.1 + CR 608.2c: the entering 1/1 has less power than Shelinda's 3,
/// so the gated instruction puts the counter on the entering creature and the
/// "Otherwise" branch does nothing.
#[test]
fn shelinda_puts_the_counter_on_a_smaller_entering_creature() {
    let (entering, shelinda) = resolve_entering_creature_with_power(1);

    assert_eq!(
        entering, 1,
        "the smaller entering creature must receive the +1/+1 counter"
    );
    assert_eq!(
        shelinda, 0,
        "the Otherwise branch must not also fire when the condition holds"
    );
}

/// CR 208.1 + CR 608.2c: the entering 5/5 has more power than Shelinda, so
/// the condition fails and the "Otherwise" branch puts the counter on Shelinda.
#[test]
fn shelinda_takes_the_counter_when_the_entering_creature_is_bigger() {
    let (entering, shelinda) = resolve_entering_creature_with_power(5);

    assert_eq!(
        entering, 0,
        "a bigger entering creature must not receive the counter"
    );
    assert_eq!(
        shelinda, 1,
        "the Otherwise branch must put the counter on Shelinda"
    );
}

/// CR 208.1 + CR 608.2c: "less than" is strict, so equal power (3 vs 3) fails
/// the condition and the counter goes on Shelinda.
#[test]
fn shelinda_takes_the_counter_on_equal_power() {
    let (entering, shelinda) = resolve_entering_creature_with_power(3);

    assert_eq!(
        entering, 0,
        "an equal-power entering creature must not receive the counter"
    );
    assert_eq!(
        shelinda, 1,
        "equal power fails the strict comparison, so Shelinda gets the counter"
    );
}
