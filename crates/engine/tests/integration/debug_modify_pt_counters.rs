//! Click-mode P/T counter edits: the `DebugAction::ModifyCounters` shape the
//! card menu sends for its +1/+1 and -1/-1 rows.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

fn debug_grizzly_bears() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;
    (runner, creature)
}

fn add_counter(runner: &mut GameRunner, object_id: ObjectId, counter_type: CounterType) {
    runner
        .act(GameAction::Debug(DebugAction::ModifyCounters {
            object_id,
            counter_type,
            delta: 1,
        }))
        .expect("debug ModifyCounters should succeed");
}

#[test]
fn debug_adds_first_plus_one_counter_from_zero() {
    let (mut runner, creature) = debug_grizzly_bears();

    add_counter(&mut runner, creature, CounterType::Plus1Plus1);

    // CR 122.1a: a +1/+1 counter adds 1 to the creature's power and toughness.
    let obj = &runner.state().objects[&creature];
    assert_eq!(obj.counters.get(&CounterType::Plus1Plus1), Some(&1));
    assert_eq!(obj.power, Some(3));
    assert_eq!(obj.toughness, Some(3));
}

#[test]
fn debug_staged_pt_counters_annihilate_at_next_sba_check() {
    let (mut runner, creature) = debug_grizzly_bears();

    add_counter(&mut runner, creature, CounterType::Plus1Plus1);
    add_counter(&mut runner, creature, CounterType::Plus1Plus1);
    add_counter(&mut runner, creature, CounterType::Minus1Minus1);

    // The raw debug edit runs no SBA, so both counter types are staged together.
    let obj = &runner.state().objects[&creature];
    assert_eq!(obj.counters.get(&CounterType::Plus1Plus1), Some(&2));
    assert_eq!(obj.counters.get(&CounterType::Minus1Minus1), Some(&1));

    runner
        .act(GameAction::PassPriority)
        .expect("PassPriority should succeed");

    // CR 704.3 + CR 704.5q: the SBA check run before the next player gets priority removes N = min(+1/+1, -1/-1) of each counter.
    let obj = &runner.state().objects[&creature];
    assert_eq!(obj.counters.get(&CounterType::Plus1Plus1), Some(&1));
    assert_eq!(obj.counters.get(&CounterType::Minus1Minus1), None);
    assert_eq!(obj.power, Some(3));
    assert_eq!(obj.toughness, Some(3));
}
