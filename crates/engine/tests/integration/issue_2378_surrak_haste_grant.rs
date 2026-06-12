//! Regression for issue #2378: Surrak and Goreclaw's ETB trigger applies the
//! +1/+1 counter but the sibling "It gains haste until end of turn" grant does
//! not take effect.
//!
//! https://github.com/phase-rs/phase/issues/2378
//!
//! Oracle: "Trample\nOther creatures you control have trample.\nWhenever another
//! nontoken creature you control enters, put a +1/+1 counter on it. It gains
//! haste until end of turn."

use engine::game::keywords::has_haste;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

const SURRAK_ORACLE: &str = "Trample\nOther creatures you control have trample.\n\
Whenever another nontoken creature you control enters, put a +1/+1 counter on it. \
It gains haste until end of turn.";

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// A nontoken creature entering under Surrak gets BOTH the +1/+1 counter and
/// haste until end of turn (CR 611.2c continuous effect, CR 702.10a haste).
/// FAILS on origin/main: the counter is applied but haste is dropped.
#[test]
fn surrak_grants_haste_and_counter_to_entering_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Surrak on P0's battlefield — its "whenever another nontoken creature you
    // control enters" trigger is registered from the parsed oracle text.
    scenario.add_creature_from_oracle(P0, "Surrak and Goreclaw", 6, 7, SURRAK_ORACLE);

    let bear = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 1,
        })
        .id();

    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Green, bear, false, vec![]),
            ManaUnit::new(ManaType::Green, bear, false, vec![]),
        ],
    );

    let mut runner = scenario.build();

    // Cast and resolve the bear; its ETB fires Surrak's trigger, which itself
    // resolves off the stack.
    runner.cast(bear).resolve();

    assert_eq!(
        plus_one_counters(&runner, bear),
        1,
        "Surrak must put a +1/+1 counter on the entering creature"
    );

    assert!(
        has_haste(&runner.state().objects[&bear]),
        "Surrak's sibling clause must grant the entering creature haste until end of turn"
    );
}
