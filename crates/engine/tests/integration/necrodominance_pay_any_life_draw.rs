//! Necrodominance — "At the beginning of your end step, you may pay any amount
//! of life. If you do, draw that many cards."
//!
//! Exercises the new `PayableResource::Life` interactive subsystem end-to-end:
//! the end-step trigger fires, the controller accepts the optional "you may",
//! commits an amount via `SubmitPayAmount`, the life-loss authority deducts that
//! many life, and the `EventContextAmount`-counted Draw rider draws that many
//! cards.
//!
//! Discriminating: starting at P0's pre-combat main, the next end step reached
//! is P0's own. The controller pays 3 life and must draw exactly 3 cards.
//! Pre-fix, "pay any amount of life" lowered to `Effect::Unimplemented`, so no
//! prompt surfaced, no life was lost, and the IfYouDo Draw read 0.

use engine::game::scenario::GameScenario;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

// Only the pay-life trigger clause — the "no maximum hand size" / draw-replacement
// clauses are orthogonal to the feature under test and kept out of the scenario.
const ORACLE: &str = "At the beginning of your end step, you may pay any amount of life. \
                      If you do, draw that many cards.";

fn hand_size(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

#[test]
fn necrodominance_pay_any_life_draws_that_many() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    // Stock libraries so the end step / draw steps never deck anyone out and so
    // there are cards to draw when the trigger resolves.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D", "Lib E"]);
    }
    // Necrodominance on P0's battlefield, parsed from real Oracle text. Card type
    // is irrelevant to the end-step trigger, so a minimal body suffices.
    scenario.add_creature_from_oracle(P0, "Necrodominance", 0, 1, ORACLE);

    let mut runner = scenario.build();
    let life_before = runner.life(P0);
    let hand_before = hand_size(&runner, P0);

    // Roll forward into P0's end step, answering the trigger: accept the optional
    // "you may" then pay 3 life. Pass priority elsewhere and declare no
    // attackers/blockers so combat never stalls.
    let mut drew = false;
    for _ in 0..400 {
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { player, .. } => {
                let player = *player;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional pay-life");
                assert_eq!(player, P0, "the optional belongs to the controller");
            }
            WaitingFor::PayAmountChoice { player, max, .. } => {
                assert_eq!(*player, P0);
                assert!(*max >= 3, "P0 has ≥3 life to pay, got max {max}");
                runner
                    .act(GameAction::SubmitPayAmount { amount: 3 })
                    .expect("submit 3 life");
                drew = true;
                // Let the IfYouDo Draw rider resolve, then stop.
                let _ = runner.act(GameAction::PassPriority);
                break;
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }

    assert!(drew, "the end-step trigger must surface a PayAmountChoice");
    assert_eq!(
        runner.life(P0),
        life_before - 3,
        "paying 3 life via the life-loss authority deducts 3 life"
    );
    assert_eq!(
        hand_size(&runner, P0),
        hand_before + 3,
        "the IfYouDo Draw must draw the chosen amount (3 cards)"
    );
}
