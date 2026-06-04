//! Regression (issue #2012): under a turn-control effect, the controller must
//! receive the controlled player's legal actions — not an empty set.
//!
//! CR 723 ("Controlling Another Player"): Emrakul, the Promised End grants its
//! caster control of a target opponent during that player's next turn. Per
//! CR 723.3 the controlled player is still the active player, so the engine's
//! `WaitingFor` reports the controlled seat as the acting player. Per CR 723.5
//! the controller makes all of the controlled player's choices.
//!
//! `legal_actions_for_viewer` is the per-viewer authority the P2P/WASM transport
//! broadcasts to each seat. It previously gated on
//! `acting_players().contains(&viewer)`, which is the controlled seat — so the
//! controller (the player who must actually act) received an empty action set
//! and the controlled turn froze for them ("crashes my game when I go to the
//! controlled player's turn"). The fix authorizes the viewer through
//! `is_authorized_submitter`, which maps each acting seat to its authorized
//! submitter.

use engine::ai_support::{legal_actions_for_viewer, legal_actions_full};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;

/// During P1's controlled turn (P0 is the controller), the controller P0 — the
/// authorized submitter — must receive the controlled turn's full legal-action
/// set, and the controlled seat P1 (not the submitter) must receive none.
#[test]
fn controller_receives_controlled_turns_legal_actions() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // Give the controlled player (P1) a land to play so the action set is
        // non-trivially populated.
        scenario.add_land_to_hand(P1, "Forest");
        scenario.build()
    };
    {
        let state = runner.state_mut();
        // CR 723.3: the controlled player (P1) is still the active player.
        state.active_player = P1;
        // CR 723: P0 controls P1's turn.
        state.turn_decision_controller = Some(P0);
        // P1 holds priority; sync re-derives the authorized submitter (P0).
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
    }

    // Baseline: the unfiltered engine view has actions to offer this turn.
    let full = legal_actions_full(runner.state());
    assert!(
        !full.0.is_empty(),
        "precondition: the controlled turn should have legal actions to offer"
    );

    // CR 723.5: the controller (authorized submitter) sees the controlled
    // turn's full action set.
    let (controller_actions, controller_costs, controller_by_object) =
        legal_actions_for_viewer(runner.state(), P0);
    assert_eq!(
        controller_actions, full.0,
        "CR 723.5: the controller must receive the controlled player's legal actions"
    );
    assert_eq!(controller_costs, full.1);
    assert_eq!(controller_by_object, full.2);

    // The controlled seat (P1) is not the authorized submitter under turn
    // control, so it receives no actions of its own.
    let (controlled_actions, controlled_costs, controlled_by_object) =
        legal_actions_for_viewer(runner.state(), P1);
    assert!(
        controlled_actions.is_empty(),
        "the controlled seat is not the authorized submitter and must receive no actions"
    );
    assert!(controlled_costs.is_empty());
    assert!(controlled_by_object.is_empty());
}

/// Without any turn-control effect, `legal_actions_for_viewer` is unchanged: the
/// acting player sees the full set and the other player sees none.
#[test]
fn no_turn_control_preserves_viewer_gating() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_land_to_hand(P0, "Forest");
        scenario.build()
    };
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.turn_decision_controller = None;
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }

    let full = legal_actions_full(runner.state());
    let (acting, _, _) = legal_actions_for_viewer(runner.state(), P0);
    assert_eq!(acting, full.0, "acting player sees the full set");

    let (other, _, _) = legal_actions_for_viewer(runner.state(), P1);
    assert!(other.is_empty(), "non-acting player sees no actions");
}
