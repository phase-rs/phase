//! Regression for GitHub issue #1323: conditional end-step token generation must
//! fire when intervening-if conditions are met (Keeper of the Accord) and when
//! optional pay-life riders resolve (Elenda and Azor).
//!
//! https://github.com/phase-rs/phase/issues/1323

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

const KEEPER_ORACLE: &str = "At the beginning of each opponent's end step, if that player controls more creatures than you, create a 1/1 white Soldier creature token.\nAt the beginning of each opponent's end step, if that player controls more lands than you, you may search your library for a basic Plains card, put it onto the battlefield tapped, then shuffle.";

const ELENDA_END_STEP: &str = "At the beginning of each end step, you may pay 4 life. If you do, create a number of 1/1 black Vampire Knight creature tokens with lifelink equal to the number of cards you've drawn this turn.";

fn count_soldier_tokens(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == engine::types::zones::Zone::Battlefield
                && obj.is_token
                && obj.controller == P0
                && obj.name.eq_ignore_ascii_case("Soldier")
        })
        .count()
}

fn count_vampire_knight_tokens(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == engine::types::zones::Zone::Battlefield
                && obj.is_token
                && obj.controller == P0
                && obj.name.eq_ignore_ascii_case("Vampire Knight")
        })
        .count()
}

fn drive_end_step_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("declare attackers");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional effect");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .ok();
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => {
                if runner.state().stack.is_empty() {
                    return;
                }
                runner.act(GameAction::PassPriority).ok();
            }
            _ if runner.state().phase == Phase::End && runner.state().stack.is_empty() => return,
            _ => runner.pass_both_players(),
        }
    }
}

#[test]
fn issue_1323_keeper_creates_soldier_on_opponent_end_step_when_ahead_on_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Keeper of the Accord", 3, 4)
        .from_oracle_text(KEEPER_ORACLE);
    let _opp_a = scenario.add_creature(P1, "Opp A", 1, 1).id();
    let _opp_b = scenario.add_creature(P1, "Opp B", 1, 1).id();
    let _opp_c = scenario.add_creature(P1, "Opp C", 1, 1).id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_end_step();
    drive_end_step_stack(&mut runner);

    assert_eq!(
        count_soldier_tokens(&runner),
        1,
        "Keeper must create a Soldier when opponent controls more creatures at their end step"
    );
}

#[test]
fn issue_1323_elenda_creates_vampire_knights_equal_to_cards_drawn_after_pay_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature(P0, "Elenda and Azor", 6, 6)
        .from_oracle_text(ELENDA_END_STEP);

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize].cards_drawn_this_turn = 3;
    runner.state_mut().players[P0.0 as usize].life = 20;

    runner.advance_to_end_step();
    drive_end_step_stack(&mut runner);

    assert_eq!(
        count_vampire_knight_tokens(&runner),
        3,
        "Elenda must create one Vampire Knight per card drawn this turn after paying 4 life"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        16,
        "accepting Elenda's end-step rider must pay 4 life"
    );
}
