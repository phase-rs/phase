//! "Request takeback" — multiplayer-safe undo (GH #1507).
//!
//! Single-player/local undo (`engine-wasm::restore_game_state`) replaces the
//! client's own state wholesale and is refused outright in multiplayer
//! sessions, because no client may unilaterally rewrite the authoritative
//! server state another player is relying on. This module gives multiplayer
//! sessions an equivalent escape hatch for misclicks and UI confusion,
//! without ever letting one player rewrite history unilaterally: a player
//! may request to roll the *authoritative* session state back to the
//! snapshot taken just before their most recent action, but the rollback
//! only takes effect once every human seat at the table (the requester
//! included) has approved it. Any single human decline cancels the request
//! and the authoritative state is left untouched.
//!
//! The same machine is parameterized by [`RewindTarget`] rather than
//! duplicated per granularity: `LastAction` is the original GH #1507 undo, and
//! `TurnStart` reaches a previous turn boundary — which the action-granular
//! ring structurally cannot, since every wire `PassPriority` burns one of its
//! twelve slots. Turn boundaries are captured from each transition's own
//! post-state into a second ring, and offered only where
//! [`offers_turn_rewind`] says so.
//!
//! This is intentionally a session/room-level concern, not an engine rule —
//! there is no Comprehensive Rules concept of "undo." The engine is never
//! told why it was handed an older `GameState`; it just continues from
//! whatever state it's given, exactly as it does on reconnect/restore.

use std::collections::{HashSet, VecDeque};

use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use phase_ai::session::AiSession;
use serde::{Deserialize, Serialize};

use crate::session::{GameSession, HostingMode};

/// How many prior authoritative snapshots a session retains for takeback
/// purposes. Bounded so a long game session can't accumulate unbounded
/// memory — a takeback can only reach back to the most recent action, so
/// anything beyond a handful of entries is never reachable anyway.
pub const MAX_TAKEBACK_HISTORY: usize = 12;

/// How many turn-boundary snapshots a session retains. Deliberately the same
/// depth as the browser-local analogue (`client/src/constants/game.ts`'s
/// `MAX_UNDO_HISTORY = 5`), so desktop solo-vs-AI offers the same reach the
/// browser sandbox already does. Small because these are whole `GameState`
/// clones and — unlike `MAX_TAKEBACK_HISTORY` — only ever populated on a
/// `HostingMode::SingleUser` sidecar (see [`offers_turn_rewind`]).
pub const MAX_TURN_REWIND_HISTORY: usize = 5;

/// How far back a rollback request reaches.
///
/// Parameterizes the one rollback machine rather than growing a sibling wire
/// message per granularity: the approval state machine, the pending-request
/// interlock, `rekey_after_trusted_restore`, and the reconnect replay are all
/// shared. `LastAction` is the pre-existing GH #1507 behaviour and is the
/// `Default` so an absent wire payload from a pre-rewind client normalizes to
/// exactly what that client meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RewindTarget {
    /// Roll back to the snapshot taken immediately before the requester's own
    /// most recent state-mutating action.
    #[default]
    LastAction,
    /// Roll back to the start of the numbered turn *of the current game*.
    /// Turn numbers are only unique within a game (see
    /// [`GameSession::observe_transition`]), which is why the rings are
    /// cleared at a match's game boundary rather than keyed by game number.
    TurnStart { turn_number: u32 },
}

/// One turn boundary a client may ask to roll back to, as published by the
/// server. Both fields are read from the stored snapshot's *own* state, never
/// recomputed at publication time, so the label a player clicks is exactly the
/// state they get.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindOption {
    pub turn_number: u32,
    pub active_player: PlayerId,
}

