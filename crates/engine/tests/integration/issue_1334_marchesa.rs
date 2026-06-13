//! Regression for GitHub issue #1334 — Marchesa, the Black Rose must return
//! creatures with +1/+1 counters at the beginning of the next end step.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{DelayedTriggerCondition, Effect};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MARCHESA_ORACLE: &str = "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\n\
Other creatures you control have dethrone.\n\
Whenever a creature you control with a +1/+1 counter on it dies, return that card to the battlefield under your control at the beginning of the next end step.";

fn drain_delayed_triggers(runner: &mut engine::game::scenario::GameRunner) {
    let mut guard = 0;
    while !runner.state().delayed_triggers.is_empty() || !runner.state().stack.is_empty() {
        guard += 1;
        assert!(
            guard < 256,
            "timed out draining delayed triggers; phase={:?} waiting_for={:?} dt={} stack={}",
            runner.state().phase,
            runner.state().waiting_for,
            runner.state().delayed_triggers.len(),
            runner.state().stack.len(),
        );
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare no attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare no blockers");
            }
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
}

#[test]
fn marchesa_returns_creature_with_plus_one_at_next_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _marchesa_id = scenario
        .add_creature_from_oracle(P0, "Marchesa, the Black Rose", 3, 3, MARCHESA_ORACLE)
        .id();

    let bear_id = scenario
        .add_creature(P0, "Grizzly Bear", 2, 2)
        .with_plus_counters(1)
        .id();

    let mut runner = scenario.build();

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), bear_id, Zone::Graveyard, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);

    // Resolve Marchesa's dies trigger onto the stack before checking delayed registration.
    let mut guard = 0;
    while !runner.state().stack.is_empty() {
        guard += 1;
        assert!(guard < 64, "stack drain exceeded bound");
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority resolving dies trigger");
            }
            other => panic!("unexpected wait while resolving dies trigger: {other:?}"),
        }
    }

    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "dying +1/+1 creature must register a delayed return trigger"
    );
    assert!(matches!(
        runner.state().delayed_triggers[0].condition,
        DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
    ));
    assert!(matches!(
        runner.state().delayed_triggers[0].ability.effect,
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            ..
        }
    ));

    runner.advance_to_end_step();
    drain_delayed_triggers(&mut runner);

    assert_eq!(
        runner.state().objects[&bear_id].zone,
        Zone::Battlefield,
        "creature that died with a +1/+1 counter must return at beginning of next end step"
    );
}
