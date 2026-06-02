//! Issues #1610 / #1660: Lathiel, the Bounteous Dawn end-step distributed +1/+1
//! counters from life gained this turn.
//!
//! Oracle: "Lifelink. At the beginning of each end step, if you gained life this
//! turn, distribute up to that many +1/+1 counters among any number of other
//! target creatures."

use super::rules::{run_combat, GameScenario, Phase, WaitingFor, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::DistributionUnit;
use engine::types::identifiers::ObjectId;

const LATHIEL_ORACLE: &str = "Lifelink\nAt the beginning of each end step, if you gained life this turn, distribute up to that many +1/+1 counters among any number of other target creatures.";

fn advance_to_end_step_trigger(runner: &mut super::rules::GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. }
            | WaitingFor::TargetSelection { .. }
            | WaitingFor::DistributeAmong { .. } => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            other => panic!("unexpected waiting state before end-step trigger: {other:?}"),
        }
    }
    panic!("phase machine did not reach the end-step trigger");
}

fn p1p1_counters(runner: &super::rules::GameRunner, id: ObjectId) -> u32 {
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
fn lathiel_end_step_distributes_counters_from_lifelink_gain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lathiel = scenario
        .add_creature_from_oracle(P0, "Lathiel, the Bounteous Dawn", 2, 2, LATHIEL_ORACLE)
        .id();
    let receiver = scenario.add_creature(P0, "Receiver A", 1, 1).id();
    let receiver2 = scenario.add_creature(P0, "Receiver B", 1, 1).id();
    scenario.add_creature(P1, "Blocker", 3, 3);

    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    run_combat(&mut runner, vec![lathiel], vec![]);

    assert_eq!(runner.life(P0), life_before + 2);
    assert_eq!(runner.state().players[0].life_gained_this_turn, 2);

    advance_to_end_step_trigger(&mut runner);

    // CR 601.2c + CR 115.3: "distribute ... among any number of OTHER target
    // creatures" is a single instance of "target", so each chosen creature must be
    // a DISTINCT object — the same receiver cannot fill two slots. Choose the two
    // distinct receivers, one per slot (slot 1's offered set excludes the creature
    // already chosen in slot 0).
    let receivers = [receiver, receiver2];
    let mut next = 0;
    let mut guard = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. }
    ) {
        guard += 1;
        assert!(guard < 10, "target selection did not terminate");
        let pick = receivers[next];
        next += 1;
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(pick)),
            })
            .expect("ChooseTarget should succeed");
    }
    assert_eq!(
        next, 2,
        "two distinct creatures targeted (slots capped to life gained = 2)"
    );

    match &runner.state().waiting_for {
        WaitingFor::DistributeAmong {
            total,
            unit: DistributionUnit::Counters(_),
            ..
        } => assert_eq!(*total, 2, "distribution pool must equal life gained"),
        other => panic!("expected DistributeAmong after targets, got {other:?}"),
    }

    // Split the pool of 2 across the two distinct targets, 1 each.
    runner
        .act(GameAction::DistributeAmong {
            distribution: vec![
                (TargetRef::Object(receiver), 1),
                (TargetRef::Object(receiver2), 1),
            ],
        })
        .expect("DistributeAmong should succeed");

    runner.advance_until_stack_empty();

    assert_eq!(
        p1p1_counters(&runner, receiver),
        1,
        "first distinct target receives 1 of the 2 +1/+1 counters"
    );
    assert_eq!(
        p1p1_counters(&runner, receiver2),
        1,
        "second distinct target receives the other +1/+1 counter (= life gained via lifelink)"
    );
}

#[test]
fn lathiel_target_selection_capped_to_life_gained_not_creature_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let lathiel = scenario
        .add_creature_from_oracle(P0, "Lathiel, the Bounteous Dawn", 2, 2, LATHIEL_ORACLE)
        .id();
    scenario.add_creature(P0, "Receiver A", 1, 1);
    scenario.add_creature(P0, "Receiver B", 1, 1);
    scenario.add_creature(P0, "Receiver C", 1, 1);
    scenario.add_creature(P1, "Blocker", 3, 3);

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![lathiel], vec![]);

    assert_eq!(runner.state().players[0].life_gained_this_turn, 2);

    advance_to_end_step_trigger(&mut runner);

    let slot_count = match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { target_slots, .. }
        | WaitingFor::TargetSelection { target_slots, .. } => target_slots.len(),
        other => panic!("expected target selection at end step, got {other:?}"),
    };
    assert_eq!(
        slot_count, 2,
        "must not offer more target slots than life gained (issue #1660 softlock)"
    );
}