/// Whether this deployment offers turn-granular rewind.
///
/// A **policy**, not a capability of the mechanism: the machinery below is
/// mode-agnostic and `LastAction` is deliberately *not* scoped by it. Nobody
/// asked for a table-wide vote at turn scale, and the trigger surface
/// (`DebugPanel`) is not gated by `showSandboxTools` — it is reachable in every
/// mode via the keyboard shortcut. Gating capture as well as publication means
/// a shared server retains zero turn snapshots rather than retaining snapshots
/// it will never offer.
pub fn offers_turn_rewind(hosting: HostingMode) -> bool {
    hosting == HostingMode::SingleUser
}

/// Records `state` as a turn-boundary rewind point iff the transition that
/// produced it *came to rest in* the turn its own events announced.
///
/// A free function over the ring rather than a `&mut self` method, so the
/// capture rule can be exercised directly against a `VecDeque` and a synthetic
/// event batch — the rule's hostile cases (a batch that both starts and ends a
/// turn, a batch carrying two boundaries) need no session at all. Callers go
/// through [`GameSession::observe_transition`], which is the `&mut self` wrapper
/// that also owns the game-boundary retirement; nothing calls this directly in
/// production.
///
/// The `started != state.turn_number` guard is an honesty guard, not an
/// optimization: a batch that both started *and* ended a turn leaves no state
/// that could be truthfully labelled with either turn, so that boundary is not
/// offered rather than mislabelled.
///
/// **Uniqueness.** No two retained entries share a `turn_number`, *within a
/// game*: `state.turn_number` is assigned 1 at a game start
/// (`engine::game::engine`'s two `start_game_*` helpers) and thereafter only
/// incremented, unconditionally and once per turn start
/// (`engine/src/game/turns.rs`, CR 500.7 — including extra turns; a *skipped*
/// turn increments without emitting `TurnStarted`). `TurnStarted` has three
/// production emission sites — `turns.rs` plus those two `start_game_*`
/// helpers, which both emit `TurnStarted { turn_number: 1 }` — so uniqueness is
/// a within-game property only. Crossing into another game of a Bo3 match is
/// handled by [`GameSession::observe_transition`], which clears both rings, and
/// the `debug_assert!` below is what fails loudly if that ever stops happening.
pub fn record_turn_rewind_point(
    ring: &mut VecDeque<GameState>,
    hosting: HostingMode,
    events: &[GameEvent],
    state: &GameState,
) {
    if !offers_turn_rewind(hosting) {
        return;
    }
    let Some(started) = events.iter().rev().find_map(|event| match event {
        GameEvent::TurnStarted { turn_number, .. } => Some(*turn_number),
        _ => None,
    }) else {
        return;
    };
    if started != state.turn_number {
        return;
    }
    debug_assert!(
        ring.back()
            .is_none_or(|s| s.turn_number < state.turn_number),
        "turn rewind ring must stay strictly increasing within a game"
    );
    if ring.len() >= MAX_TURN_REWIND_HISTORY {
        ring.pop_front();
    }
    ring.push_back(state.clone());
}

/// A takeback request awaiting unanimous human approval.
#[derive(Debug, Clone)]
pub struct PendingTakeback {
    /// The player who asked for the takeback. Implicitly counted as having
    /// approved their own request.
    pub requested_by: PlayerId,
    /// The authoritative state to restore if every human seat approves —
    /// the snapshot taken immediately before the requester's last
    /// state-mutating action.
    pub target_state: GameState,
    /// Human seats that have approved so far. The request resolves the
    /// instant this set contains every human seat in the session.
    pub approvals: HashSet<PlayerId>,
    /// How much of `takeback_history` remains a genuine ancestor chain of
    /// `target_state` once the rollback lands. Computed at request time,
    /// which is sound because every path that could mutate the history
    /// refuses while a request is pending (`handle_action`,
    /// `handle_interaction`, `handle_match_concede`) and `run_ai` is a no-op.
    pub history_truncate_len: usize,
}

