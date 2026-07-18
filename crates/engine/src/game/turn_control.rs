use crate::types::ability::ControlWindow;
use crate::types::game_state::{
    ActivePlayerControl, ActiveSearchDecisionAuthority, GameState, ScheduledTurnControl, WaitingFor,
};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// CR 723.1 / CR 723.2 / CR 800.4a: the single authority that ENDS a
/// player-control effect. Removes the consumed schedule entry, clears its typed
/// active-window identity iff it is that exact effect, then recomputes the
/// current decision controller from the effects that remain applicable. Returns
/// the removed entry so the caller can apply
/// window-specific post-processing (CR 723.1 extra-turn grant; CR 723.2 no-op).
/// All three release sites — turn boundary (`start_next_turn`), combat-phase
/// boundary (`finish_enter_phase`), and leave-game cleanup (`do_eliminate`) —
/// route through here so control ends in exactly one place.
pub(super) fn release_control_at(state: &mut GameState, idx: usize) -> ScheduledTurnControl {
    let entry = state.scheduled_turn_controls[idx];
    let identity = control_identity(entry);
    let legacy_latch = (state.active_full_turn_control.is_none()
        && state.active_combat_phase_control.is_none())
    .then_some((
        state.turn_decision_controller,
        state.turn_decision_control_timestamp,
    ));
    let was_active =
        active_control_identity(state, entry.target_player, entry.window) == Some(identity);
    state.scheduled_turn_controls.remove(idx);
    match entry.window {
        ControlWindow::NextTurn if state.active_full_turn_control == Some(identity) => {
            state.active_full_turn_control = None;
        }
        ControlWindow::NextCombatPhase if state.active_combat_phase_control == Some(identity) => {
            state.active_combat_phase_control = None;
        }
        ControlWindow::NextTurn | ControlWindow::NextCombatPhase => {}
    }
    recompute_active_player_control(state);
    if !was_active {
        if let Some((controller, timestamp)) = legacy_latch {
            state.turn_decision_controller = controller;
            state.turn_decision_control_timestamp = timestamp;
        }
    }
    entry
}

pub(super) fn control_identity(scheduled: ScheduledTurnControl) -> ActivePlayerControl {
    ActivePlayerControl {
        controller: scheduled.controller,
        timestamp: scheduled.timestamp,
    }
}

/// CR 723.1a: Recompute the controlling player from every currently applicable
/// player-control effect. A combat-only effect may temporarily win by timestamp;
/// when it ends, the still-applicable full-turn effect automatically resumes.
pub(super) fn recompute_active_player_control(state: &mut GameState) {
    let active = [
        state.active_full_turn_control,
        state.active_combat_phase_control,
    ]
    .into_iter()
    .flatten()
    .max_by_key(|control| control.timestamp);
    state.turn_decision_controller = active.map(|control| control.controller);
    state.turn_decision_control_timestamp = active.map(|control| control.timestamp);
}

/// CR 723.1a: Activate the newest pending effect for one player-control window
/// and discard older effects it overwrote. Entries created after activation are
/// retained by the scheduler until the next matching window begins.
pub(super) fn activate_scheduled_control(
    state: &mut GameState,
    target_player: PlayerId,
    window: ControlWindow,
) -> Option<ScheduledTurnControl> {
    let selected_idx = state
        .scheduled_turn_controls
        .iter()
        .enumerate()
        .filter(|(_, scheduled)| {
            scheduled.target_player == target_player && scheduled.window == window
        })
        .max_by_key(|(_, scheduled)| scheduled.timestamp)
        .map(|(idx, _)| idx)?;
    let selected = state.scheduled_turn_controls[selected_idx];

    for idx in (0..state.scheduled_turn_controls.len()).rev() {
        if idx != selected_idx {
            let scheduled = state.scheduled_turn_controls[idx];
            if scheduled.target_player == target_player && scheduled.window == window {
                state.scheduled_turn_controls.remove(idx);
            }
        }
    }

    match window {
        ControlWindow::NextTurn => {
            state.active_full_turn_control = Some(control_identity(selected));
        }
        ControlWindow::NextCombatPhase => {
            state.active_combat_phase_control = Some(control_identity(selected));
        }
    }
    recompute_active_player_control(state);
    Some(selected)
}

fn explicit_active_control(
    state: &GameState,
    window: ControlWindow,
) -> Option<ActivePlayerControl> {
    match window {
        ControlWindow::NextTurn => state.active_full_turn_control,
        ControlWindow::NextCombatPhase => state.active_combat_phase_control,
    }
}

