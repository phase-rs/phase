//! Issue #828 — Full Throttle must schedule extra combat phases after the main phase.
//!
//! https://github.com/phase-rs/phase/issues/828

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const FULL_THROTTLE: &str = "After this main phase, there are two additional combat phases.
At the beginning of each combat this turn, untap all creatures that attacked this turn.";

#[test]
fn full_throttle_schedules_two_extra_combats_after_main_phase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let throttle = scenario
        .add_spell_to_hand_from_oracle(P0, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.cast(throttle).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().extra_phases.len(),
        2,
        "Full Throttle must schedule two extra combat phases after the current main phase"
    );
    assert_eq!(
        runner.state().extra_phases[0],
        engine::types::game_state::ExtraPhase {
            anchor: Phase::PreCombatMain,
            phase: Phase::BeginCombat,
            attacker_restriction: None,
            attacker_restriction_source: None,
        }
    );
    assert_eq!(
        runner.state().extra_phases[1],
        engine::types::game_state::ExtraPhase {
            anchor: Phase::EndCombat,
            phase: Phase::BeginCombat,
            attacker_restriction: None,
            attacker_restriction_source: None,
        },
        "the second extra combat must chain after the first combat ends"
    );
}

#[test]
fn full_throttle_postcombat_main_anchors_to_postcombat_main() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let throttle = scenario
        .add_spell_to_hand_from_oracle(P0, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.cast(throttle).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().extra_phases.len(), 2);
    assert_eq!(
        runner.state().extra_phases[0].anchor,
        Phase::PostCombatMain,
        "casting in postcombat main must anchor the first extra combat to postcombat main"
    );
    assert_eq!(
        runner.state().extra_phases[1].anchor,
        Phase::EndCombat,
        "the second extra combat must chain after end of combat"
    );
}

#[test]
fn full_throttle_turn_advances_through_two_extra_combats() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _attacker = scenario.add_vanilla(P0, 1, 1);
    let throttle = scenario
        .add_spell_to_hand_from_oracle(P0, "Full Throttle", false, FULL_THROTTLE)
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    runner.cast(throttle).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().extra_phases.len(), 2);

    let mut declare_attackers_rounds = 0;
    let mut finished = false;
    for _ in 0..150 {
        if runner.state().phase == Phase::PostCombatMain && runner.state().extra_phases.is_empty() {
            finished = true;
            break;
        }

        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                declare_attackers_rounds += 1;
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare empty attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare empty blockers");
            }
            WaitingFor::Priority { .. } => {
                runner.pass_both_players();
            }
            _ => {
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }

    assert!(
        finished,
        "turn must reach postcombat main with all extra combats consumed; phase={:?} extra_phases={:?} declare_attackers_rounds={declare_attackers_rounds}",
        runner.state().phase,
        runner.state().extra_phases
    );
    assert_eq!(
        declare_attackers_rounds, 2,
        "Full Throttle must produce two reachable extra combat phases"
    );
}