/// Outcome of requesting or responding to a takeback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TakebackOutcome {
    /// Still waiting on one or more human players to respond.
    Pending,
    /// Every human seat approved — the session state has already been
    /// rolled back to the target snapshot by the time this is returned.
    Approved,
    /// A human player declined — the request was withdrawn and the
    /// authoritative state is unchanged.
    Rejected,
}

impl GameSession {
    /// Records the current authoritative state as a takeback checkpoint,
    /// tagged with the player about to act. Caps retention at
    /// [`MAX_TAKEBACK_HISTORY`] (oldest dropped first).
    ///
    /// Tagging by actor (rather than just "most recent action") is what lets
    /// `request_takeback` find *this player's* last action even when other
    /// players have acted since — see its doc comment.
    pub fn push_takeback_snapshot(&mut self, actor: PlayerId) {
        self.push_takeback_state(actor, self.state.clone());
    }

    /// Records the pre-action state for an action that has already succeeded.
    /// Keeping the state explicit lets the session apply through the engine
    /// first, so rejected attempts never become undo points.
    pub fn push_takeback_state(&mut self, actor: PlayerId, state: GameState) {
        if self.takeback_history.len() >= MAX_TAKEBACK_HISTORY {
            self.takeback_history.pop_front();
        }
        self.takeback_history.push_back((actor, state));
    }

    /// The single per-transition rewind bookkeeping authority. Every
    /// authoritative state transition calls this with **its own** event batch
    /// and post-state — never with `self.state`, which several call sites have
    /// already advanced past the transition being recorded.
    ///
    /// Two responsibilities, in this order because the second depends on the
    /// first: retire both rings at a match's game boundary, then record the
    /// turn boundary this transition came to rest in (if any).
    ///
    /// **Why the game-boundary clear.** `turn_number` is *assigned* 1 at each
    /// game's start rather than carried across a Bo3 match (CR 100.6a: a
    /// two-player match is a series of *games*), and the
    /// between-games `ChoosePlayDraw` action rebuilds the state through
    /// `handle_action` — a capture site — with `TurnStarted { turn_number: 1 }`
    /// in its batch. Without this, `turn_number` is not monotone across the
    /// session, and a `TurnStart { n }` request would resolve by first match to
    /// a *finished game's* turn n and install that board into the live game.
    /// Both rings are retired, not just the turn ring: a rollback into a
    /// previous game of the match is not a coherent offer at any granularity,
    /// and clearing both is what restores the within-game monotonicity that
    /// `partition_point`, `retain`, and `record_turn_rewind_point`'s
    /// `debug_assert!` all rely on. (`takeback_history` was not previously
    /// cleared on a game transition — it is cleared only after an approved
    /// rollback — so this is a new, deliberate retirement point for it.)
    pub fn observe_transition(&mut self, events: &[GameEvent], state: &GameState) {
        if state.game_number != self.rewind_game_number {
            self.rewind_game_number = state.game_number;
            self.takeback_history.clear();
            self.turn_rewind_history.clear();
        }
        record_turn_rewind_point(&mut self.turn_rewind_history, self.hosting, events, state);
    }

    /// The turn boundaries this session currently offers, oldest first. Each
    /// option is labelled from its snapshot's *own* fields, so the label and
    /// the state a player gets cannot disagree.
    pub fn rewind_options(&self) -> Vec<RewindOption> {
        if !offers_turn_rewind(self.hosting) {
            return Vec::new();
        }
        self.turn_rewind_history
            .iter()
            .map(|state| RewindOption {
                turn_number: state.turn_number,
                active_player: state.active_player,
            })
            .collect()
    }

    /// Human (non-AI) seats in this session, in seat order. AI seats never
    /// request, approve, or block a takeback — they have no UI to misclick.
    pub fn human_seats(&self) -> Vec<PlayerId> {
        (0..self.player_count)
            .map(PlayerId)
            .filter(|p| !self.ai_seats.contains(p))
            .collect()
    }

