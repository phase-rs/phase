use std::collections::{HashMap, HashSet};

use engine::game::engine::{apply, EngineError};
use engine::game::turn_control;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::log::GameLogEntry;
use engine::types::player::PlayerId;
use rand::Rng;
use std::sync::Arc;

use crate::config::AiConfig;
use crate::search::choose_action_with_session;
use crate::session::AiSession;

/// Maximum AI actions before forcing a stop (safety invariant — not CR-derived).
/// Typical AI sequences (mulligans + full turn) are 30–50 actions.
const MAX_AI_ACTIONS_PER_SEQUENCE: usize = 200;

/// Result of a single AI action: the action taken and the resulting events.
pub struct AiActionResult {
    pub action: GameAction,
    pub state: GameState,
    pub events: Vec<GameEvent>,
    pub log_entries: Vec<GameLogEntry>,
}

/// Which of `run_ai_actions`'s four break doors (see its doc comment) ended
/// the batch before the safety cap. `None` means the loop ran out AI actions
/// to take for a benign reason the caller already distinguishes elsewhere
/// (hit `MAX_AI_ACTIONS_PER_SEQUENCE`, or the very first iteration found no
/// actor at all — that case is folded into `NoActor` below instead, so `None`
/// in practice only means "hit the safety cap").
///
/// Diagnostic surface for phase#6080 (the driver-stall family): today the only
/// signal at these break points is a `tracing::error`/`tracing::warn` that no
/// harness subscriber captures. Exposing the reason as typed data lets a
/// caller like `ai_commander` print it instead of installing a subscriber.
#[derive(Debug, Clone)]
pub enum AiActionsBreakReason {
    /// No AI seat can currently act. Two causes are still folded together
    /// here: `WaitingFor::acting_players()` returned empty (`GameOver`, or an
    /// empty pending set), or it returned one or more players and none of
    /// their `turn_control::authorized_submitter_for_player` mappings is in
    /// `ai_players` (a human seat, or a human turn-decision controller).
    /// Deliberately carries no `PlayerId`: the first cause has no player at
    /// all, and the simultaneous-decision variants (`MulliganDecision`,
    /// `OpeningHandBottomCards`) can pend several at once, so naming one
    /// would be arbitrary. A missing AI *configuration* is `MissingAiConfig`.
    NoActor,
    /// `player` is in `ai_players` but has no entry in `ai_configs`. Distinct
    /// from `NoActor`: an actor *was* found and *is* AI-controlled, so the
    /// remedy is caller wiring (register a config for this seat), not "wait
    /// for a human" or "the game ended".
    MissingAiConfig { player: PlayerId },
    /// `choose_action_with_session` returned `None` for `player` — the AI
    /// policy stack produced no legal action for a decision it was asked to
    /// make.
    ChooseActionNone { player: PlayerId },
    /// `apply()` rejected `player`'s chosen `action`. `action` is boxed because
    /// `GameAction` is large relative to the other variants (clippy
    /// `large_enum_variant`); `EngineError` is four small variants (largest
    /// payload a `String`) and needs no box.
    ApplyFailed {
        player: PlayerId,
        action: Box<GameAction>,
        error: EngineError,
    },
}

/// Outcome of a `run_ai_actions` batch.
///
/// `Deref`s to `Vec<AiActionResult>` so the many existing callers that only
/// care about the actions taken (`.is_empty()`, `.len()`, indexing, iterating
/// by reference) are source-compatible; only diagnostic consumers need
/// `break_reason`.
pub struct AiActionsRun {
    pub results: Vec<AiActionResult>,
    pub break_reason: Option<AiActionsBreakReason>,
}

impl std::ops::Deref for AiActionsRun {
    type Target = Vec<AiActionResult>;
    fn deref(&self) -> &Vec<AiActionResult> {
        &self.results
    }
}

impl IntoIterator for AiActionsRun {
    type Item = AiActionResult;
    type IntoIter = std::vec::IntoIter<AiActionResult>;
    fn into_iter(self) -> Self::IntoIter {
        self.results.into_iter()
    }
}

impl<'a> IntoIterator for &'a AiActionsRun {
    type Item = &'a AiActionResult;
    type IntoIter = std::slice::Iter<'a, AiActionResult>;
    fn into_iter(self) -> Self::IntoIter {
        self.results.iter()
    }
}

impl<'a> IntoIterator for &'a mut AiActionsRun {
    type Item = &'a mut AiActionResult;
    type IntoIter = std::slice::IterMut<'a, AiActionResult>;
    fn into_iter(self) -> Self::IntoIter {
        self.results.iter_mut()
    }
}

