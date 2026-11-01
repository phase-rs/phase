//! "Request takeback" — multiplayer-safe undo (GH #1507).
//!
//! Single-player/local undo (`engine-wasm::restore_game_state`) replaces the
//! client's own state wholesale and is refused outright in multiplayer
//! sessions, because no client may unilaterally rewrite the authoritative
//! server state another player is relying on. Takeback gives multiplayer
//! sessions an equivalent escape hatch for misclicks and UI confusion; its
//! rules live in `engine::game::takeback::TakebackLedger`. This module keeps
//! only what is server-specific: the turn-rewind policy, the AI-driver fault
//! guard, interaction rebinding and the AI rebuild after an approved rollback,
//! and the display-name prompt.
//!
//! This is intentionally a session/room-level concern, not an engine rule —
//! there is no Comprehensive Rules concept of "undo." The engine is never
//! told why it was handed an older `GameState`; it just continues from
//! whatever state it's given, exactly as it does on reconnect/restore.

use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use phase_ai::session::AiSession;

pub use engine::game::takeback::{
    RewindOption, RewindTarget, TakebackLedger, TakebackOutcome, TakebackResolution,
    MAX_TAKEBACK_HISTORY, MAX_TURN_REWIND_HISTORY,
};

use crate::session::{bind_fresh_interaction_session, GameSession, HostingMode};

/// Whether this deployment offers turn-granular rewind.
///
/// A **policy**, not a capability of the mechanism: the machinery is
/// mode-agnostic and `LastAction` is deliberately *not* scoped by it. Nobody
/// asked for a table-wide vote at turn scale, and the trigger surface
/// (`DebugPanel`) is not gated by `showSandboxTools` — it is reachable in every
/// mode via the keyboard shortcut. Gating capture as well as publication means
/// a shared server retains zero turn snapshots rather than retaining snapshots
/// it will never offer.
pub fn offers_turn_rewind(hosting: HostingMode) -> bool {
    hosting == HostingMode::SingleUser
}

/// The ledger a session hosted under `hosting` keeps: with a turn ring iff
/// [`offers_turn_rewind`].
pub(crate) fn takeback_ledger_for(hosting: HostingMode, game_number: u8) -> TakebackLedger {
    if offers_turn_rewind(hosting) {
        TakebackLedger::with_turn_rewind(game_number)
    } else {
        TakebackLedger::new(game_number)
    }
}

impl GameSession {
    /// Records the current authoritative state as a takeback checkpoint,
    /// tagged with the player about to act.
    pub fn push_takeback_snapshot(&mut self, actor: PlayerId) {
        self.push_takeback_state(actor, self.state.clone());
    }

    /// Records the pre-action state for an action that has already succeeded.
    pub fn push_takeback_state(&mut self, actor: PlayerId, state: GameState) {
        self.takeback.record_point(actor, state);
    }

    /// The per-transition rewind bookkeeping. Every authoritative state
    /// transition calls this with **its own** event batch and post-state —
    /// never with `self.state`, which several call sites have already advanced
    /// past the transition being recorded.
    pub fn observe_transition(&mut self, events: &[GameEvent], state: &GameState) {
        self.takeback.observe_transition(events, state);
    }

    /// The turn boundaries this session currently offers, oldest first.
    pub fn rewind_options(&self) -> Vec<RewindOption> {
        self.takeback.rewind_options()
    }

    /// Human (non-AI) seats in this session, in seat order. AI seats never
    /// request, approve, or block a takeback — they have no UI to misclick.
    pub fn human_seats(&self) -> Vec<PlayerId> {
        (0..self.player_count)
            .map(PlayerId)
            .filter(|p| !self.ai_seats.contains(p))
            .collect()
    }

    pub(crate) fn ai_seat_list(&self) -> Vec<PlayerId> {
        self.ai_seats.iter().copied().collect()
    }

    /// The server-side half of an approved rollback, after the ledger has
    /// installed the target into `self.state`.
    pub(crate) fn after_takeback_approved(&mut self) {
        bind_fresh_interaction_session(&mut self.state, &self.game_code);
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
    }

    /// A human player requests a rollback; see `TakebackLedger::request`.
    pub fn request_takeback(
        &mut self,
        player: PlayerId,
        target: RewindTarget,
    ) -> Result<TakebackOutcome, String> {
        self.reject_if_ai_driver_faulted()?;
        let ai_seats = self.ai_seat_list();
        let outcome = self
            .takeback
            .request(&mut self.state, player, target, &ai_seats)?;
        if outcome == TakebackOutcome::Resolved(TakebackResolution::Approved) {
            self.after_takeback_approved();
        }
        Ok(outcome)
    }

    /// A voter approves or declines the pending takeback request.
    pub fn respond_takeback(
        &mut self,
        player: PlayerId,
        approve: bool,
    ) -> Result<TakebackOutcome, String> {
        self.reject_if_ai_driver_faulted()?;
        let ai_seats = self.ai_seat_list();
        let outcome = self
            .takeback
            .respond(&mut self.state, player, approve, &ai_seats)?;
        if outcome == TakebackOutcome::Resolved(TakebackResolution::Approved) {
            self.after_takeback_approved();
        }
        Ok(outcome)
    }

    /// The original requester withdraws their own pending takeback request.
    pub fn cancel_takeback(&mut self, player: PlayerId) -> Result<(), String> {
        self.reject_if_ai_driver_faulted()?;
        self.takeback.cancel(player)
    }

    /// The `TakebackRequested` notification for the current pending request,
    /// if any. Used both for the original broadcast when a request goes out
    /// and to replay the same prompt to a socket that (re)connects while the
    /// vote is still in flight — otherwise a disconnected approver comes
    /// back with no way to respond, and `handle_action` rejects every action
    /// but a concession while a request is pending, stalling the table.
    pub fn pending_takeback_message(&self) -> Option<crate::protocol::ServerMessage> {
        let vote = self
            .takeback
            .pending_vote(&self.state, &self.ai_seat_list())?;
        let requester = vote.requester;
        let requester_name = self
            .display_names
            .get(requester.0 as usize)
            .cloned()
            .unwrap_or_default();
        Some(crate::protocol::ServerMessage::TakebackRequested {
            requester,
            requester_name,
            voters: vote.voters,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Capture, not just publication, is scoped to the sidecar: only a
    /// `SingleUser` host builds a ledger with a turn ring.
    #[test]
    fn turn_rewind_is_offered_only_on_a_single_user_host() {
        assert!(!offers_turn_rewind(HostingMode::Shared));
        assert!(offers_turn_rewind(HostingMode::SingleUser));
    }
}
