use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

pub fn turn_resource_owner(state: &GameState) -> PlayerId {
    state.active_player
}

pub fn turn_decision_maker(state: &GameState) -> PlayerId {
    state
        .turn_decision_controller
        .unwrap_or(state.active_player)
}

/// CR 117 + CR 723: The player who currently *holds* priority — the semantic
/// seat — as opposed to `state.priority_player`, which is the authorized
/// submitter. Under a turn-control effect (CR 723, e.g. Mindslaver) these
/// differ: `priority_player` collapses onto the controller for every seat the
/// controller submits for, so any rules check that means "who holds priority"
/// must use this, not the raw field. Sourced from `waiting_for`, falling back to
/// `priority_player` for states that carry no single acting player.
pub fn priority_seat(state: &GameState) -> PlayerId {
    state
        .waiting_for
        .acting_player()
        .unwrap_or(state.priority_player)
}

pub fn authorized_submitter_for_player(state: &GameState, semantic_player: PlayerId) -> PlayerId {
    if semantic_player == state.active_player {
        turn_decision_maker(state)
    } else {
        semantic_player
    }
}

pub fn authorized_submitter(state: &GameState) -> Option<PlayerId> {
    state
        .waiting_for
        .acting_player()
        .map(|player| authorized_submitter_for_player(state, player))
}

/// CR 103.5: Set-aware authorization. Returns every PlayerId who is currently
/// allowed to submit an action for `state.waiting_for`. For single-player
/// states this is a one-element Vec; for simultaneous-decision states
/// (`MulliganDecision`, `MulliganBottomCards`, `OpeningHandBottomCards`) it is the full pending set.
/// Each entry is mapped through `authorized_submitter_for_player` so that
/// turn-decision-controller effects (e.g., Mindslaver) still re-route the
/// submitter correctly.
pub fn authorized_submitters(state: &GameState) -> Vec<PlayerId> {
    state
        .waiting_for
        .acting_players()
        .into_iter()
        .map(|player| authorized_submitter_for_player(state, player))
        .collect()
}

/// CR 103.5: True iff `actor` is one of the authorized submitters for the
/// current `WaitingFor`. Use this in `check_actor_authorization` so the
/// simultaneous mulligan variants accept any pending player.
pub fn is_authorized_submitter(state: &GameState, actor: PlayerId) -> bool {
    authorized_submitters(state).contains(&actor)
}

pub fn viewer_controls_active_turn(state: &GameState, viewer: PlayerId) -> bool {
    state.turn_decision_controller == Some(viewer)
}
