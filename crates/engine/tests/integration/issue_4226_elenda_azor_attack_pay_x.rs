//! Regression for GitHub issue #4226: Elenda and Azor's attack trigger must not
//! cap pay-{X} at the number of attacking creatures.
//!
//! Oracle: "Whenever Elenda and Azor attacks, you may pay {X}{W}{U}{B}. If you do,
//! draw X cards."
//!
//! https://github.com/phase-rs/phase/issues/4226

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const ELENDA_ATTACK: &str =
    "Whenever Elenda and Azor attacks, you may pay {X}{W}{U}{B}. If you do, draw X cards.";

fn advance_to_pay_amount_or_panic(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional pay-{X}");
                continue;
            }
            WaitingFor::PayAmountChoice { .. } => return,
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                panic!(
                    "stack settled before PayAmountChoice; waiting_for={:?}",
                    runner.state().waiting_for
                );
            }
            _ => runner.pass_both_players(),
        }
    }
    panic!(
        "exhausted advance budget without PayAmountChoice; waiting_for={:?}",
        runner.state().waiting_for
    );
}

#[test]
fn issue_4226_elenda_attack_pay_x_not_capped_by_attacker_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let elenda = scenario
        .add_creature(P0, "Elenda and Azor", 6, 6)
        .from_oracle_text(ELENDA_ATTACK)
        .id();
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Black);
    for _ in 0..7 {
        scenario.add_basic_land(P0, ManaColor::White);
    }
    for i in 0..10 {
        scenario.add_spell_to_library_top(P0, &format!("Library Pad {i}"), false);
    }

    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(elenda, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("declare Elenda attacking");

    advance_to_pay_amount_or_panic(&mut runner);
    match &runner.state().waiting_for {
        WaitingFor::PayAmountChoice { max, .. } => {
            assert!(
                *max >= 3,
                "pay-{{X}} max must not be capped at attacker count (1); got {max} with \
                 ten untapped lands available for {{X}}{{W}}{{U}}{{B}}"
            );
        }
        other => panic!("expected PayAmountChoice after attack trigger, got {other:?}"),
    }

    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("submit X=3");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 3,
        "paying {{X}}{{W}}{{U}}{{B}} with X=3 must draw 3 cards"
    );
}
