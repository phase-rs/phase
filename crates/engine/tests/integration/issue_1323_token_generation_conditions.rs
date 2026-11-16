//! Regression for GitHub issue #1323: conditional end-step token generation must
//! fire when intervening-if conditions are met (Keeper of the Accord) and when
//! optional pay-life riders resolve (Elenda and Azor).
//!
//! https://github.com/phase-rs/phase/issues/1323

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::format::FormatConfig;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

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

/// 2HG Keeper board on team B's shared turn: P0 controls Keeper plus
/// `p0_extra` extra creatures; P2/P3 hold `p2_n`/`p3_n` creatures each. No
/// lands anywhere, so Keeper's second (Plains-search) trigger stays silent
/// and only the creature trigger is under test. Parked at PostCombatMain with
/// priority; the caller advances into the end step through the production
/// pipeline (the P-F2a driver).
fn keeper_shared_board(p0_extra: usize, p2_n: usize, p3_n: usize) -> GameRunner {
    let mut scenario = GameScenario::new_with_format(FormatConfig::two_headed_giant(), 4, 42);
    scenario.at_phase(Phase::PostCombatMain);
    scenario
        .add_creature(P0, "Keeper of the Accord", 3, 4)
        .from_oracle_text(KEEPER_ORACLE);
    for i in 0..p0_extra {
        scenario.add_creature(P0, &format!("Loyal {i}"), 1, 1);
    }
    for i in 0..p2_n {
        scenario.add_creature(P2, &format!("P2 Ally {i}"), 1, 1);
    }
    for i in 0..p3_n {
        scenario.add_creature(P3, &format!("P3 Ally {i}"), 1, 1);
    }
    let mut runner = scenario.build();
    runner.state_mut().active_player = P2;
    runner.state_mut().priority_player = P2;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P2 };
    runner
}

/// Drain the end step until the stack settles, failing loudly on ANY prompt.
/// Keeper's creature trigger is mandatory and the lands trigger is silent on
/// this board, so nothing may prompt.
fn drive_end_step_to_settle(runner: &mut GameRunner) {
    for _ in 0..100 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::OrderTriggers { .. } => {
                panic!(
                    "single qualifying seat must not prompt for ordering, got {:?}",
                    runner.state().waiting_for
                );
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                panic!(
                    "no optional prompt may surface on this board, got {:?}",
                    runner.state().waiting_for
                );
            }
            _ => {
                runner.pass_both_players();
            }
        }
    }
    panic!("the end step did not settle within the budget");
}

/// K1 delivery (CR 805.4d): on team B's shared end step only the qualifying
/// nonrepresentative seat fires. P0 holds Keeper + 1 (2 creatures); P2 holds
/// 1 (behind); P3 holds 3 (ahead) — exactly one Soldier, with no ordering
/// prompt (single-firing proof).
///
/// Was red until the F2 condition arm landed: the fan-out builder used to
/// read only the effect tree, so Keeper never fanned out on the shared
/// turn — no firing, no token — until condition-scoped turn-player
/// references were admitted.
#[test]
fn issue_1323_keeper_shared_end_step_fires_only_for_qualifying_seat() {
    let mut runner = keeper_shared_board(1, 1, 3);
    runner.advance_to_end_step();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach-guard: the runner must reach the end step, else the firing \
         below never happens"
    );
    drive_end_step_to_settle(&mut runner);
    assert_eq!(
        count_soldier_tokens(&runner),
        1,
        "exactly the qualifying seat's firing creates a Soldier"
    );
}

/// K3 (CR 603.4 resolution recheck): both seats qualify at trigger time
/// (P0 holds 2; P2 and P3 hold 3 each), so two firings order and stack. The
/// first resolution grants the controller a Soldier (2 → 3), tying the other
/// seat — the second firing's kept-condition recheck against its stored
/// occurrence event then fails and creates no token. Exactly one Soldier
/// total.
///
/// Seat identity is unobservable at the `OrderTriggers` prompt (both
/// summaries describe the same Keeper object), so identity order is used:
/// the board is symmetric, making the verdict order-independent while still
/// exercising trigger-time-true → resolution-time-false for the second
/// firing. LIFO stacking resolves one firing fully before the other either
/// way.
///
/// REVERT-FAIL (kept condition dropped): two tokens (CR 603.4 violation).
/// Was red until the F2 condition arm landed (same builder-gate cause as the
/// delivery test): the ordering prompt never surfaced until condition-scoped
/// references were admitted.
#[test]
fn issue_1323_keeper_shared_end_step_rechecks_condition_at_resolution() {
    let mut runner = keeper_shared_board(1, 3, 3);
    runner.advance_to_end_step();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach-guard: the runner must reach the end step, else the firings \
         below never happen"
    );
    match runner.state().waiting_for.clone() {
        WaitingFor::OrderTriggers { triggers, .. } => {
            assert_eq!(
                triggers.len(),
                2,
                "both seats' firings await ordering, got {}",
                triggers.len()
            );
            runner
                .act(GameAction::OrderTriggers { order: vec![0, 1] })
                .expect("trigger order");
        }
        other => panic!("expected both seats' firings to await ordering, got {other:?}"),
    }
    drive_end_step_to_settle(&mut runner);
    assert_eq!(
        count_soldier_tokens(&runner),
        1,
        "only the first resolution creates a token; the second rechecks \
         parity at resolution (CR 603.4) and creates none"
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
