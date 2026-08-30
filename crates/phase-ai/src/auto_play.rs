use std::collections::{HashMap, HashSet};

use engine::ai_support::AiDecisionContract;
use engine::game::engine::{
    apply_interaction, apply_verified_ai_priority_pass, verified_ai_stack_pass_player, EngineError,
};
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

/// Identifies whether a batch bound belongs to the caller or to the module's
/// infinite-loop safety cap. A caller requesting exactly the cap remains a
/// caller budget; only a larger request is truncated by the safety cap.
#[derive(Clone, Copy)]
enum ActionLimit {
    SafetyCap,
    CallerBudget { requested: usize },
}

impl ActionLimit {
    fn effective(self) -> usize {
        match self {
            Self::SafetyCap => MAX_AI_ACTIONS_PER_SEQUENCE,
            Self::CallerBudget { requested } => requested.min(MAX_AI_ACTIONS_PER_SEQUENCE),
        }
    }

    fn is_safety_cap(self) -> bool {
        match self {
            Self::SafetyCap => true,
            Self::CallerBudget { requested } => requested > MAX_AI_ACTIONS_PER_SEQUENCE,
        }
    }
}

/// Result of a single AI action: the action taken and the resulting events.
pub struct AiActionResult {
    pub action: GameAction,
    pub state: GameState,
    pub events: Vec<GameEvent>,
    pub log_entries: Vec<GameLogEntry>,
}

/// Why an AI action batch stopped.
///
/// Diagnostic surface for phase#6080 (the driver-stall family): today the only
/// signal at these break points is a `tracing::error`/`tracing::warn` that no
/// harness subscriber captures. Exposing the reason as typed data lets a
/// caller like `ai_commander` print it instead of installing a subscriber.
#[derive(Debug, Clone)]
pub enum AiActionsStop {
    /// No AI seat can currently act. Two causes are still folded together
    /// here: `WaitingFor::acting_players()` returned empty (`GameOver`, or an
    /// empty pending set), or it returned one or more players and none of
    /// their `turn_control::authorized_submitter_for_player` mappings is in
    /// `ai_players` (a human seat, or a human turn-decision controller).
    /// Deliberately carries no `PlayerId`: the first cause has no player at
    /// all, and the simultaneous-decision variants (`MulliganDecision`,
    /// `OpeningHandBottomCards`) can pend several at once, so naming one
    /// would be arbitrary. A missing AI *configuration* is `MissingAiConfig`.
    NoEligibleAiActor,
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
    /// The module-wide safety budget was exhausted while an AI-controlled
    /// submitter still had work. This is a driver failure, not a benign handoff.
    ActionSafetyCapReached { limit: usize },
    /// A caller-provided smaller budget was exhausted while an AI-controlled
    /// submitter still had work. The caller owns continuation from this point.
    ActionBudgetReached { limit: usize },
}

/// Outcome of a `run_ai_actions` batch.
///
/// `Deref`s to `Vec<AiActionResult>` so callers that only care about actions
/// taken (`.is_empty()`, `.len()`, indexing, iterating by reference) remain
/// source-compatible.
pub struct AiActionsRun {
    pub results: Vec<AiActionResult>,
    pub stop: AiActionsStop,
}

fn eligible_ai_decision(
    state: &GameState,
    ai_players: &HashSet<PlayerId>,
) -> Option<(PlayerId, PlayerId)> {
    state
        .waiting_for
        .acting_players()
        .into_iter()
        .find_map(|semantic_owner| {
            let actor = turn_control::authorized_submitter_for_player(state, semantic_owner);
            ai_players
                .contains(&actor)
                .then_some((semantic_owner, actor))
        })
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
    // Thin delegate: existing callers get the full safety-cap budget and
    // exactly the prior semantics.
    run_ai_actions_with_limit(
        state,
        ai_players,
        ai_configs,
        rng,
        session,
        ActionLimit::SafetyCap,
    )
}

