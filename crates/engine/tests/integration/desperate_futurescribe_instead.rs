//! Desperate Futurescribe (FRA): "At the beginning of combat on your turn,
//! another target creature you control gets +1/+1 until end of turn. If
//! you've scried or surveilled this turn, put a +1/+1 counter on that
//! creature instead." Exercises the `StaticCondition::Or` verb-list parse
//! through `ConditionInstead` (`ability_utils.rs::apply_instead_swap`) — the
//! INSTEAD branch trades the temporary pump for a permanent counter, so P/T
//! must land on base+1 either way (the counter's own +1/+1, not stacked with
//! the pump).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const FUTURESCRIBE_ORACLE: &str = "Flying\nAt the beginning of combat on your turn, another target creature you control gets +1/+1 until end of turn. If you've scried or surveilled this turn, put a +1/+1 counter on that creature instead.";

fn power(runner: &GameRunner, id: ObjectId) -> i32 {
    runner.state().objects[&id].power.unwrap_or(0)
}

fn plus1plus1_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    *runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .unwrap_or(&0)
}

/// Advance from precombat main to begin combat, answer the trigger's target
/// selection with `target`, then resolve it to completion.
fn resolve_begin_combat_trigger(runner: &mut GameRunner, target: ObjectId) {
    runner.pass_both_players();
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ) {
        runner
            .act(GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target)],
            })
            .expect("select Futurescribe's begin-combat target");
    }
    runner.advance_until_stack_empty();
}

/// No scry or surveil this turn — the plain pump branch applies: +1/+1
/// until end of turn, zero +1/+1 counters.
#[test]
fn futurescribe_pumps_without_counter_when_no_scry_or_surveil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Desperate Futurescribe", 3, 4, FUTURESCRIBE_ORACLE);
    let target = scenario.add_creature(P0, "Bear", 2, 2).id();
    let mut runner = scenario.build();

    resolve_begin_combat_trigger(&mut runner, target);

    assert_eq!(power(&runner, target), 3, "2/2 base + 1/+1 pump = 3 power");
    assert_eq!(
        plus1plus1_counters(&runner, target),
        0,
        "no scry/surveil this turn: the instead branch must not fire"
    );
}

/// After `Surveil 1.` in main phase 1, the instead branch fires — a
/// permanent +1/+1 counter instead of the temporary pump.
#[test]
fn futurescribe_grants_counter_after_surveil() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    scenario.add_creature_from_oracle(P0, "Desperate Futurescribe", 3, 4, FUTURESCRIBE_ORACLE);
    let target = scenario.add_creature(P0, "Bear", 2, 2).id();
    let surveil_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Surveil Spell", true, "Surveil 1.")
        .id();
    let mut runner = scenario.build();
    runner.cast(surveil_spell).resolve();

    resolve_begin_combat_trigger(&mut runner, target);

    assert_eq!(
        plus1plus1_counters(&runner, target),
        1,
        "the instead branch must put a +1/+1 counter on the target"
    );
    assert_eq!(
        power(&runner, target),
        3,
        "power must land on base+1 (the counter), not base+2 from a stacked pump"
    );
}

/// The same, but the condition is satisfied by `Scry 1.` instead.
#[test]
fn futurescribe_grants_counter_after_scry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Library Card");
    scenario.add_creature_from_oracle(P0, "Desperate Futurescribe", 3, 4, FUTURESCRIBE_ORACLE);
    let target = scenario.add_creature(P0, "Bear", 2, 2).id();
    let scry_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Test Scry Spell", true, "Scry 1.")
        .id();
    let mut runner = scenario.build();
    runner.cast(scry_spell).resolve();

    resolve_begin_combat_trigger(&mut runner, target);

    assert_eq!(
        plus1plus1_counters(&runner, target),
        1,
        "the instead branch must put a +1/+1 counter on the target"
    );
    assert_eq!(
        power(&runner, target),
        3,
        "power must land on base+1 (the counter), not base+2 from a stacked pump"
    );
}
