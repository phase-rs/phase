//! Regression for issue #1318: Throne of the God-Pharaoh must make each opponent
//! lose life equal to the number of tapped creatures you control at your end step.
//!
//! Root cause was fixed in #1429 (original_controller during player_scope iteration).
//!
//! https://github.com/phase-rs/phase/issues/1318

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const THRONE_ORACLE: &str =
    "At the beginning of your end step, each opponent loses life equal to the number of tapped creatures you control.";

fn reach_end_step_and_resolve_stack(runner: &mut engine::game::scenario::GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack declaration should succeed");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => {
                if runner.state().stack.is_empty() {
                    runner.pass_both_players();
                } else {
                    runner.act(GameAction::PassPriority).ok();
                }
            }
            _ if runner.state().phase == Phase::End && runner.state().stack.is_empty() => return,
            _ => runner.pass_both_players(),
        }
    }
}

#[test]
fn issue_1318_throne_drains_opponents_for_tapped_creature_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Throne of the God-Pharaoh", 0, 0)
        .as_artifact()
        .from_oracle_text(THRONE_ORACLE);
    let c1 = scenario.add_creature(P0, "Bear1", 2, 2).id();
    let c2 = scenario.add_creature(P0, "Bear2", 2, 2).id();
    let c3 = scenario.add_creature(P0, "Bear3", 2, 2).id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&c1).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&c2).unwrap().tapped = true;
    runner.state_mut().objects.get_mut(&c3).unwrap().tapped = true;

    let opp_life_before = runner.state().players[P1.0 as usize].life;
    reach_end_step_and_resolve_stack(&mut runner);

    let opp_life_after = runner.state().players[P1.0 as usize].life;
    assert_eq!(
        opp_life_after,
        opp_life_before - 3,
        "with three tapped creatures, each opponent should lose 3 life at end step"
    );
}
