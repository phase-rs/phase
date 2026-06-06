//! Integration test for the Magus of the Abyss / The Abyss "of their choice"
//! trigger chooser deadlock.
//!
//! Magus of the Abyss: "At the beginning of each player's upkeep, destroy target
//! nonartifact creature that player controls of their choice. It can't be
//! regenerated." (CR 601.2c + CR 603.3d) The phase trigger fires at EVERY
//! player's upkeep, and "that player" (the active upkeep player) both controls
//! the target filter and announces the target at stack placement.
//!
//! Before the fix, `begin_pending_trigger_target_selection` routed target
//! selection to the trigger source's CONTROLLER. On an OPPONENT's upkeep that
//! meant the engine prompted Magus's controller to pick a creature the OPPONENT
//! controls — a misrouted decision that hangs the game (the opponent, whose
//! decision it is per CR 109.5, is never asked).
//!
//! This file drives the REAL trigger pipeline in a 2-player game: Magus is on
//! P0's battlefield, the turn advances into P1's (the opponent's) upkeep so the
//! engine emits the `Phase` trigger, `process_triggers` puts the ability on the
//! stack, and `begin_pending_trigger_target_selection` builds the
//! `WaitingFor::TriggerTargetSelection`. The test asserts the resulting
//! `player` is P1 (the upkeep player), NOT P0 (Magus's controller).
//!
//! Discriminating: pre-fix the engine yields P0 here and the assertion fails;
//! post-fix it yields P1.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Magus of the Abyss's printed Oracle text (the relevant phase trigger).
const MAGUS_ORACLE: &str = "At the beginning of each player's upkeep, destroy \
     target nonartifact creature that player controls of their choice. It can't \
     be regenerated.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

/// Drive the engine forward until it pauses on a trigger target selection (the
/// Magus trigger) or a terminal state, passing priority and declaring no
/// attackers/blockers so the turn rolls over from P0 into P1's upkeep.
fn advance_to_trigger_target_selection(runner: &mut GameRunner) {
    for _ in 0..240 {
        match &runner.state().waiting_for {
            WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
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
                    return;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// CR 601.2c + CR 603.3d + CR 109.5: on the OPPONENT's upkeep, Magus's targeted
/// "destroy target X that player controls of their choice" routes target
/// selection to the upkeep player (P1), not Magus's controller (P0).
#[test]
fn magus_targets_chosen_by_upkeep_player_not_controller() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Libraries so draw steps never deck anyone out before the assertion.
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    // Magus on P0's battlefield, parsed from real Oracle text (no DB load).
    scenario
        .add_creature_from_oracle(P0, "Magus of the Abyss", 5, 5, MAGUS_ORACLE)
        .id();
    // P1 (the opponent) controls TWO nonartifact creatures so the upkeep
    // trigger surfaces an interactive `TriggerTargetSelection` rather than
    // auto-resolving on a single legal target — that's the prompt whose
    // routing is under test.
    scenario.add_creature(P1, "P1 Beast A", 1, 1).id();
    scenario.add_creature(P1, "P1 Beast B", 2, 2).id();

    let mut runner = scenario.build();

    // Advance until the first trigger target selection surfaces. The first
    // upkeep reached after P0's pre-combat main is P1's upkeep (the opponent's
    // turn) — the deadlock case.
    advance_to_trigger_target_selection(&mut runner);

    let active = runner.state().active_player;
    let phase = runner.state().phase;
    // The first interactive trigger target selection is P1's upkeep (the
    // opponent's turn) — Magus only surfaces a prompt where the upkeep player
    // controls 2+ legal targets, and here that's P1.
    assert_eq!(
        active, P1,
        "expected the prompt at the opponent's (P1) upkeep; active {active:?}, phase {phase:?}"
    );
    assert_eq!(phase, Phase::Upkeep, "trigger should surface during upkeep");
    match &runner.state().waiting_for {
        WaitingFor::TriggerTargetSelection { player, .. } => {
            // CR 109.5: the upkeep player (P1) announces the target, NOT Magus's
            // controller (P0). Pre-fix the engine yields P0 here (the deadlock);
            // post-fix it yields P1.
            assert_eq!(
                *player, P1,
                "the upkeep player (P1) announces Magus's target, not the \
                 source's controller (P0)"
            );
        }
        other => panic!("expected TriggerTargetSelection at P1's upkeep, got {other:?}"),
    }
}