/// Run AI actions on the game state until the next actor is human or the game is over.
///
/// Returns one `AiActionResult` per AI action taken, preserving granularity for
/// the caller to broadcast individual state updates with animation timing.
///
/// # Arguments
/// * `state` — mutable game state (modified in place)
/// * `ai_players` — set of AI-controlled player IDs
/// * `ai_configs` — per-player AI configuration
///
/// CR 116.3: AI players receive and pass priority automatically.
/// The loop terminates when a non-AI player receives priority or the game ends.
pub fn run_ai_actions(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
) -> AiActionsRun {
    let mut results = Vec::new();
    let mut break_reason = None;

    for _ in 0..MAX_AI_ACTIONS_PER_SEQUENCE {
        // CR 723.5: Under turn control (Mindslaver, Emrakul), the authorized
        // submitter is the controller — not the active player. Only run AI when
        // that submitter is an AI seat; otherwise wait for the human controller
        // (issue #1189).
        let actor = state
            .waiting_for
            .acting_players()
            .into_iter()
            .map(|player| turn_control::authorized_submitter_for_player(state, player))
            .find(|player| ai_players.contains(player));

        let Some(actor) = actor else {
            break_reason = Some(AiActionsBreakReason::NoActor);
            break;
        };

        let config = match ai_configs.get(&actor) {
            Some(c) => c,
            None => {
                tracing::warn!(player = ?actor, "AI seat has no config — stopping AI loop");
                break_reason = Some(AiActionsBreakReason::MissingAiConfig { player: actor });
                break;
            }
        };

        let action = match choose_action_with_session(state, actor, config, rng, session) {
            Some(a) => a,
            None => {
                tracing::warn!(player = ?actor, "choose_action returned None — stopping AI loop");
                break_reason = Some(AiActionsBreakReason::ChooseActionNone { player: actor });
                break;
            }
        };

        // `actor` is the AI's authenticated PlayerId — we selected the action
        // for this seat and the engine's guard will reject if turn-decision
        // control has shifted in the meantime.
        match apply(state, actor, action.clone()) {
            Ok(result) => {
                results.push(AiActionResult {
                    action,
                    state: state.clone(),
                    events: result.events,
                    log_entries: result.log_entries,
                });
            }
            Err(e) => {
                tracing::error!(player = ?actor, error = %e, "AI action apply failed — stopping");
                break_reason = Some(AiActionsBreakReason::ApplyFailed {
                    player: actor,
                    action: Box::new(action),
                    error: e,
                });
                break;
            }
        }
    }

    if results.len() >= MAX_AI_ACTIONS_PER_SEQUENCE {
        tracing::warn!(
            count = results.len(),
            "AI action loop hit safety cap — possible infinite loop"
        );
    }

    AiActionsRun {
        results,
        break_reason,
    }
}

/// Driver-relevant outcome of processing one `run_ai_actions` batch: how many
/// actions to add to a caller's running total, and the break reason to stop
/// and report at this batch boundary, if the batch carries one.
///
/// phase#6080 follow-up: a batch can complete one or more actions (`results`
/// non-empty) and *still* carry a `break_reason` — e.g. it applies two
/// actions, then the third choice is `ChooseActionNone` or the fourth
/// `apply()` call fails. A driver that only inspects `break_reason` when
/// `results.is_empty()` silently discards the diagnostic for exactly that
/// case, loops again, and may report a misleading `NoActor`/unknown reason
/// once a later, unrelated batch happens to come back empty. `driver_step`
/// is the single place that decision is made, so callers (and tests) don't
/// re-derive it ad hoc.
pub struct DriverStep {
    pub actions_taken: usize,
    pub break_reason: Option<AiActionsBreakReason>,
}

/// Extracts the [`DriverStep`] for one batch. Callers should process
/// `results`'s individual `AiActionResult`s (logging, animation, dumps)
/// before or after calling this — it only reports the count/stop decision.
pub fn driver_step(results: AiActionsRun) -> DriverStep {
    DriverStep {
        actions_taken: results.results.len(),
        break_reason: results.break_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_result(state: &GameState) -> AiActionResult {
        AiActionResult {
            action: GameAction::PassPriority,
            state: state.clone(),
            events: Vec::new(),
            log_entries: Vec::new(),
        }
    }

    #[test]
    fn driver_step_preserves_break_reason_from_non_empty_batch() {
        // The exact regression: a batch that completed an action must not
        // have its break_reason discarded just because `results` isn't
        // empty.
        let state = GameState::new_two_player(1);
        let run = AiActionsRun {
            results: vec![dummy_result(&state)],
            break_reason: Some(AiActionsBreakReason::ChooseActionNone {
                player: PlayerId(1),
            }),
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 1);
        assert!(
            matches!(
                step.break_reason,
                Some(AiActionsBreakReason::ChooseActionNone { .. })
            ),
            "break_reason from a non-empty batch must survive driver_step"
        );
    }

    #[test]
    fn driver_step_empty_batch_behavior_is_unchanged() {
        // Existing behavior (empty batch + break reason) must still work.
        let run = AiActionsRun {
            results: Vec::new(),
            break_reason: Some(AiActionsBreakReason::NoActor),
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 0);
        assert!(matches!(
            step.break_reason,
            Some(AiActionsBreakReason::NoActor)
        ));
    }

    #[test]
    fn driver_step_ordinary_batch_is_unaffected() {
        // Ordinary caller path: batch completed actions and hit no break
        // door (e.g. hit the safety cap) — driver_step must not fabricate a
        // stop signal.
        let state = GameState::new_two_player(1);
        let run = AiActionsRun {
            results: vec![dummy_result(&state), dummy_result(&state)],
            break_reason: None,
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 2);
        assert!(step.break_reason.is_none());
    }
}
