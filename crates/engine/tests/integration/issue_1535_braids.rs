//! Issue #1535: Braids, Conjurer Adept — on each player's upkeep, that player
//! (not Braids's controller) must receive the optional "put from hand" prompt.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const BRAIDS_ORACLE: &str = "At the beginning of each player's upkeep, that player may put an artifact, creature, or land card from their hand onto the battlefield.";

fn advance_to_optional_on_upkeep(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).ok();
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .ok();
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .ok();
            }
            _ => return,
        }
    }
}

#[test]
fn braids_optional_prompt_routes_to_upkeep_player_not_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    scenario.add_creature_from_oracle(P0, "Braids, Conjurer Adept", 2, 2, BRAIDS_ORACLE);
    scenario.add_creature_to_hand(P1, "Grizzly Bears", 2, 2);

    let mut runner = scenario.build();
    advance_to_optional_on_upkeep(&mut runner);

    assert_eq!(
        runner.state().active_player,
        P1,
        "first interactive Braids prompt should be on opponent upkeep"
    );
    assert_eq!(runner.state().phase, Phase::Upkeep);

    match &runner.state().waiting_for {
        WaitingFor::OptionalEffectChoice { player, .. } => {
            assert_eq!(
                *player, P1,
                "Braids must prompt the upkeep player, not Braids's controller"
            );
        }
        other => panic!("expected OptionalEffectChoice at P1 upkeep, got {other:?}"),
    }
}
