//! Issue #5640 — Shadrix Silverquill: the begin-combat triggered modal
//! ("you may choose two") must offer a decline for the entire ability and, when
//! accepted, require exactly two modes targeting different players.
//!
//! Parser-shape assertions alone cannot prove runtime behavior: this file drives
//! the real begin-combat pipeline and exercises both the decline and accept
//! branches.
//!
//! CR 508.1 (begin-combat step) + CR 603.2 (triggered abilities on the stack) +
//! CR 608.2c (optional triggered effects) + CR 700.2b (modal choice).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SHADRIX_ORACLE: &str = "Flying, double strike\n\
At the beginning of combat on your turn, you may choose two. Each mode must target a different player.\n\
• Target player creates a 2/1 white and black Inkling creature token with flying.\n\
• Target player draws a card and loses 1 life.\n\
• Target player puts a +1/+1 counter on each creature they control.";

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn hand_len(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn inkling_tokens_for(runner: &GameRunner, controller: PlayerId) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == controller
                && o.zone == Zone::Battlefield
                && o.is_token
                && o.power == Some(2)
                && o.toughness == Some(1)
        })
        .map(|o| o.id)
        .collect()
}

fn seed_library(scenario: &mut GameScenario, n: usize) {
    for i in 0..n {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }
}

fn shadrix_board(library_cards: usize) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Shadrix Silverquill", 2, 5, SHADRIX_ORACLE);
    seed_library(&mut scenario, library_cards);
    scenario.build()
}

/// Advance from PreCombatMain through the begin-combat step so Shadrix's trigger
/// fires and reaches the stack.
fn fire_begin_combat_trigger(runner: &mut GameRunner) {
    runner.pass_both_players();
    assert_eq!(
        runner.state().phase,
        Phase::BeginCombat,
        "Shadrix regression must fire from a genuine begin-combat step"
    );
}

/// Bounded drive loop for Shadrix's optional triggered modal resolution.
fn drive_shadrix(
    runner: &mut GameRunner,
    accept_optional: bool,
    modes: &[usize],
    mode_targets: &[PlayerId],
) {
    let mut target_idx = 0usize;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return;
                }
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: accept_optional,
                    })
                    .expect("Shadrix optional begin-combat trigger must offer decline");
            }
            WaitingFor::AbilityModeChoice { .. } => {
                assert!(
                    accept_optional,
                    "declining Shadrix must not reach AbilityModeChoice"
                );
                runner
                    .act(GameAction::SelectModes {
                        indices: modes.to_vec(),
                    })
                    .expect("choosing Shadrix modes must succeed");
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                let player = mode_targets[target_idx];
                target_idx += 1;
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Player(player)),
                    })
                    .expect("selecting a Shadrix mode target must succeed");
            }
            other => panic!("unexpected waiting state during Shadrix resolution: {other:?}"),
        }
    }
    panic!("Shadrix resolution did not settle after 80 iterations — likely a stall");
}

#[test]
fn shadrix_begin_combat_declining_optional_skips_all_modes() {
    let mut runner = shadrix_board(4);

    let p0_life_before = life(&runner, P0);
    let p1_life_before = life(&runner, P1);
    let p0_hand_before = hand_len(&runner, P0);
    let p1_hand_before = hand_len(&runner, P1);

    fire_begin_combat_trigger(&mut runner);
    drive_shadrix(&mut runner, false, &[], &[]);

    assert!(
        inkling_tokens_for(&runner, P0).is_empty() && inkling_tokens_for(&runner, P1).is_empty(),
        "declining Shadrix must create no Inkling tokens"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before,
        "declining Shadrix must draw no cards for P0"
    );
    assert_eq!(
        hand_len(&runner, P1),
        p1_hand_before,
        "declining Shadrix must draw no cards for P1"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life_before,
        "declining Shadrix must change no life totals"
    );
    assert_eq!(
        life(&runner, P1),
        p1_life_before,
        "declining Shadrix must change no life totals"
    );
}

#[test]
fn shadrix_begin_combat_accepting_requires_exactly_two_modes() {
    let mut runner = shadrix_board(4);
    fire_begin_combat_trigger(&mut runner);

    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).ok();
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accepting Shadrix optional trigger must succeed");
            }
            WaitingFor::AbilityModeChoice { .. } => {
                let err = runner.act(GameAction::SelectModes { indices: vec![0] });
                assert!(
                    err.is_err(),
                    "Shadrix must reject choosing only one mode when min_choices is 2"
                );
                return;
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            other => panic!("expected AbilityModeChoice after accepting Shadrix, got {other:?}"),
        }
    }
    panic!("never reached AbilityModeChoice while accepting Shadrix");
}

#[test]
fn shadrix_begin_combat_accepting_two_modes_resolves_both() {
    let mut runner = shadrix_board(4);

    let p0_life_before = life(&runner, P0);
    let p0_hand_before = hand_len(&runner, P0);

    fire_begin_combat_trigger(&mut runner);
    // Mode 0: Inkling for P1. Mode 1: draw + lose 1 life for P0.
    drive_shadrix(&mut runner, true, &[0, 1], &[P1, P0]);

    assert_eq!(
        inkling_tokens_for(&runner, P1).len(),
        1,
        "mode 0 must create exactly one Inkling token for the chosen player"
    );
    assert!(
        inkling_tokens_for(&runner, P0).is_empty(),
        "mode 0 targeted P1, so P0 must receive no Inkling"
    );
    assert_eq!(
        hand_len(&runner, P0),
        p0_hand_before + 1,
        "mode 1 must draw P0 a card"
    );
    assert_eq!(
        life(&runner, P0),
        p0_life_before - 1,
        "mode 1 must make P0 lose 1 life"
    );
}