/// Run AI actions like [`run_ai_actions`], but with a caller-supplied upper
/// bound on how many actions the batch may take.
///
/// The effective bound is `min(max_actions, MAX_AI_ACTIONS_PER_SEQUENCE)`: the
/// module's safety cap remains the single authority — a caller can *shrink* a
/// batch below it (to honor an action budget) but never *enlarge* one past it.
/// This function never returns more than that many `AiActionResult`s.
///
/// `max_actions == 0` returns `ActionBudgetReached { limit: 0 }` — no actor is
/// inspected. A caller that loops on this function must therefore guarantee a
/// positive budget before each call.
///
/// The "hit safety cap" warning stays keyed to `MAX_AI_ACTIONS_PER_SEQUENCE`,
/// not `max_actions`: a small operator budget reaching its bound is expected,
/// not a pathological infinite loop, so the warning is naturally silent whenever
/// the clamp is the lower of the two.
pub fn run_ai_actions_bounded(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
    max_actions: usize,
) -> AiActionsRun {
    run_ai_actions_with_limit(
        state,
        ai_players,
        ai_configs,
        rng,
        session,
        ActionLimit::CallerBudget {
            requested: max_actions,
        },
    )
}

fn run_ai_actions_with_limit(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
    action_limit: ActionLimit,
) -> AiActionsRun {
    let mut results = Vec::new();
    let limit = action_limit.effective();

    if limit == 0 {
        return AiActionsRun {
            results,
            stop: AiActionsStop::ActionBudgetReached { limit },
        };
    }

    for _ in 0..limit {
        // CR 723.5: Under turn control (Mindslaver, Emrakul), the authorized
        // submitter is the controller — not the active player. Only run AI when
        // that submitter is an AI seat; otherwise wait for the human controller
        // (issue #1189).
        let decision = eligible_ai_decision(state, ai_players);

        let Some((semantic_owner, actor)) = decision else {
            return AiActionsRun {
                results,
                stop: AiActionsStop::NoEligibleAiActor,
            };
        };

        let config = match ai_configs.get(&actor) {
            Some(c) => c,
            None => {
                tracing::warn!(player = ?actor, "AI seat has no config — stopping AI loop");
                return AiActionsRun {
                    results,
                    stop: AiActionsStop::MissingAiConfig { player: actor },
                };
            }
        };

        let contract = AiDecisionContract::issue(state, semantic_owner);
        let action = match choose_action_with_session(state, semantic_owner, config, rng, session) {
            Some(a) => a,
            None => {
                tracing::warn!(player = ?actor, "choose_action returned None — stopping AI loop");
                return AiActionsRun {
                    results,
                    stop: AiActionsStop::ChooseActionNone { player: actor },
                };
            }
        };

        // `verified_ai_stack_pass_player` is the single authority for this
        // classification and is what `apply_verified_ai_priority_pass` itself
        // gates on, so this router cannot drift from the boundary it selects.
        // Everything it rejects belongs on the ordinary `apply_interaction`
        // boundary — including a payment finalize, which the reducer routes to
        // `finalize_mana_payment` (CR 601.2h).
        let is_stack_recheck_pass = verified_ai_stack_pass_player(state, &action).is_some();
        // Note the coupling: narrowing the classification above also brings a
        // payment finalize back under `contract.permits`, which it bypassed
        // while it was misclassified. `permits` additionally requires
        // `state_revision` to still match, so a finalize proposed against a
        // superseded state is now refused here rather than reaching the
        // reducer — the same staleness discipline every other action already
        // gets, not a special case.
        if !is_stack_recheck_pass && !contract.permits(state, actor, &action) {
            let error = EngineError::InvalidAction(
                "AI chose an action outside its issued decision contract".to_string(),
            );
            tracing::error!(
                ?semantic_owner,
                ?actor,
                "AI action violated decision contract"
            );
            return AiActionsRun {
                results,
                stop: AiActionsStop::ApplyFailed {
                    player: actor,
                    action: Box::new(action),
                    error,
                },
            };
        }

        // The decision owner and authenticated AI actor are intentionally
        // separate: control effects can make one player submit another
        // player's pending choice.
        let applied = if is_stack_recheck_pass {
            apply_verified_ai_priority_pass(state, actor, &contract, action.clone())
        } else {
            apply_interaction(state, actor, semantic_owner, action.clone())
        };
        match applied {
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
                return AiActionsRun {
                    results,
                    stop: AiActionsStop::ApplyFailed {
                        player: actor,
                        action: Box::new(action),
                        error: e,
                    },
                };
            }
        }
    }

    if action_limit.is_safety_cap() {
        tracing::warn!(
            count = limit,
            "AI action loop hit safety cap — possible infinite loop"
        );
    }

    AiActionsRun {
        results,
        stop: if eligible_ai_decision(state, ai_players).is_none() {
            AiActionsStop::NoEligibleAiActor
        } else if action_limit.is_safety_cap() {
            AiActionsStop::ActionSafetyCapReached { limit }
        } else {
            AiActionsStop::ActionBudgetReached { limit }
        },
    }
}