    /// Resolves the pending request if every human seat has now approved,
    /// applying the rollback in place. Returns `None` if still pending.
    fn try_resolve_pending_takeback(&mut self) -> Option<TakebackOutcome> {
        let pending = self.pending_takeback.as_ref()?;
        let humans = self.human_seats();
        if !humans.iter().all(|p| pending.approvals.contains(p)) {
            return None;
        }
        let pending = self.pending_takeback.take().expect("checked above");
        self.state = pending.target_state;
        engine::game::rekey_after_trusted_restore(&mut self.state);
        // The rolled-back state is the new baseline. Snapshots *after* the
        // restored one belong to the branch the table just discarded, and
        // taking another takeback back through them would resurrect actions
        // the table just agreed to undo. Snapshots *before* it are still
        // genuine ancestors of it, and a takeback that targets one of those
        // goes strictly further back — discarding strictly more, resurrecting
        // nothing. `history_truncate_len`, computed at request time, is
        // exactly that ancestor prefix; `clear()` was the conservative
        // over-approximation of this same reasoning, and truncating is what
        // makes undo repeatable rather than one-shot.
        self.takeback_history.truncate(pending.history_truncate_len);
        // `<=`, not `<`: a `TurnStart { n }` rewind targets the turn-n
        // snapshot itself, so keeping turn n selectable is what makes turn
        // rewind idempotent and repeatable rather than consuming its own
        // target.
        let restored_turn = self.state.turn_number;
        self.turn_rewind_history
            .retain(|snapshot| snapshot.turn_number <= restored_turn);
        // Every other path that installs a `GameState` wholesale rebuilds the
        // AI session (`GameSession::start_game`, `from_persisted`); this one
        // must too, now that an approved rollback can be followed immediately
        // by `run_ai`. Rebuild rather than invalidate selectively:
        // `rekey_after_trusted_restore` rewrites object identity, so the
        // cached routes and prompts keyed on the discarded branch's ids are
        // stale by construction, not merely cold.
        if self.ai_session.is_some() {
            self.ai_session = Some(AiSession::arc_from_game(&self.state));
        }
        Some(TakebackOutcome::Approved)
    }

    /// A human player requests rolling the game back to the state just
    /// before *their own* most recent action — not simply the most recent
    /// action by anyone. Other players may have acted since; rolling back
    /// to before the requester's action necessarily discards those later
    /// actions too (there is no way to keep them while undoing an earlier
    /// action they were built on), but it must never target a snapshot that
    /// precedes a different player's action while leaving the requester's
    /// own action untouched. Auto-resolves to `Approved` immediately when
    /// the requester is the only human at the table (e.g. solo vs. AI)
    /// since there is nobody else to ask.
    ///
    /// `target` selects the granularity. `RewindTarget::TurnStart` reaches a
    /// boundary the action-granular ring structurally cannot: every wire
    /// `PassPriority` burns a `MAX_TAKEBACK_HISTORY` slot, so a whole turn
    /// (CR 500.1) never fits inside twelve entries.
    pub fn request_takeback(
        &mut self,
        player: PlayerId,
        target: RewindTarget,
    ) -> Result<TakebackOutcome, String> {
        self.reject_if_ai_driver_faulted()?;
        if self.pending_takeback.is_some() {
            return Err("A takeback request is already pending for this game".to_string());
        }
        if !self.human_seats().contains(&player) {
            return Err("Only human players may request a takeback".to_string());
        }
        let (target_state, history_truncate_len) = match target {
            RewindTarget::LastAction => {
                let index = self
                    .takeback_history
                    .iter()
                    .rposition(|(actor, _)| *actor == player)
                    .ok_or_else(|| {
                        "There is no previous action of yours to take back".to_string()
                    })?;
                (self.takeback_history[index].1.clone(), index)
            }
            RewindTarget::TurnStart { turn_number } => {
                // Refuse explicitly rather than letting an always-empty ring
                // produce a "no longer available" miss. This is a system
                // boundary, and a refusal that depends on a collection being
                // empty is untestably vacuous.
                if !offers_turn_rewind(self.hosting) {
                    return Err("Turn rewind is not available in this game".to_string());
                }
                let target_state = self
                    .turn_rewind_history
                    .iter()
                    .find(|snapshot| snapshot.turn_number == turn_number)
                    .ok_or_else(|| "That turn is no longer available to rewind to".to_string())?
                    .clone();
                // An exact prefix predicate: within a game the action ring is
                // chronological and `turn_number` is monotone, so every entry
                // recorded before turn `n` began is an ancestor of turn `n`'s
                // opening state, and every entry from turn `n` onwards is not.
                let history_truncate_len = self
                    .takeback_history
                    .partition_point(|(_, state)| state.turn_number < turn_number);
                (target_state, history_truncate_len)
            }
        };

        let mut approvals = HashSet::new();
        approvals.insert(player);
        self.pending_takeback = Some(PendingTakeback {
            requested_by: player,
            target_state,
            approvals,
            history_truncate_len,
        });

        Ok(self
            .try_resolve_pending_takeback()
            .unwrap_or(TakebackOutcome::Pending))
    }