/// CR 723.1a: Return the identity applicable in one typed control window. The
/// latch fallback preserves compatibility with legacy saves and direct test
/// fixtures created before active-window identities were serialized.
pub(super) fn active_control_identity(
    state: &GameState,
    target_player: PlayerId,
    window: ControlWindow,
) -> Option<ActivePlayerControl> {
    explicit_active_control(state, window).or_else(|| {
        let identity = ActivePlayerControl {
            controller: state.turn_decision_controller?,
            timestamp: state.turn_decision_control_timestamp.unwrap_or(0),
        };
        state
            .scheduled_turn_controls
            .iter()
            .any(|scheduled| {
                scheduled.target_player == target_player
                    && scheduled.window == window
                    && control_identity(*scheduled) == identity
            })
            .then_some(identity)
    })
}

/// CR 723.1a: Locate the scheduled entry that created the currently active
/// player-control effect. Controller alone is insufficient because a newer,
/// future control effect may have the same controller and target.
pub(super) fn active_scheduled_control_index(
    state: &GameState,
    target_player: PlayerId,
    window: ControlWindow,
) -> Option<usize> {
    let identity = active_control_identity(state, target_player, window)?;
    state.scheduled_turn_controls.iter().position(|scheduled| {
        scheduled.target_player == target_player
            && scheduled.window == window
            && control_identity(*scheduled) == identity
    })
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlayerControlCandidate {
    controller: PlayerId,
    timestamp: u64,
    tie_breaker: u64,
}

/// CR 723.1a: Read creation provenance for the currently active scheduled
/// turn-control effect. A legacy or directly-constructed controller latch with
/// no provenance remains valid at timestamp zero, making it older than every
/// normally-created effect.
fn active_turn_control_candidate(
    state: &GameState,
    semantic_player: PlayerId,
) -> Option<PlayerControlCandidate> {
    let controlled_seat = if state.format_config.topology().has_shared_team_turns() {
        super::topology::team_members(state, state.active_player).contains(&semantic_player)
    } else {
        semantic_player == state.active_player
    };
    if !controlled_seat {
        return None;
    }
    let controller = state.turn_decision_controller?;
    Some(PlayerControlCandidate {
        controller,
        timestamp: state.turn_decision_control_timestamp.unwrap_or(0),
        tie_breaker: 0,
    })
}

/// CR 723.1a + CR 723.5: Select the newest functioning static that controls
/// `searcher` while that player searches their own library.
fn own_library_search_control_candidate(
    state: &GameState,
    searcher: PlayerId,
) -> Option<PlayerControlCandidate> {
    crate::game::functioning_abilities::battlefield_active_statics(state)
        .filter_map(|(source, definition)| match &definition.mode {
            StaticMode::ControlPlayersDuringOwnLibrarySearch { who }
                if match who {
                    // CR 102.3: In a team game, a player's teammates are not
                    // opponents. Keep this search-control scope on the canonical
                    // team-aware opponent authority without changing legacy
                    // prohibition-scope semantics for unrelated statics.
                    crate::types::statics::ProhibitionScope::Opponents => {
                        crate::game::players::is_opponent(state, source.controller, searcher)
                    }
                    _ => crate::game::static_abilities::prohibition_scope_matches_player(
                        who, searcher, source.id, state,
                    ),
                } =>
            {
                Some(PlayerControlCandidate {
                    controller: source.controller,
                    timestamp: source.timestamp,
                    // Normally timestamps are unique. The source identity keeps
                    // direct-constructed zero-timestamp fixtures deterministic
                    // while treating a real static as newer than a legacy latch.
                    tie_breaker: source.id.0.saturating_add(1),
                })
            }
            _ => None,
        })
        .max_by_key(|candidate| (candidate.timestamp, candidate.tie_breaker))
}

/// CR 723.1a + CR 723.5: Determine the controller whose authority must be
/// snapshotted for one prepared search. Search-scoped statics participate only
/// for an actual own-library search; ordinary turn control still governs every
/// decision the controlled player makes, including cross-library searches.
pub(crate) fn library_search_decision_controller(
    state: &GameState,
    searcher: PlayerId,
    effective_library_owner: Option<PlayerId>,
) -> PlayerId {
    let active_turn = active_turn_control_candidate(state, searcher);
    let search_static = (effective_library_owner == Some(searcher))
        .then(|| own_library_search_control_candidate(state, searcher))
        .flatten();
    active_turn
        .into_iter()
        .chain(search_static)
        .max_by_key(|candidate| (candidate.timestamp, candidate.tie_breaker))
        .map_or(searcher, |candidate| candidate.controller)
}

fn decision_audience(semantic_player: PlayerId, submitter: PlayerId) -> Vec<PlayerId> {
    if submitter == semantic_player {
        vec![semantic_player]
    } else {
        vec![semantic_player, submitter]
    }
}

/// CR 723.4: Build the hidden-information audience from the same controller
/// that search preparation is about to latch, avoiding a second live scan.
pub(crate) fn decision_audience_for_controller(
    semantic_player: PlayerId,
    controller: PlayerId,
) -> Vec<PlayerId> {
    decision_audience(semantic_player, controller)
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
    decision_audience(semantic_player, submitter)
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
