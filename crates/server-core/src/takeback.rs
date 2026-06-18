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
//! This is intentionally a session/room-level concern, not an engine rule —
//! there is no Comprehensive Rules concept of "undo." The engine is never
//! told why it was handed an older `GameState`; it just continues from
//! whatever state it's given, exactly as it does on reconnect/restore.

use std::collections::HashSet;

use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use crate::session::GameSession;

/// How many prior authoritative snapshots a session retains for takeback
/// purposes. Bounded so a long game session can't accumulate unbounded
/// memory — a takeback can only reach back to the most recent action, so
/// anything beyond a handful of entries is never reachable anyway.
pub const MAX_TAKEBACK_HISTORY: usize = 12;

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
    /// tagged with the player about to act — to be called immediately
    /// before that player's action is applied. Caps retention at
    /// [`MAX_TAKEBACK_HISTORY`] (oldest dropped first).
    ///
    /// Tagging by actor (rather than just "most recent action") is what lets
    /// `request_takeback` find *this player's* last action even when other
    /// players have acted since — see its doc comment.
    pub fn push_takeback_snapshot(&mut self, actor: PlayerId) {
        if self.takeback_history.len() >= MAX_TAKEBACK_HISTORY {
            self.takeback_history.pop_front();
        }
        self.takeback_history.push_back((actor, self.state.clone()));
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
        // The rolled-back state is the new baseline; snapshots from the
        // branch we just discarded no longer correspond to reachable
        // history, and taking another takeback back through them would
        // resurrect actions the table just agreed to undo.
        self.takeback_history.clear();
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
    pub fn request_takeback(&mut self, player: PlayerId) -> Result<TakebackOutcome, String> {
        if self.pending_takeback.is_some() {
            return Err("A takeback request is already pending for this game".to_string());
        }
        if !self.human_seats().contains(&player) {
            return Err("Only human players may request a takeback".to_string());
        }
        let target_state = self
            .takeback_history
            .iter()
            .rev()
            .find(|(actor, _)| *actor == player)
            .map(|(_, state)| state.clone())
            .ok_or_else(|| "There is no previous action of yours to take back".to_string())?;

        let mut approvals = HashSet::new();
        approvals.insert(player);
        self.pending_takeback = Some(PendingTakeback {
            requested_by: player,
            target_state,
            approvals,
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