    /// A human player approves or declines the pending takeback request.
    /// A single decline withdraws the request outright (unanimity required).
    pub fn respond_takeback(
        &mut self,
        player: PlayerId,
        approve: bool,
    ) -> Result<TakebackOutcome, String> {
        self.reject_if_ai_driver_faulted()?;
        if self.pending_takeback.is_none() {
            return Err("There is no pending takeback request".to_string());
        }
        if !self.human_seats().contains(&player) {
            return Err("Only human players may respond to a takeback request".to_string());
        }

        if !approve {
            self.pending_takeback = None;
            return Ok(TakebackOutcome::Rejected);
        }

        if let Some(pending) = self.pending_takeback.as_mut() {
            pending.approvals.insert(player);
        }
        Ok(self
            .try_resolve_pending_takeback()
            .unwrap_or(TakebackOutcome::Pending))
    }

    /// The original requester withdraws their own pending takeback request.
    pub fn cancel_takeback(&mut self, player: PlayerId) -> Result<(), String> {
        self.reject_if_ai_driver_faulted()?;
        match &self.pending_takeback {
            Some(pending) if pending.requested_by == player => {
                self.pending_takeback = None;
                Ok(())
            }
            Some(_) => Err("Only the player who requested the takeback may cancel it".to_string()),
            None => Err("There is no pending takeback request".to_string()),
        }
    }

