use crate::types::game_state::{
    ActiveSearchDecisionAuthority, GameState, ScheduledTurnControl, WaitingFor,
};
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

fn effective_authority_for_player(state: &GameState, semantic_player: PlayerId) -> PlayerId {
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

/// CR 723.5: The controller of a searching player makes that player's
/// search-related choices while the latched search-control authority applies.
fn search_decision_authority(
    state: &GameState,
    semantic_player: PlayerId,
) -> Option<ActiveSearchDecisionAuthority> {
    if matches!(
        state.waiting_for,
        WaitingFor::OptionalEffectChoice { player, .. } if player == semantic_player
    ) {
        if let Some(authority) = state
            .pending_scoped_library_search
            .as_ref()
            .and_then(|pending| match &pending.phase {
                crate::types::game_state::ScopedLibrarySearchPhase::CollectAcceptance {
                    acceptance_authorities,
                    ..
                } => acceptance_authorities
                    .iter()
                    .find(|(player, _)| *player == semantic_player)
                    .map(|(_, authority)| *authority),
                _ => None,
            })
        {
            return Some(authority);
        }
    }
    let eligible = match &state.waiting_for {
        WaitingFor::SearchChoice { player, .. } => *player == semantic_player,
        WaitingFor::ReplacementChoice { player, .. } => {
            *player == semantic_player
                && state
                    .pending_search_found_batch
                    .as_ref()
                    .is_some_and(|batch| batch.searcher == semantic_player)
        }
        _ => false,
    };
    eligible
        .then(|| state.active_search_decision_controls.get(&semantic_player))
        .flatten()
        .map(|record| record.authority)
}

pub fn authorized_submitter_for_player(state: &GameState, semantic_player: PlayerId) -> PlayerId {
    match search_decision_authority(state, semantic_player) {
        Some(ActiveSearchDecisionAuthority::LatchedController { controller }) => controller,
        Some(ActiveSearchDecisionAuthority::SearcherFallback) => semantic_player,
        None => effective_authority_for_player(state, semantic_player),
    }
}

/// CR 723.4: A controlled player and the player controlling them may see the
/// controlled player's private information while that control applies.
pub fn decision_audience_for_player(state: &GameState, semantic_player: PlayerId) -> Vec<PlayerId> {
    let submitter = effective_authority_for_player(state, semantic_player);
    if submitter == semantic_player {
        vec![semantic_player]
    } else {
        vec![semantic_player, submitter]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::SearchSelectionConstraint;
    use crate::types::game_state::ActiveSearchDecisionControl;

    #[test]
    fn search_prompt_uses_latched_controller_without_rebinding_to_live_control() {
        let mut state = GameState::new(crate::types::format::FormatConfig::free_for_all(), 3, 7);
        state.waiting_for = WaitingFor::SearchChoice {
            player: PlayerId(0),
            library_owner: Some(PlayerId(0)),
            cards: Vec::new(),
            count: 0,
            reveal: false,
            up_to: true,
            allows_partial_find: true,
            constraint: SearchSelectionConstraint::None,
            split: None,
        };
        state
            .active_search_decision_controls
            .insert(ActiveSearchDecisionControl {
                searcher: PlayerId(0),
                searched_zone_owner: PlayerId(0),
                authority: ActiveSearchDecisionAuthority::LatchedController {
                    controller: PlayerId(1),
                },
            });
        state.turn_decision_controller = Some(PlayerId(2));

        assert_eq!(authorized_submitter(&state), Some(PlayerId(1)));
        assert!(is_authorized_submitter(&state, PlayerId(1)));
        assert!(!is_authorized_submitter(&state, PlayerId(0)));
        assert!(!is_authorized_submitter(&state, PlayerId(2)));

        state
            .active_search_decision_controls
            .get_mut(&PlayerId(0))
            .unwrap()
            .authority = ActiveSearchDecisionAuthority::SearcherFallback;
        assert_eq!(authorized_submitter(&state), Some(PlayerId(0)));
    }
}
