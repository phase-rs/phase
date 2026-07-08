use crate::types::game_state::{GameState, ScheduledTurnControl};
use crate::types::player::PlayerId;

/// CR 723.1 / CR 723.2 / CR 800.4a: the single authority that ENDS a
/// player-control effect. Removes the consumed schedule entry (the resolver
/// dedups to at most one per target — CR 723.1a) and clears
/// `turn_decision_controller` iff it currently points at that entry's
/// controller. Returns the removed entry so the caller can apply
/// window-specific post-processing (CR 723.1 extra-turn grant; CR 723.2 no-op).
/// All three release sites — turn boundary (`start_next_turn`), combat-phase
/// boundary (`finish_enter_phase`), and leave-game cleanup (`do_eliminate`) —
/// route through here so control ends in exactly one place.
pub(super) fn release_control_at(state: &mut GameState, idx: usize) -> ScheduledTurnControl {
    let entry = state.scheduled_turn_controls.remove(idx);
    if state.turn_decision_controller == Some(entry.controller) {
        state.turn_decision_controller = None;
    }
    entry
}

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
    let Some(controller) = state.turn_decision_controller else {
        return semantic_player;
    };

    // CR 723.5 + CR 805.8: A turn controller makes decisions for the
    // controlled player; in shared team turns, controlling one affected player
    // controls that player's team.
    let controlled_seat = if state.format_config.topology().has_shared_team_turns() {
        super::topology::team_members(state, state.active_player).contains(&semantic_player)
    } else {
        semantic_player == state.active_player
    };

    if controlled_seat {
        controller
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
/// (`MulliganDecision`, `OpeningHandBottomCards`) it is the full pending set.
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
