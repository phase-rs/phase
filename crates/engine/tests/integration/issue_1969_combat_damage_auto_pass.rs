//! Issue #1969 - combat damage must be dealt even when the active player is in an
//! UntilEndOfTurn auto-pass session ("pass to end step" must not skip damage).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{AutoPassRequest, WaitingFor};
use engine::types::phase::Phase;

#[test]
fn until_end_of_turn_auto_pass_still_deals_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker_id = scenario.add_creature(P0, "Bear", 3, 3).id();
    let mut runner = scenario.build();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker_id, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare attackers");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "active player receives priority after attackers"
    );
    runner.pass_both_players();

    match &runner.state().waiting_for {
        WaitingFor::DeclareBlockers { .. } => {
            runner
                .act(GameAction::DeclareBlockers {
                    assignments: vec![],
                })
                .expect("declare no blockers");
        }
        WaitingFor::Priority { .. } => {
            assert!(
                matches!(
                    runner.state().phase,
                    Phase::DeclareBlockers | Phase::CombatDamage
                ),
                "priority after attackers must be in declare blockers or combat damage"
            );
        }
        other => panic!(
            "expected DeclareBlockers or Priority after attackers, got {other:?} (phase={:?})",
            runner.state().phase
        ),
    }

    runner
        .act(GameAction::SetAutoPass {
            mode: AutoPassRequest::UntilEndOfTurn,
        })
        .expect("enable pass-to-end");

    for _ in 0..40 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority during auto-pass drain");
            }
            other => panic!(
                "unexpected waiting state while draining combat: {other:?} (phase={:?})",
                runner.state().phase
            ),
        }
    }

    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "auto-pass must advance through combat damage to postcombat main"
    );
    assert_eq!(
        runner.life(P1),
        17,
        "defender must take 3 combat damage even under UntilEndOfTurn auto-pass; \
         waiting_for={:?}",
        runner.state().waiting_for
    );
}
