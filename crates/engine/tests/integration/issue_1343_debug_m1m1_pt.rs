//! Issue #1343: click-mode M1M1 counters must update power/toughness display.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::phase::Phase;

#[test]
fn debug_modify_counters_m1m1_updates_power_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    runner
        .act(GameAction::Debug(DebugAction::ModifyCounters {
            object_id: creature,
            counter_type: CounterType::Minus1Minus1,
            delta: 1,
        }))
        .expect("debug ModifyCounters should succeed");

    let obj = &runner.state().objects[&creature];
    assert_eq!(
        obj.counters.get(&CounterType::Minus1Minus1),
        Some(&1),
        "M1M1 counter should be stored as typed Minus1Minus1"
    );
    assert_eq!(obj.power, Some(1));
    assert_eq!(obj.toughness, Some(1));
}
