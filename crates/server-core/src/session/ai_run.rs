//! Resumable native AI driving without changing the authoritative action boundary.
//!
//! A transport retains its game's exclusive ordering lease across every yield.
//! The cursor preserves the sequence-wide safety limit and transition history;
//! only completion attaches a decision to the final original state snapshot.

use std::ops::ControlFlow;

use phase_ai::auto_play::{run_ai_action, AiActionsStop, MAX_AI_ACTIONS_PER_SEQUENCE};

use super::{AiDriverFailure, AiRunOutcome, AiTransition, DecisionSnapshot, GameSession};

/// Opaque progress for one AI sequence. Obtain it through
/// [`GameSession::begin_ai_run`] and retain the same game's exclusive lease
/// until [`GameSession::advance_ai_run`] returns `ControlFlow::Break`.
#[derive(Debug)]
pub struct AiRunCursor {
    transitions: Vec<AiTransition>,
}

impl GameSession {
    /// Perform the existing cheap driver gates before admitting compute.
    /// A pending takeback must never be advanced by an AI continuation.
    pub fn begin_ai_run(&self) -> ControlFlow<AiRunOutcome, AiRunCursor> {
        if let Some(fault) = self.ai_driver_fault.clone() {
            return ControlFlow::Break(AiRunOutcome {
                transitions: Vec::new(),
                failure: None,
                fault: Some(fault),
            });
        }
        if self.ai_seats.is_empty() || self.pending_takeback.is_some() || !self.ai_seat_can_act() {
            return ControlFlow::Break(AiRunOutcome {
                transitions: Vec::new(),
                failure: None,
                fault: None,
            });
        }
        ControlFlow::Continue(AiRunCursor {
            transitions: Vec::new(),
        })
    }

    /// Choose and apply at most one AI action, then yield CPU admission.
    /// The caller must not release the game's ordering lease between calls.
    pub fn advance_ai_run(
        &mut self,
        mut cursor: AiRunCursor,
    ) -> ControlFlow<AiRunOutcome, AiRunCursor> {
        // Native AI choice randomness is intentionally unseeded, as in the
        // synchronous driver. Never move a !Send ThreadRng between workers;
        // authoritative game randomness remains owned by self.state.
        let mut rng = rand::rng();
        let ai_session = self
            .ai_session
            .get_or_insert_with(|| phase_ai::session::AiSession::arc_from_game(&self.state));
        match run_ai_action(
            &mut self.state,
            &self.ai_seats,
            &self.ai_configs,
            &mut rng,
            ai_session,
        ) {
            ControlFlow::Continue(result) => {
                let revision = self.advance_state_revision();
                self.observe_transition(&result.events, &result.state);
                self.game_log
                    .write_game_log_entries(&self.game_code, &result.log_entries);
                cursor.transitions.push(AiTransition {
                    state_revision: revision,
                    state: result.state,
                    events: result.events,
                    log_entries: result.log_entries,
                    decision: None,
                });
                let stop = if !self.ai_seat_can_act() {
                    Some(AiActionsStop::NoEligibleAiActor)
                } else if cursor.transitions.len() >= MAX_AI_ACTIONS_PER_SEQUENCE {
                    Some(AiActionsStop::ActionSafetyCapReached {
                        limit: MAX_AI_ACTIONS_PER_SEQUENCE,
                    })
                } else {
                    None
                };
                match stop {
                    Some(stop) => ControlFlow::Break(self.finish_ai_run(cursor, stop)),
                    None => ControlFlow::Continue(cursor),
                }
            }
            ControlFlow::Break(stop) => ControlFlow::Break(self.finish_ai_run(cursor, stop)),
        }
    }

    fn finish_ai_run(&mut self, mut cursor: AiRunCursor, stop: AiActionsStop) -> AiRunOutcome {
        if let Some(last) = cursor.transitions.last_mut() {
            last.decision = Some(DecisionSnapshot::from_state(&last.state));
        }
        let failure = match stop {
            AiActionsStop::NoEligibleAiActor | AiActionsStop::ActionBudgetReached { .. } => None,
            AiActionsStop::MissingAiConfig { player } => {
                Some(AiDriverFailure::MissingAiConfig { player })
            }
            AiActionsStop::ChooseActionNone { player } => {
                Some(AiDriverFailure::ChooseActionNone { player })
            }
            AiActionsStop::ApplyFailed { player, error, .. } => {
                Some(AiDriverFailure::ApplyFailed {
                    player,
                    error: error.to_string(),
                })
            }
            AiActionsStop::ActionSafetyCapReached { limit } if self.ai_seat_can_act() => {
                Some(AiDriverFailure::ActionSafetyCapReached { limit })
            }
            AiActionsStop::ActionSafetyCapReached { .. } => None,
        };
        let fault = failure
            .clone()
            .map(|failure| self.record_ai_driver_fault(failure));
        if !cursor.transitions.is_empty() {
            tracing::debug!(game = %self.game_code, ai_actions = cursor.transitions.len(), "AI actions computed");
        }
        AiRunOutcome {
            transitions: cursor.transitions,
            failure,
            fault,
        }
    }
}
