//! Issue #2868 — Charging Cinderhorn must gate its end-step trigger on the
//! intervening-if "if no creatures attacked this turn" (CR 603.4).
//!
//! Oracle:
//!   Haste
//!   At the beginning of each player's end step, if no creatures attacked this
//!   turn, put a fury counter on this creature. Then this creature deals damage
//!   equal to the number of fury counters on it to that player.

use super::rules::{run_combat, GameRunner, GameScenario, Phase, WaitingFor, P0};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;

const CHARGING_CINDERHORN: &str = "Haste\nAt the beginning of each player's end step, if no creatures attacked this turn, put a fury counter on this creature. Then this creature deals damage equal to the number of fury counters on it to that player.";

fn fury_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("cinderhorn on battlefield")
        .counters
        .get(&CounterType::Generic("fury".to_string()))
        .copied()
        .unwrap_or(0)
}

/// Walk the turn structure to the active player's end step, auto-declaring no
/// attackers when combat opens.
fn advance_to_end_step(runner: &mut GameRunner) {
    for _ in 0..200 {
        if runner.state().phase == Phase::End {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while advancing to end step");
            }
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
            other => panic!(
                "unexpected prompt advancing to end step: {other:?} (phase={:?})",
                runner.state().phase
            ),
        }
    }
    panic!("failed to reach end step within 200 steps");
}

/// Resolve any end-step triggers on the stack, then return once priority is open
/// in the end step with an empty stack.
fn resolve_end_step_triggers(runner: &mut GameRunner) {
    for _ in 0..200 {
        if runner.state().phase == Phase::End
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while resolving end-step triggers");
            }
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
            other => panic!(
                "unexpected prompt resolving end-step triggers: {other:?} \
                 (phase={:?}, stack={})",
                runner.state().phase,
                runner.state().stack.len()
            ),
        }
    }
    panic!("end-step triggers did not resolve within 200 steps");
}

#[test]
fn charging_cinderhorn_adds_fury_counter_when_no_creature_attacked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cinderhorn = scenario
        .add_creature_from_oracle(P0, "Charging Cinderhorn", 4, 2, CHARGING_CINDERHORN)
        .id();

    let mut runner = scenario.build();

    advance_to_end_step(&mut runner);
    resolve_end_step_triggers(&mut runner);

    assert_eq!(
        fury_counters(&runner, cinderhorn),
        1,
        "must add a fury counter when no creature attacked this turn (CR 603.4)"
    );
}

#[test]
fn charging_cinderhorn_skips_trigger_when_a_creature_attacked() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let cinderhorn = scenario
        .add_creature_from_oracle(P0, "Charging Cinderhorn", 4, 2, CHARGING_CINDERHORN)
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();

    let mut runner = scenario.build();
    let life_before = runner.life(P0);

    run_combat(&mut runner, vec![attacker], vec![]);

    advance_to_end_step(&mut runner);
    resolve_end_step_triggers(&mut runner);

    assert_eq!(
        fury_counters(&runner, cinderhorn),
        0,
        "must not add a fury counter when a creature attacked this turn (issue #2868)"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "must not deal end-step damage when the intervening-if fails"
    );
}
