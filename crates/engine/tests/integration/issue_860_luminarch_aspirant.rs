//! Issue #860 — Luminarch Aspirant begin-combat trigger must surface target
//! selection instead of clobbering `waiting_for` with Priority.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const LUMINARCH_ASPIRANT: &str =
    "At the beginning of combat on your turn, put a +1/+1 counter on target creature you control.";

fn p1p1_counters(runner: &engine::game::scenario::GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object still present")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

#[test]
fn issue_860_luminarch_aspirant_prompts_for_target_at_begin_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Luminarch Aspirant", 1, 1, LUMINARCH_ASPIRANT);
    scenario.add_creature(P0, "Recipient", 2, 2);

    let mut runner = scenario.build();
    runner.pass_both_players();

    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "begin-combat trigger must prompt for a target, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn issue_860_luminarch_aspirant_puts_counter_on_chosen_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Luminarch Aspirant", 1, 1, LUMINARCH_ASPIRANT);
    let recipient = scenario.add_creature(P0, "Recipient", 2, 2).id();

    let mut runner = scenario.build();
    runner.pass_both_players();

    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(recipient)],
        })
        .expect("select target creature for begin-combat trigger");
    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1_counters(&runner, recipient),
        1,
        "Luminarch Aspirant must put a +1/+1 counter on the chosen creature"
    );
}

#[test]
fn issue_860_two_luminarch_aspirants_both_trigger_at_begin_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Luminarch Aspirant A", 1, 1, LUMINARCH_ASPIRANT);
    scenario.add_creature_from_oracle(P0, "Luminarch Aspirant B", 1, 1, LUMINARCH_ASPIRANT);
    let recipient = scenario.add_creature(P0, "Recipient", 2, 2).id();

    let mut runner = scenario.build();
    runner.pass_both_players();

    assert_eq!(runner.state().phase, Phase::BeginCombat);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "first begin-combat trigger must prompt for target selection, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().deferred_triggers.len(),
        1,
        "second copy must remain deferred until the first resolves targeting"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(recipient)),
        })
        .expect("select target for first trigger");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "second trigger must prompt for target selection instead of Priority, got {:?}",
        runner.state().waiting_for
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(recipient)),
        })
        .expect("select target for second trigger");

    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1_counters(&runner, recipient),
        2,
        "both Luminarch Aspirants must put a +1/+1 counter on the chosen creature"
    );
}