/// Driver-relevant outcome of processing one `run_ai_actions` batch: how many
/// actions to add to a caller's running total and its exact stop condition.
///
/// phase#6080 follow-up: a batch can complete one or more actions (`results`
/// non-empty) and *still* carry a stop reason — e.g. it applies two
/// actions, then the third choice is `ChooseActionNone` or the fourth
/// `apply()` call fails. A driver that only inspects the stop reason when
/// `results.is_empty()` silently discards the diagnostic for exactly that
/// case, loops again, and may report a misleading `NoActor`/unknown reason
/// once a later, unrelated batch happens to come back empty. `driver_step`
/// is the single place that decision is made, so callers (and tests) don't
/// re-derive it ad hoc.
pub struct DriverStep {
    pub actions_taken: usize,
    pub stop: AiActionsStop,
}

/// Extracts the [`DriverStep`] for one batch. Callers should process
/// `results`'s individual `AiActionResult`s (logging, animation, dumps)
/// before or after calling this — it only reports the count/stop decision.
pub fn driver_step(results: AiActionsRun) -> DriverStep {
    DriverStep {
        actions_taken: results.results.len(),
        stop: results.stop,
    }
}

/// Why [`run_driver_loop`] returned: it either hit the action cap exactly, or a
/// batch carried a stop reason ([`AiActionsStop`]) at its boundary. One
/// fact, one type — not an `aborted: bool` plus an `Option<AiActionsStop>`
/// pair whose two illegal combinations (aborted with a reason, not-aborted with
/// none) a caller would have to defend against.
#[derive(Debug)]
pub enum DriverExit {
    /// `total_actions` reached `action_cap` with no break door firing first.
    CapReached,
    /// A batch stopped early at its boundary; carries the reason to report.
    BatchBreak(AiActionsStop),
}

/// Outcome of a [`run_driver_loop`] run: the total actions taken and why it
/// stopped.
#[derive(Debug)]
pub struct DriverOutcome {
    pub total_actions: usize,
    pub exit: DriverExit,
}

