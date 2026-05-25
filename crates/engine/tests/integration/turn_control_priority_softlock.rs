//! Regression: priority passing must not soft-lock under a turn-control effect.
//!
//! CR 723 ("Controlling Another Player", e.g. Mindslaver) makes one player the
//! decision-maker for another's turn: per CR 723.5 the controller makes the
//! controlled player's choices, and per CR 723.8 still makes their own. The
//! engine models this by re-deriving `priority_player` (the *authorized
//! submitter*) from `waiting_for`, so during the controlled player's turn it
//! collapses onto the controller for BOTH seats.
//!
//! The bug: `handle_priority_pass` recorded the consecutive-pass set keyed on
//! `priority_player` (the submitter). Under turn-control that value never
//! changed between seats, so `priority_passes` could never reach all players —
//! CR 117.4's "all players pass in succession" was never satisfied, the stack
//! never resolved, and the phase never advanced. The game hung forever.
//!
//! Reproduced from a real Commander game (turn 23) where P0 cast Mindslaver to
//! control P1's turn; every PassPriority returned `Priority { player: P1 }` with
//! the stack frozen.

use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// With an empty stack, two consecutive passes (the controlled seat, then the
/// controller's own seat) must complete the round and advance the phase — not
/// loop on the controlled player's priority.
#[test]
fn turn_control_empty_stack_priority_advances_phase() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.build()
    };
    {
        let state = runner.state_mut();
        // CR 723: P0 controls P1's turn.
        state.active_player = P1;
        state.turn_decision_controller = Some(P0);
        state.priority_passes.clear();
        // P1 holds priority; sync re-derives priority_player to the submitter (P0).
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
    }
    assert_eq!(runner.state().phase, Phase::PreCombatMain);

    // First pass is the controlled seat (P1, submitted by P0). Priority must
    // move to the other seat, not stay on P1.
    apply_as_current(runner.state_mut(), GameAction::PassPriority).unwrap();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "after the controlled seat passes, priority must move to P0's seat, got {:?}",
        runner.state().waiting_for
    );

    // Second pass completes the round; the empty-stack phase must advance.
    apply_as_current(runner.state_mut(), GameAction::PassPriority).unwrap();
    assert_ne!(
        runner.state().phase,
        Phase::PreCombatMain,
        "two passes under turn-control must advance the phase, not soft-lock"
    );
}
