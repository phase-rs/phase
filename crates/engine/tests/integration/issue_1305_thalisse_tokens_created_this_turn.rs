//! Regression for issue #1305: Thalisse, Reverent Medium must create Spirits
//! equal to tokens its controller created this turn.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const THALISSE: &str = "At the beginning of each end step, create X 1/1 white \
Spirit creature tokens with flying, where X is the number of tokens you created this turn.";

fn count_spirit_tokens(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == engine::types::zones::Zone::Battlefield
                && obj.is_token
                && obj.name.eq_ignore_ascii_case("Spirit")
        })
        .count()
}

#[test]
fn issue_1305_thalisse_creates_spirits_for_tokens_created_this_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let _thalisse = scenario
        .add_creature_from_oracle(P0, "Thalisse, Reverent Medium", 3, 4, THALISSE)
        .id();

    let token_a = scenario.add_creature(P0, "Clue", 0, 0).id();
    let token_b = scenario.add_creature(P0, "Food", 0, 0).id();

    let mut runner = scenario.build();
    for id in [token_a, token_b] {
        let obj = runner.state_mut().objects.get_mut(&id).unwrap();
        obj.is_token = true;
        engine::game::restrictions::record_token_created(runner.state_mut(), id);
    }

    for _ in 0..200 {
        if runner.state().phase == Phase::End
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        match &runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("declare blockers");
            }
            other => panic!("unexpected waiting state: {other:?}"),
        }
    }

    assert_eq!(
        count_spirit_tokens(&runner),
        2,
        "end step should create one Spirit per token created this turn"
    );
}
