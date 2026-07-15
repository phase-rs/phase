use crate::types::game_state::{GameState, ScheduledTurnControl, ScheduledTurnControlLifecycle};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// Materialize the pre-schedule turn-controller latch as the oldest active
/// scheduled control effect when loading a legacy save. Keeping a real entry
/// (rather than the read-time virtual fallback) lets CR 723.1a timestamp
/// ordering overlay it with newer phase/turn controls and reveal it again when
/// the newer effect ends.
pub(crate) fn migrate_legacy_turn_controller_latch(state: &mut GameState) {
    let Some(controller) = state.turn_decision_controller else {
        return;
    };
    let target_player =
        super::topology::normalize_shared_turn_recipient(state, state.active_player);
    if state.scheduled_turn_controls.iter().any(|scheduled| {
        scheduled.target_player == target_player
            && scheduled.controller == controller
            && scheduled.lifecycle == ScheduledTurnControlLifecycle::Active
    }) {
        return;
    }

    if let Some(index) = state.scheduled_turn_controls.iter().position(|scheduled| {
        scheduled.target_player == target_player
            && scheduled.controller == controller
            && scheduled.timestamp == 0
    }) {
        let mut legacy = state.scheduled_turn_controls.remove(index);
        legacy.lifecycle = ScheduledTurnControlLifecycle::Active;
        // The missing legacy timestamp deserializes to zero. Put that entry
        // first so any other zero-timestamp entry is deterministically newer.
        state.scheduled_turn_controls.insert(0, legacy);
        return;
    }

    // Legacy saves have no creation timestamp. Zero is the deterministic oldest
    // timestamp; insertion at the front makes any other timestamp-zero legacy
    // schedule later in vector order win because Iterator::max_by_key returns
    // the last element among equal maxima.
    state.scheduled_turn_controls.insert(
        0,
        ScheduledTurnControl {
            target_player,
            controller,
            timestamp: 0,
            lifecycle: ScheduledTurnControlLifecycle::Active,
            grant_extra_turn_after: false,
            window: crate::types::ability::ControlWindow::NextTurn,
        },
    );
}

/// CR 723.1 / CR 723.2 / CR 800.4a: the single authority that ENDS a
/// player-control effect. Removes the consumed schedule entry, then recomputes
/// the newest remaining active effect for the affected player (CR 723.1a).
/// Returns the removed entry so the caller can apply
/// window-specific post-processing (CR 723.1 extra-turn grant; CR 723.2 no-op).
/// All three release sites — turn boundary (`start_next_turn`), combat-phase
/// boundary (`finish_enter_phase`), and leave-game cleanup (`do_eliminate`) —
/// route through here so control ends in exactly one place.
pub(super) fn release_control_at(state: &mut GameState, idx: usize) -> ScheduledTurnControl {
    let entry = state.scheduled_turn_controls.remove(idx);
    recompute_active_turn_controller(state);
    entry
}

