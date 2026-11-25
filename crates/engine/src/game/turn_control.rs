use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::types::ability::ControlWindow;
use crate::types::actions::ResolveAllScope;
use crate::types::game_state::{
    ActivePlayerControl, ActiveSearchDecisionAuthority, GameState, ResolveAllConsentRun,
    ScheduledTurnControl, WaitingFor,
};
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// CR 723.1 / CR 723.2 / CR 800.4a: the single authority that ENDS a
/// player-control effect. Removes the consumed schedule entry, clears its typed
/// active-window identity iff it is that exact effect, then recomputes the
/// current decision controller from the effects that remain applicable. Returns
/// the removed entry so the caller can apply
/// window-specific post-processing (CR 723.1 extra-turn grant; CR 723.2 no-op).
/// The per-entry release sites are the three boundaries: turn boundary
/// (`start_next_turn`), combat-phase boundary (`finish_enter_phase`), and
/// leave-game cleanup (`do_eliminate`). Every game-ending release goes through
/// [`end_all_player_control`], which enumerates its own callers, so control
/// ends in exactly one place.
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

/// CR 104.1 + CR 723.1: end every player-control effect because the game ended.
/// A game that has ended takes no further turn and no further combat phase, so
/// every scheduled window is over — including one that has not activated yet.
/// Each removal routes through [`release_control_at`], whose returned CR 500.7
/// extra-turn grant is deliberately dropped: the game is already over. That
/// authority clears a window only for the exact entry that created it, so the
/// two explicit clears are what turn its per-entry guarantee into the
/// whole-state postcondition "no control survives", and the recompute retires a
/// latch no surviving window backs.
///
/// Called from every site that ends a game: `elimination::end_game` (the
/// terminal record) and `match_flow::apply_trusted_match_forfeit` (the match
/// ending that does not go through it).
pub(super) fn end_all_player_control(state: &mut GameState) {
    while !state.scheduled_turn_controls.is_empty() {
        release_control_at(state, 0);
    }
    state.active_full_turn_control = None;
    state.active_combat_phase_control = None;
    recompute_active_player_control(state);
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
    let controlled_seat = if super::topology::side_takes_shared_turn(state, state.active_player) {
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

/// The current, unfrozen controller for a semantic player. Resolve All uses
/// this only to verify that a consent-time submitter was not rebound before
/// materializing its shared stack-resolution session.
pub(crate) fn live_authorized_submitter_for_player(
    state: &GameState,
    semantic_player: PlayerId,
) -> PlayerId {
    match search_decision_authority(state, semantic_player) {
        Some(ActiveSearchDecisionAuthority::LatchedController { controller }) => controller,
        Some(ActiveSearchDecisionAuthority::SearcherFallback) => semantic_player,
        None => effective_authority_for_player(state, semantic_player),
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
    // Resolve All consent freezes the submitting authority at proposal time.
    // This must win over live turn control: otherwise a control effect that
    // changes while a representative is queued could redirect an already-issued
    // response to a different actor.
    if let WaitingFor::ResolveAllConsent {
        epoch,
        representative,
    } = &state.waiting_for
    {
        if *representative == semantic_player {
            if let Some(submitter) = state
                .resolve_all_consent_run
                .as_ref()
                .filter(|run| run.epoch == *epoch)
                .and_then(|run| run.authorized_submitter_for(*representative))
            {
                return submitter;
            }
        }
    }
    live_authorized_submitter_for_player(state, semantic_player)
}

/// Whether a frozen Resolve All participant set still names precisely the
/// current priority representatives and their live submitting authorities.
///
/// CR 117.6 + CR 805.5b: representatives are the shared-team priority seats.
/// CR 723.5: a live controller change must not rebind a consent response that
/// was authorized for a different submitter.
pub(crate) fn resolve_all_consent_authority_matches_live(
    state: &GameState,
    run: &ResolveAllConsentRun,
) -> bool {
    let frozen_representatives: BTreeSet<_> = run
        .participants
        .iter()
        .map(|participant| participant.representative)
        .collect();
    let live_representatives: BTreeSet<_> = super::topology::priority_pass_participants(state)
        .into_iter()
        .collect();

    // CR 117.3d: a `Shared` run is a table-wide proposal, so a seat that became
    // a priority participant after the snapshot would otherwise be bound by a
    // consent it never gave — the live set must match exactly. An `Own` run
    // binds only the requester and asks nobody, so it stays coherent as long as
    // every seat it froze is still a live participant.
    let membership_holds = match run.scope {
        ResolveAllScope::Own => frozen_representatives.is_subset(&live_representatives),
        ResolveAllScope::Shared => frozen_representatives == live_representatives,
    };

    membership_holds
        && run.participants.iter().all(|participant| {
            live_authorized_submitter_for_player(state, participant.representative)
                == participant.authorized_submitter
        })
}

/// Returns the frozen submitter who may revoke one granted Resolve All consent.
/// Revoke is valid while a later representative is queued and after the run is
/// Ready, so it cannot be expressed through the ordinary single-actor prompt.
pub fn resolve_all_granted_submitter(
    state: &GameState,
    epoch: u64,
    representative: PlayerId,
) -> Option<PlayerId> {
    matches!(
        &state.waiting_for,
        WaitingFor::ResolveAllConsent { epoch: active, .. }
            | WaitingFor::ResolveAllReady { epoch: active }
            if *active == epoch
    )
    .then(|| state.resolve_all_consent_run.as_ref())
    .flatten()
    .filter(|run| run.epoch == epoch && run.is_granted(representative))
    .and_then(|run| run.authorized_submitter_for(representative))
}

/// Drops an active Resolve All consent run when player topology changes. A
/// frozen representative set is no longer meaningful after elimination, so
/// restart ordinary priority from a living representative instead of trying to
/// repair the proposal in place.
///
/// The public consent state, not the private run, decides whether a repair is
/// owed. An earlier form returned as soon as `take()` found no run, which made
/// this a no-op in exactly the case it exists to fix — a consent `WaitingFor`
/// left standing over a run that is already gone. Neither half of that pairing
/// can advance: a run-less `ResolveAllReady` has no acting player AND — with
/// no run to enumerate grantors from — not even the Revoke that an intact
/// latch still offers, and a run-less `ResolveAllConsent` still offers its
/// representative a Grant that `respond_resolve_all_consent` can only reject.
/// Taking the run stays unconditional so the two fields cannot disagree
/// afterwards.
///
/// CR 117.4: clearing the recorded passes restarts the pass cycle that the
/// discarded consent state had suspended, so priority resumes from the
/// repaired holder rather than from a partial round nobody can complete.
pub fn invalidate_resolve_all_consent(state: &mut GameState) {
    invalidate_resolve_all_consent_inner(state, false);
}

/// Invalidates a frozen consent run because the player topology is about to
/// change. Even before the departing seat is physically removed, its pending
/// departure makes the saved pass round unusable, so this deliberately chooses
/// the projected-live rebase rather than exact snapshot rollback.
pub fn invalidate_resolve_all_consent_for_topology_change(state: &mut GameState) {
    rebase_invalid_resolve_all_consent(state);
}

/// Discards an incoherent consent run and projects ordinary priority from the
/// live table rather than replaying its no-longer-valid saved pass round.
pub(crate) fn rebase_invalid_resolve_all_consent(state: &mut GameState) {
    invalidate_resolve_all_consent_inner(state, true);
}

fn invalidate_resolve_all_consent_inner(state: &mut GameState, force_projected_rebase: bool) {
    let run = state.resolve_all_consent_run.take();
    if let Some(baseline) = run.as_ref().and_then(|run| run.auto_pass_baseline.as_ref()) {
        state.auto_pass = baseline
            .iter()
            .map(|(&player, &mode)| (player, mode))
            .collect();
    }
    if !matches!(
        state.waiting_for,
        WaitingFor::ResolveAllConsent { .. } | WaitingFor::ResolveAllReady { .. }
    ) {
        return;
    }
    let exact_snapshot_is_live = !force_projected_rebase
        && run.as_ref().is_some_and(|run| {
            matches!(
                state.waiting_for,
                WaitingFor::ResolveAllConsent { epoch, .. } if epoch == run.epoch
            ) && state.priority_player == run.priority_snapshot.priority_player
                && state.priority_pass_count == run.priority_snapshot.priority_pass_count
                && state.priority_passes == run.priority_snapshot.priority_passes
                && resolve_all_consent_authority_matches_live(state, run)
        });
    if exact_snapshot_is_live {
        let snapshot = &run
            .as_ref()
            .expect("exact Resolve All recovery retains its run")
            .priority_snapshot;
        state.waiting_for = WaitingFor::Priority {
            player: snapshot.waiting_player,
        };
        state.priority_player = snapshot.priority_player;
        state.priority_pass_count = snapshot.priority_pass_count;
        state.priority_passes = snapshot.priority_passes.clone();
        return;
    }
    let preferred = super::topology::priority_pass_representative(state, state.active_player);
    let player = super::players::is_alive(state, preferred)
        .then_some(preferred)
        .or_else(|| {
            super::topology::priority_pass_participants(state)
                .first()
                .copied()
        })
        .unwrap_or(preferred);
    state.waiting_for = WaitingFor::Priority { player };
    state.priority_player = authorized_submitter_for_player(state, player);
    state.priority_pass_count = 0;
    state.priority_passes.clear();
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
///
/// CR 805.5a: A player may cast a spell, activate an ability, or take a special
/// action when their team has priority. Under the shared team turns option the
/// priority window is parked on the team's representative, but every living
/// member of that team is an authorized submitter — not just the holder.
pub fn authorized_submitters(state: &GameState) -> Vec<PlayerId> {
    if let WaitingFor::Priority { player } = &state.waiting_for {
        if super::topology::side_takes_shared_turn(state, *player) {
            let mut submitters: Vec<PlayerId> = super::topology::team_members(state, *player)
                .into_iter()
                .map(|member| authorized_submitter_for_player(state, member))
                .collect();
            // A team-wide controller (CR 805.8) maps every member to the same
            // submitter; dedup so the set stays a set.
            submitters.sort();
            submitters.dedup();
            return submitters;
        }
    }
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

/// CR 805.5a: the semantic decision owners for the current `WaitingFor` —
/// every living teammate under a shared-turn Priority window, otherwise the
/// engine's acting players. AI drivers iterate this (not `acting_players`)
/// so an AI teammate is eligible while the window sits on its representative.
pub fn decision_owners(state: &GameState) -> Vec<PlayerId> {
    if let WaitingFor::Priority { player } = &state.waiting_for {
        if super::topology::side_takes_shared_turn(state, *player) {
            return super::topology::team_members(state, *player);
        }
    }
    state.waiting_for.acting_players()
}

/// CR 723.5 + CR 805.8: one pending decision's semantic owner paired with the
/// seat authorized to submit it. The owner is who the rules say decides; the
/// actor is who actually submits — the owner's controller under a
/// turn-control effect, otherwise the owner itself.
///
/// This is NOT `ai_support::payment_surplus::DecisionBinding`: that
/// module-private type is a payment-preparation certificate (revision +
/// interaction + pools) with disjoint semantics. This type is always
/// referenced by full path (`turn_control::DecisionBinding`), matching the
/// existing `turn_control::decision_owners` call-site convention, so no `use`
/// collision is possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionBinding {
    pub semantic_owner: PlayerId,
    pub authorized_actor: PlayerId,
}

impl DecisionBinding {
    /// Map `semantic_owner` to its actor through the same frozen-or-live
    /// composition `AiDecisionContract::issue` binds — shared, not duplicated.
    pub fn for_owner(state: &GameState, semantic_owner: PlayerId) -> Self {
        Self {
            semantic_owner,
            authorized_actor: contract_actor_for_owner(state, semantic_owner),
        }
    }
}

/// The owner→actor composition `AiDecisionContract::issue` binds: a frozen
/// Resolve All submitter where one exists, else the live authorized
/// submitter. Moved here from `ai_support::context` so the one constructor
/// every scheduler shares sits beside its game-side callees.
pub(crate) fn contract_actor_for_owner(state: &GameState, semantic_owner: PlayerId) -> PlayerId {
    resolve_all_frozen_actor(state, semantic_owner)
        .unwrap_or_else(|| authorized_submitter_for_player(state, semantic_owner))
}

fn resolve_all_frozen_actor(state: &GameState, semantic_owner: PlayerId) -> Option<PlayerId> {
    let epoch = match &state.waiting_for {
        WaitingFor::ResolveAllConsent { epoch, .. } | WaitingFor::ResolveAllReady { epoch } => {
            *epoch
        }
        _ => return None,
    };
    resolve_all_granted_submitter(state, epoch, semantic_owner).or_else(|| {
        state
            .resolve_all_consent_run
            .as_ref()
            .filter(|run| run.epoch == epoch)
            .and_then(|run| run.authorized_submitter_for(semantic_owner))
    })
}

/// CR 805.5a + CR 805.5b + CR 723.5: the strict pending bindings — every
/// decision owner that still owes a decision, each paired with its authorized
/// actor. On a Priority window a member who already passed owes nothing until
/// someone acts (which clears passes); every other window maps the engine's
/// acting players unchanged. Strict pending only: no all-owners fallback here
/// (that stays the headless driver's documented policy) and no
/// holder-collapse (that stays the derived view's).
pub fn pending_decision_bindings(state: &GameState) -> Vec<DecisionBinding> {
    let owners = decision_owners(state);
    // Only a Priority window records passes; every other window resolves to
    // the engine's acting players unchanged.
    let unpassed: Vec<PlayerId> = if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        owners
            .into_iter()
            .filter(|owner| !state.priority_passes.contains(owner))
            .collect()
    } else {
        owners
    };
    unpassed
        .into_iter()
        .map(|owner| DecisionBinding::for_owner(state, owner))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_support::AiDecisionContract;
    use crate::types::ability::{Effect, EffectKind, ResolvedAbility, SearchSelectionConstraint};
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        ActiveSearchDecisionControl, PendingCast, ResolveAllConsentParticipant,
        ResolveAllPrioritySnapshot, StackResolutionBudget, TargetEffectDetail,
        TargetSelectionProgress, TargetSelectionSlot,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;

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
            ordering_hint: Default::default(),
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

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);
    const P2: PlayerId = PlayerId(2);

    fn two_hg_priority_state() -> GameState {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state
    }

    fn consent_run(
        epoch: u64,
        participants: Vec<(PlayerId, PlayerId, bool)>,
    ) -> ResolveAllConsentRun {
        ResolveAllConsentRun {
            epoch,
            max_resolutions: StackResolutionBudget::Unlimited,
            priority_snapshot: ResolveAllPrioritySnapshot {
                waiting_player: P0,
                priority_player: P0,
                priority_pass_count: 0,
                priority_passes: BTreeSet::new(),
            },
            scope: ResolveAllScope::Shared,
            participants: participants
                .into_iter()
                .map(|(representative, authorized_submitter, granted)| {
                    ResolveAllConsentParticipant {
                        representative,
                        authorized_submitter,
                        granted,
                    }
                })
                .collect(),
            auto_pass_baseline: None,
        }
    }

    fn targeting_window(player: PlayerId) -> WaitingFor {
        WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(PendingCast::new(
                ObjectId(1),
                CardId(1),
                ResolvedAbility::new(Effect::NoOp, Vec::new(), ObjectId(1), player),
                ManaCost::NoCost,
            )),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: Vec::new(),
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: TargetSelectionProgress::default(),
        }
    }

    /// VM-A1: the authority lists strict pending bindings with controller
    /// actors. Pure-fn pin: mixed pass states under control are unreachable
    /// through the production pass boundary (a controller-submitted pass
    /// records the whole team), so this is imperative by necessity.
    #[test]
    fn pending_bindings_pair_unpassed_owners_with_actors() {
        let mut state = two_hg_priority_state();

        // Sibling (uncontrolled): actor == owner identity.
        assert_eq!(
            pending_decision_bindings(&state),
            vec![
                DecisionBinding {
                    semantic_owner: P0,
                    authorized_actor: P0,
                },
                DecisionBinding {
                    semantic_owner: P1,
                    authorized_actor: P1,
                },
            ]
        );

        // Latch P2 as the active team's controller (CR 723.5 + CR 805.8).
        state.turn_decision_controller = Some(P2);
        // Reach guards: the latch is live and the owner list is the full team.
        assert_eq!(authorized_submitter_for_player(&state, P0), P2);
        assert_eq!(decision_owners(&state), vec![P0, P1]);

        state.priority_passes.insert(P1);
        assert_eq!(
            pending_decision_bindings(&state),
            vec![DecisionBinding {
                semantic_owner: P0,
                authorized_actor: P2,
            }]
        );

        state.priority_passes.insert(P0);
        assert!(
            pending_decision_bindings(&state).is_empty(),
            "every member passed, so no binding is pending"
        );

        // Non-Priority windows map the acting players unchanged (no pass
        // filter); the controlled actor still applies.
        state.priority_passes.clear();
        state.priority_passes.insert(P1);
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: Vec::new(),
            valid_attack_targets: Vec::new(),
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
        assert_eq!(
            pending_decision_bindings(&state),
            vec![DecisionBinding {
                semantic_owner: P0,
                authorized_actor: P2,
            }]
        );
    }

    /// VM-A2: the constructor's actor equals `issue`'s actor on every window
    /// family — the shared-fn contract. Consent/Ready/granted legs assert the
    /// frozen-vs-live divergence is REAL; Priority/TargetSelection legs assert
    /// frozen-`None` agreement (both sides equal the live submitter).
    #[test]
    fn binding_actor_matches_issued_contract_actor() {
        // Shared Priority, uncontrolled: frozen arm is None, both sides equal
        // the live submitter.
        let state = two_hg_priority_state();
        for owner in decision_owners(&state) {
            let live = live_authorized_submitter_for_player(&state, owner);
            assert_eq!(
                DecisionBinding::for_owner(&state, owner).authorized_actor,
                live,
                "constructor must equal the live submitter on Priority (no frozen arm)"
            );
            assert_eq!(
                AiDecisionContract::issue(&state, owner).authorized_actor,
                live,
                "issued contract must equal the live submitter on Priority (no frozen arm)"
            );
        }

        // Individual Priority: degenerate single binding, same agreement.
        let mut solo = GameState::new(FormatConfig::standard(), 2, 42);
        solo.waiting_for = WaitingFor::Priority { player: P1 };
        for owner in decision_owners(&solo) {
            let live = live_authorized_submitter_for_player(&solo, owner);
            assert_eq!(
                DecisionBinding::for_owner(&solo, owner).authorized_actor,
                AiDecisionContract::issue(&solo, owner).authorized_actor,
            );
            assert_eq!(
                DecisionBinding::for_owner(&solo, owner).authorized_actor,
                live,
            );
        }

        // TargetSelection: same frozen-None agreement outside Priority.
        let mut targeted = two_hg_priority_state();
        targeted.waiting_for = targeting_window(P0);
        for owner in decision_owners(&targeted) {
            let live = live_authorized_submitter_for_player(&targeted, owner);
            assert_eq!(
                DecisionBinding::for_owner(&targeted, owner).authorized_actor,
                AiDecisionContract::issue(&targeted, owner).authorized_actor,
            );
            assert_eq!(
                DecisionBinding::for_owner(&targeted, owner).authorized_actor,
                live,
            );
        }

        // Consent with a granted run: frozen P2 diverges from the live P0.
        let mut consent = two_hg_priority_state();
        consent.waiting_for = WaitingFor::ResolveAllConsent {
            epoch: 0,
            representative: P0,
        };
        consent.resolve_all_consent_run = Some(consent_run(0, vec![(P0, P2, true)]));
        assert_eq!(resolve_all_granted_submitter(&consent, 0, P0), Some(P2));
        assert_eq!(
            live_authorized_submitter_for_player(&consent, P0),
            P0,
            "reach: the frozen-vs-live divergence is REAL (no latch, so live is P0)"
        );
        assert!(consent.turn_decision_controller.is_none());
        assert_eq!(
            DecisionBinding::for_owner(&consent, P0).authorized_actor,
            P2,
        );
        assert_eq!(
            DecisionBinding::for_owner(&consent, P0).authorized_actor,
            AiDecisionContract::issue(&consent, P0).authorized_actor,
        );

        // Hostile: granted-revoke window — the frozen granted submitter still
        // binds after the run is Ready (direct call, never enumerated: Ready
        // has no acting players).
        let mut ready = two_hg_priority_state();
        ready.waiting_for = WaitingFor::ResolveAllReady { epoch: 0 };
        ready.resolve_all_consent_run = Some(consent_run(0, vec![(P0, P2, true)]));
        assert!(
            decision_owners(&ready).is_empty(),
            "reach: Ready enumerates no owners"
        );
        assert_eq!(
            live_authorized_submitter_for_player(&ready, P0),
            P0,
            "reach: the frozen-vs-live divergence is REAL on Ready too"
        );
        assert_eq!(DecisionBinding::for_owner(&ready, P0).authorized_actor, P2);
        assert_eq!(
            DecisionBinding::for_owner(&ready, P0).authorized_actor,
            AiDecisionContract::issue(&ready, P0).authorized_actor,
        );
    }
}