    /// The `TakebackRequested` notification for the current pending request,
    /// if any. Used both for the original broadcast when a request goes out
    /// and to replay the same prompt to a socket that (re)connects while the
    /// vote is still in flight — otherwise a disconnected approver comes
    /// back with no way to respond, and `handle_action` rejects all actions
    /// while a request is pending, stalling the table.
    pub fn pending_takeback_message(&self) -> Option<crate::protocol::ServerMessage> {
        let pending = self.pending_takeback.as_ref()?;
        let requester_name = self
            .display_names
            .get(pending.requested_by.0 as usize)
            .cloned()
            .unwrap_or_default();
        Some(crate::protocol::ServerMessage::TakebackRequested {
            requester: pending.requested_by,
            requester_name,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A state that has come to rest in `turn_number`, with `active_player`
    /// distinct from seat 0 so a label mix-up is visible.
    fn resting_at(turn_number: u32) -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = turn_number;
        state.active_player = PlayerId(1);
        state
    }

    fn turn_started(turn_number: u32) -> GameEvent {
        GameEvent::TurnStarted {
            player_id: PlayerId(1),
            turn_number,
        }
    }

    /// R1. The capture rule is driven by the transition's own post-state: a
    /// batch that announces turn 4 and comes to rest in turn 4 is recorded, and
    /// the entry is labelled from the stored state's own fields.
    #[test]
    fn record_turn_rewind_point_stores_the_boundary_it_came_to_rest_in() {
        let mut ring = VecDeque::new();
        record_turn_rewind_point(
            &mut ring,
            HostingMode::SingleUser,
            &[turn_started(4)],
            &resting_at(4),
        );
        assert_eq!(ring.len(), 1);
        assert_eq!(ring[0].turn_number, 4);
        assert_eq!(ring[0].active_player, PlayerId(1));
    }

    /// R1 hostile A. A batch that both *started* and *ended* a turn leaves no
    /// state that could be labelled truthfully, so the boundary is refused
    /// rather than mislabelled. Without the `started != state.turn_number`
    /// guard this stores a snapshot of turn 5 under the label "turn 4".
    #[test]
    fn a_batch_that_started_and_ended_a_turn_records_nothing() {
        let mut ring = VecDeque::new();
        record_turn_rewind_point(
            &mut ring,
            HostingMode::SingleUser,
            &[turn_started(4)],
            &resting_at(5),
        );
        assert!(ring.is_empty());
    }

    /// R1 hostile B. Two boundaries in one batch must produce exactly one
    /// entry, taken from the LAST announcement — the one the state rests in.
    #[test]
    fn a_batch_carrying_two_boundaries_records_only_the_one_it_rests_in() {
        let mut ring = VecDeque::new();
        record_turn_rewind_point(
            &mut ring,
            HostingMode::SingleUser,
            &[turn_started(4), turn_started(5)],
            &resting_at(5),
        );
        assert_eq!(
            ring.len(),
            1,
            "no duplicate entry for the passed-through turn"
        );
        assert_eq!(ring[0].turn_number, 5);
    }

    /// R1 hostile C. A batch with no boundary at all leaves the ring alone.
    #[test]
    fn a_batch_with_no_turn_boundary_records_nothing() {
        let mut ring = VecDeque::new();
        record_turn_rewind_point(
            &mut ring,
            HostingMode::SingleUser,
            &[GameEvent::PriorityPassed {
                player_id: PlayerId(0),
            }],
            &resting_at(4),
        );
        assert!(ring.is_empty());
    }

    /// R2. Capture — not just publication — is scoped to the sidecar, so a
    /// shared server retains zero turn snapshots. R1's positive case is the
    /// reach guard: the identical fixture DOES store under `SingleUser`, so
    /// this assertion is not vacuous.
    #[test]
    fn turn_rewind_capture_is_silent_on_a_shared_host() {
        let mut ring = VecDeque::new();
        record_turn_rewind_point(
            &mut ring,
            HostingMode::Shared,
            &[turn_started(4)],
            &resting_at(4),
        );
        assert!(ring.is_empty());
        assert!(!offers_turn_rewind(HostingMode::Shared));
        assert!(offers_turn_rewind(HostingMode::SingleUser));
    }

    /// R3. The ring is bounded and drops the OLDEST entry. Asserting both ends
    /// is what fails a `pop_back` inversion.
    #[test]
    fn turn_rewind_ring_is_bounded_and_drops_oldest() {
        let mut ring = VecDeque::new();
        let overflow = MAX_TURN_REWIND_HISTORY as u32 + 2;
        for turn in 1..=overflow {
            record_turn_rewind_point(
                &mut ring,
                HostingMode::SingleUser,
                &[turn_started(turn)],
                &resting_at(turn),
            );
        }
        assert_eq!(ring.len(), MAX_TURN_REWIND_HISTORY);
        assert_eq!(
            ring.front().expect("bounded ring is non-empty").turn_number,
            3,
            "the two oldest turns must have been dropped from the FRONT"
        );
        assert_eq!(
            ring.back().expect("bounded ring is non-empty").turn_number,
            overflow,
            "the newest turn must be at the BACK"
        );
    }
}
