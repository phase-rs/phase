//! Regression for issue #1996: Rot Wolf must draw when a creature it damaged dies.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::rules::run_combat;
use crate::support::shared_card_db as load_db;

#[test]
fn rot_wolf_draws_when_infect_kill_dies() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rot_wolf = scenario.add_real_card(P0, "Rot Wolf", Zone::Battlefield, db);
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.add_real_card(P0, "Forest", Zone::Library, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let hand_before = runner.state().players[0].hand.len();

    run_combat(&mut runner, vec![rot_wolf], vec![(victim, rot_wolf)]);

    assert!(
        !runner.state().stack.is_empty(),
        "Rot Wolf draw trigger must reach the stack after a combat trade"
    );

    let mut accepted_optional = false;
    for _ in 0..24 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional draw");
                accepted_optional = true;
            }
            WaitingFor::OrderTriggers { .. } => {
                runner.advance_until_stack_empty();
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            _ => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).ok();
            }
        }
    }

    assert!(accepted_optional, "Rot Wolf optional draw must prompt");
    assert!(
        runner.state().players[0].hand.len() > hand_before,
        "accepting Rot Wolf's draw must increase hand size"
    );
}