/// CR 723.1: transition an already-resolved next-turn player-control effect
/// from its future window into the active window. Keeping this transition on
/// the stored effect (rather than only setting the legacy controller latch)
/// lets authorization distinguish active control from a pending schedule.
pub(super) fn activate_control_at(state: &mut GameState, idx: usize) {
    state.scheduled_turn_controls[idx].lifecycle = ScheduledTurnControlLifecycle::Active;
    recompute_active_turn_controller(state);
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PlayerControlEffect {
    controller: PlayerId,
    timestamp: u64,
}

/// CR 723.1a: among every currently active effect for the active player, the
/// one created last controls that player. Activation and release both route
/// through this authority so ending a newer phase-scoped effect restores an
/// older full-turn effect instead of clearing the decision-controller latch.
fn newest_active_scheduled_control(state: &GameState) -> Option<PlayerControlEffect> {
    let active_key = super::topology::normalize_shared_turn_recipient(state, state.active_player);
    state
        .scheduled_turn_controls
        .iter()
        .filter(|scheduled| {
            scheduled.target_player == active_key
                && (scheduled.lifecycle == ScheduledTurnControlLifecycle::Active
                    // Backwards-compatible migration for saved games written
                    // before lifecycle existed. Timestamp zero plus the active
                    // target and legacy controller latch identifies the
                    // already-active legacy entry.
                    || (scheduled.lifecycle == ScheduledTurnControlLifecycle::Pending
                        && scheduled.timestamp == 0
                        && state.turn_decision_controller == Some(scheduled.controller)))
        })
        .max_by_key(|scheduled| scheduled.timestamp)
        .map(|scheduled| PlayerControlEffect {
            controller: scheduled.controller,
            timestamp: scheduled.timestamp,
        })
}

fn recompute_active_turn_controller(state: &mut GameState) {
    state.turn_decision_controller =
        newest_active_scheduled_control(state).map(|effect| effect.controller);
}

/// CR 723.1a + CR 723.5: Of the functioning effects that control decisions made
/// during this player's library search, return the newest effect's controller.
fn library_search_control_effect(
    state: &GameState,
    searcher: PlayerId,
) -> Option<PlayerControlEffect> {
    crate::game::functioning_abilities::battlefield_active_statics(state)
        .filter_map(|(source, definition)| match &definition.mode {
            StaticMode::ControlPlayersDuringOwnLibrarySearch { who }
                if crate::game::static_abilities::prohibition_scope_matches_player(
                    who, searcher, source.id, state,
                ) =>
            {
                Some((source.timestamp, source.id, source.controller))
            }
            _ => None,
        })
        .max_by_key(|(timestamp, source_id, _)| (*timestamp, *source_id))
        .map(|(timestamp, _, controller)| PlayerControlEffect {
            controller,
            timestamp,
        })
}

fn active_turn_control_effect(
    state: &GameState,
    semantic_player: PlayerId,
) -> Option<PlayerControlEffect> {
    let active_key = super::topology::normalize_shared_turn_recipient(state, state.active_player);
    let semantic_key = super::topology::normalize_shared_turn_recipient(state, semantic_player);
    if active_key != semantic_key {
        return None;
    }
    if let Some(active) = newest_active_scheduled_control(state) {
        return Some(active);
    }
    let current_controller = state.turn_decision_controller?;
    let matching_schedule_exists = state.scheduled_turn_controls.iter().any(|scheduled| {
        scheduled.target_player == active_key && scheduled.controller == current_controller
    });
    if matching_schedule_exists {
        return None;
    }
    // Directly constructed legacy/test states can carry the controller latch
    // without a serialized schedule entry. Keep their old behavior at the
    // oldest possible creation time.
    Some(PlayerControlEffect {
        controller: current_controller,
        timestamp: 0,
    })
}

pub fn authorized_submitter_for_player(state: &GameState, semantic_player: PlayerId) -> PlayerId {
    // CR 723.1a + CR 723.5: library-search control redirects only decisions
    // made during the search. The semantic player in `WaitingFor` remains the
    // searcher; transport authorization is derived here. Among applicable
    // continuous effects, the newest functioning source controls the choice.
    let search_decision_active = matches!(
        &state.waiting_for,
        crate::types::game_state::WaitingFor::SearchChoice { player, .. }
            if *player == semantic_player
    ) || state
        .pending_search_found_batch
        .as_ref()
        .is_some_and(|batch| batch.searcher == semantic_player)
        || state.pending_replacement.as_ref().is_some_and(|pending| {
            matches!(
                &pending.proposed,
                crate::types::proposed_event::ProposedEvent::SearchFound { searcher, .. }
                    if *searcher == semantic_player
            )
        });
    if state
        .library_search_control
        .as_ref()
        .is_some_and(|binding| {
            binding.searcher == semantic_player && binding.library_owner == semantic_player
        })
        && search_decision_active
    {
        let newest = [
            active_turn_control_effect(state, semantic_player),
            library_search_control_effect(state, semantic_player),
        ]
        .into_iter()
        .flatten()
        .max_by_key(|effect| effect.timestamp);
        if let Some(effect) = newest {
            return effect.controller;
        }
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ControlWindow;

    /// CR 723.1a: a newer phase-scoped control overwrites an older full-turn
    /// control only while both are active; releasing the phase-scoped effect
    /// restores the older active effect.
    #[test]
    fn releasing_newer_phase_control_restores_older_full_turn_control() {
        let mut state = GameState::new_two_player(42);
        let affected = state.active_player;
        state.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: affected,
            controller: PlayerId(1),
            timestamp: 10,
            lifecycle: ScheduledTurnControlLifecycle::Active,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        });
        state.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: affected,
            controller: PlayerId(0),
            timestamp: 20,
            lifecycle: ScheduledTurnControlLifecycle::Pending,
            grant_extra_turn_after: false,
            window: ControlWindow::NextCombatPhase,
        });

        activate_control_at(&mut state, 1);
        assert_eq!(state.turn_decision_controller, Some(PlayerId(0)));

        let released = release_control_at(&mut state, 1);
        assert_eq!(released.window, ControlWindow::NextCombatPhase);
        assert_eq!(state.turn_decision_controller, Some(PlayerId(1)));
        assert_eq!(turn_decision_maker(&state), PlayerId(1));
    }

    fn legacy_state_with_newer_phase_control() -> GameState {
        let mut legacy = GameState::new_two_player(42);
        let affected = legacy.active_player;
        let legacy_controller = PlayerId(1);
        let phase_controller = PlayerId(0);
        legacy.turn_decision_controller = Some(legacy_controller);
        legacy.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: affected,
            controller: legacy_controller,
            timestamp: 0,
            lifecycle: ScheduledTurnControlLifecycle::Pending,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        });
        legacy.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: affected,
            controller: phase_controller,
            timestamp: 20,
            lifecycle: ScheduledTurnControlLifecycle::Pending,
            grant_extra_turn_after: false,
            window: ControlWindow::NextCombatPhase,
        });
        legacy
    }

    fn assert_legacy_control_reappears_after_overlay(mut restored: GameState) {
        let legacy_controller = PlayerId(1);
        let phase_controller = PlayerId(0);

        assert_eq!(restored.scheduled_turn_controls.len(), 2);
        assert_eq!(restored.scheduled_turn_controls[0].timestamp, 0);
        assert_eq!(
            restored.scheduled_turn_controls[0].lifecycle,
            ScheduledTurnControlLifecycle::Active
        );

        activate_control_at(&mut restored, 1);
        assert_eq!(restored.turn_decision_controller, Some(phase_controller));
        release_control_at(&mut restored, 1);
        assert_eq!(restored.turn_decision_controller, Some(legacy_controller));
    }

    #[test]
    fn raw_legacy_pending_schedule_is_activated_and_restored_after_newer_phase_control() {
        let legacy = legacy_state_with_newer_phase_control();

        let json = serde_json::to_string(&legacy).expect("serialize legacy state");
        let persisted: crate::types::game_state::PersistedGameState =
            serde_json::from_str(&json).expect("deserialize legacy state");
        assert_legacy_control_reappears_after_overlay(persisted.into_game_state());
    }

    #[test]
    fn trusted_legacy_pending_schedule_is_activated_and_restored_after_newer_phase_control() {
        let legacy = legacy_state_with_newer_phase_control();
        let persisted = crate::types::game_state::PersistedGameState::capture(legacy);
        let json = serde_json::to_string(&persisted).expect("serialize trusted legacy state");
        let persisted: crate::types::game_state::PersistedGameState =
            serde_json::from_str(&json).expect("deserialize trusted legacy state");

        assert_legacy_control_reappears_after_overlay(persisted.into_game_state());
    }

    #[test]
    fn materialized_legacy_latch_loses_timestamp_zero_tie_to_existing_schedule() {
        let mut state = GameState::new_two_player(42);
        let affected = state.active_player;
        let legacy_controller = PlayerId(1);
        let existing_controller = PlayerId(0);
        state.turn_decision_controller = Some(legacy_controller);
        state.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: affected,
            controller: existing_controller,
            timestamp: 0,
            lifecycle: ScheduledTurnControlLifecycle::Active,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        });

        migrate_legacy_turn_controller_latch(&mut state);

        assert_eq!(
            state.scheduled_turn_controls[0].controller,
            legacy_controller
        );
        recompute_active_turn_controller(&mut state);
        assert_eq!(
            state.turn_decision_controller,
            Some(existing_controller),
            "max_by_key must select the later existing schedule on an equal timestamp"
        );
    }
}