/// Drives repeated [`run_ai_actions_bounded`] batches until the action cap is
/// reached or a batch breaks, threading the remaining-budget arithmetic that
/// keeps a small `action_cap` from being overshot. This is the single authority
/// for the batch / remaining-budget boundary: `ai_commander`'s `main` and the
/// regression tests both drive it, so the exact-cap contract is exercised on the
/// production path rather than re-derived in a test-only mirror loop.
///
/// # Exact-cap contract
/// `DriverOutcome::total_actions` never exceeds `action_cap`. Each batch is
/// bounded to the *remaining* budget (`action_cap - total`), so the loop stops
/// exactly at the cap instead of overshooting by up to
/// `MAX_AI_ACTIONS_PER_SEQUENCE` within a final batch. This deliberately differs
/// from `duel_suite`'s `drive_game_observed`, whose documented contract checks
/// the cap only at batch boundaries and may overshoot within a batch — that
/// overshoot is baseline-witnessed and intentional there, so this helper must
/// not be "helpfully" unified with it.
///
/// # Observer contract
/// `on_batch(&mut results, &state, total)` fires exactly once per batch, with:
/// the batch results *mutably* (so the observer can `mem::take` per-action log
/// vectors while draining dumps — this is why the seam is `&mut`, unlike
/// `drive_game_observed`'s immutable `&GameState`-only observer); the
/// post-batch `state` immutably; and the *pre-batch* running `total` (turn and
/// ELIMINATED numbering in `ai_commander` depend on the pre-batch total, so the
/// observer must see the count as it stood before this batch's actions).
///
/// # Caller contract
/// `action_cap >= 1`. `ai_commander`'s CLI parsing guarantees this; the
/// `debug_assert!` enforces it in tests. `remaining` is a plain subtraction (not
/// `saturating_sub`): the `CapReached` abort door below breaks at
/// `total >= action_cap`, so `remaining >= 1` whenever the loop body runs. An
/// underflow here would be a real invariant violation and must panic rather than
/// be silently masked into a zero-budget no-op.
pub fn run_driver_loop(
    state: &mut GameState,
    ai_players: &HashSet<PlayerId>,
    ai_configs: &HashMap<PlayerId, AiConfig>,
    rng: &mut impl Rng,
    session: &Arc<AiSession>,
    action_cap: usize,
    on_batch: &mut dyn FnMut(&mut AiActionsRun, &GameState, usize),
) -> DriverOutcome {
    debug_assert!(action_cap > 0);
    let mut total: usize = 0;
    loop {
        let remaining = action_cap - total;
        let mut results =
            run_ai_actions_bounded(state, ai_players, ai_configs, rng, session, remaining);
        on_batch(&mut results, &*state, total);

        let step = driver_step(results);
        total += step.actions_taken;
        match step.stop {
            AiActionsStop::ActionBudgetReached { .. }
            | AiActionsStop::ActionSafetyCapReached { .. }
                if total >= action_cap =>
            {
                return DriverOutcome {
                    total_actions: total,
                    exit: DriverExit::CapReached,
                };
            }
            AiActionsStop::ActionSafetyCapReached { .. } if total < action_cap => continue,
            stop => {
                return DriverOutcome {
                    total_actions: total,
                    exit: DriverExit::BatchBreak(stop),
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{
        StackEntry, StackEntryKind, StackResolutionPolicy, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    fn recheck_priority_state() -> GameState {
        let mut state = GameState::new_two_player(1);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state.stack.push_back(StackEntry {
            id: ObjectId(70_001),
            source_id: ObjectId(70_001),
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(70_001),
                ability: Box::new(engine::types::ability::ResolvedAbility::new(
                    Effect::NoOp,
                    Vec::new(),
                    ObjectId(70_001),
                    PlayerId(1),
                )),
            },
        });
        let ability_source = create_object(
            &mut state,
            CardId(70_002),
            PlayerId(1),
            "AI Recheck Action".to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&ability_source)
            .expect("created battlefield object");
        object.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        state
    }

    fn push_no_op_stack_entry(state: &mut GameState, id: u64) {
        state.stack.push_back(StackEntry {
            id: ObjectId(id),
            source_id: ObjectId(id),
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(id),
                ability: Box::new(engine::types::ability::ResolvedAbility::new(
                    Effect::NoOp,
                    Vec::new(),
                    ObjectId(id),
                    PlayerId(1),
                )),
            },
        });
    }

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
        // have its stop reason discarded just because `results` isn't
        // empty.
        let state = GameState::new_two_player(1);
        let run = AiActionsRun {
            results: vec![dummy_result(&state)],
            stop: AiActionsStop::ChooseActionNone {
                player: PlayerId(1),
            },
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 1);
        assert!(
            matches!(step.stop, AiActionsStop::ChooseActionNone { .. }),
            "stop reason from a non-empty batch must survive driver_step"
        );
    }

    #[test]
    fn driver_step_empty_batch_behavior_is_unchanged() {
        // Existing behavior (empty batch + break reason) must still work.
        let run = AiActionsRun {
            results: Vec::new(),
            stop: AiActionsStop::NoEligibleAiActor,
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 0);
        assert!(matches!(step.stop, AiActionsStop::NoEligibleAiActor));
    }

    #[test]
    fn driver_step_preserves_budget_stop() {
        let state = GameState::new_two_player(1);
        let run = AiActionsRun {
            results: vec![dummy_result(&state), dummy_result(&state)],
            stop: AiActionsStop::ActionBudgetReached { limit: 2 },
        };
        let step = driver_step(run);
        assert_eq!(step.actions_taken, 2);
        assert!(matches!(
            step.stop,
            AiActionsStop::ActionBudgetReached { limit: 2 }
        ));
    }

    #[test]
    fn zero_sized_bounded_batch_is_a_normal_budget_stop() {
        let mut state = GameState::new_two_player(1);
        let mut rng = rand::rng();
        let session = AiSession::arc_from_game(&state);
        let run = run_ai_actions_bounded(
            &mut state,
            &HashSet::new(),
            &HashMap::new(),
            &mut rng,
            &session,
            0,
        );

        assert!(run.is_empty());
        assert!(matches!(
            run.stop,
            AiActionsStop::ActionBudgetReached { limit: 0 }
        ));
    }

    #[test]
    fn exact_cap_caller_budget_is_not_a_safety_cap() {
        assert!(!ActionLimit::CallerBudget {
            requested: MAX_AI_ACTIONS_PER_SEQUENCE,
        }
        .is_safety_cap());
        assert!(ActionLimit::SafetyCap.is_safety_cap());
    }

    #[test]
    fn native_ai_pass_on_the_stack_uses_the_verified_recheck_seam() {
        let mut state = recheck_priority_state();
        let ai_players = HashSet::from([PlayerId(0)]);
        let ai_configs = HashMap::from([(PlayerId(0), AiConfig::default())]);
        let session = AiSession::arc_from_game(&state);
        let mut rng = rand::rng();

        let run =
            run_ai_actions_bounded(&mut state, &ai_players, &ai_configs, &mut rng, &session, 1);

        assert!(matches!(
            run.results.as_slice(),
            [AiActionResult {
                action: GameAction::PassPriority,
                ..
            }]
        ));
        assert_eq!(
            state
                .stack_resolution_session
                .as_ref()
                .map(|session| session.policy),
            Some(StackResolutionPolicy::RecheckNoMeaningfulPriorityAction),
            "the native AI runner must use the verified stack-pass seam"
        );
    }

    #[test]
    fn verified_pass_cache_drains_a_large_ai_stack_without_action_cap() {
        let mut state = recheck_priority_state();
        state.objects.clear();
        for id in 70_003..70_204 {
            push_no_op_stack_entry(&mut state, id);
        }
        let ai_players = HashSet::from([PlayerId(0), PlayerId(1)]);
        let ai_configs = HashMap::from([
            (PlayerId(0), AiConfig::default()),
            (PlayerId(1), AiConfig::default()),
        ]);
        let session = AiSession::arc_from_game(&state);
        let mut rng = rand::rng();

        engine::game::perf_counters::reset();
        let run = run_ai_actions(&mut state, &ai_players, &ai_configs, &mut rng, &session);
        let counters = engine::game::perf_counters::snapshot();

        assert!(
            matches!(run.stop, AiActionsStop::NoEligibleAiActor),
            "a completed fenced stack must not hit the generic AI action cap: {:?}",
            run.stop
        );
        assert_eq!(
            run.results.len(),
            26,
            "the fenced cache must keep a 201-entry stack well below the generic action cap"
        );
        assert!(state.stack.is_empty());
        assert!(state.stack_resolution_session.is_none());
        assert_eq!(
            counters.priority_cast_probe_builds, 0,
            "cached verified passes must avoid the recheck probe on every stack entry"
        );
    }
}
