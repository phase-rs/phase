//! Horizon-parameterized projection of the game state into an opponent's
//! upcoming combat. Used by `combat_ai` and `eval` to read opponent creature
//! power/toughness as it will be when they actually attack, not as it is now.
//!
//! The primitive clones the state, advances through the real engine reducer
//! until a requested horizon on a specified opponent's next turn, and returns
//! the projected state. Phase-based growth triggers (Ouroboroid), attack-
//! declaration triggers (Battle Cry, Mentor), and combat-damage riders all
//! fire naturally because the engine does the work — no reimplementation
//! of trigger effects in the AI layer.

use std::collections::HashMap;

use engine::ai_support::{
    classify_payment_continuation, legal_actions, witness_payment_continuations,
    PaymentContinuationBatchStatus, PaymentContinuationState,
};
use engine::game::combat::AttackTarget;
use engine::game::engine::{apply_interaction_for_simulation, EngineError};
use engine::game::priority;
use engine::game::turn_control;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt};
use engine::types::{
    CoreType, GameAction, GameState, ObjectId, PayCostKind, Phase, PlayerId, WaitingFor,
};
use engine::util::Deadline;

use web_time::{Duration, Instant};

use crate::config::ExecutionMode;

/// How far into the opponent's upcoming turn to project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProjectionHorizon {
    /// Phase-based growth only (Ouroboroid, sagas).
    OpponentBeginCombat,
    /// Adds attack-declaration triggers (Battle Cry, Mentor, Hellrider).
    OpponentAttackersDeclared,
    /// Adds first combat damage step (v0: no-blocks baseline).
    OpponentCombatDamage,
}

/// Why the projection could not reach the requested horizon.
#[derive(Debug, Clone)]
pub enum BailReason {
    StepCapExceeded { steps: u32 },
    TimeCapExceeded { elapsed: Duration },
    GameOverDuringProjection,
    MulliganOrSideboardEncountered,
    NoLegalAction { waiting_for: String },
    NoLegalManaPayment,
    IncompleteManaPaymentWitness,
    EngineRejected(EngineError),
}

/// Per-creature growth across the horizon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VelocitySample {
    /// Creature survived projection; `delta` may be negative (-1/-1 counters).
    Changed { delta: i32 },
    /// Creature was destroyed or exiled during projection.
    Removed,
    /// Token or creature appeared during projection (e.g., Ophiomancer snake).
    Appeared { projected_power: i32 },
}

/// How certain the projection is.
#[derive(Debug, Clone, Copy)]
pub enum Confidence {
    /// No non-trivial policy choices were required.
    Exact,
    /// One or more choices resolved via the policy — callers should apply
    /// a safety margin.
    Approximated { choice_count: u32 },
}

/// Result of a successful projection.
#[derive(Debug, Clone)]
pub struct Projection {
    pub horizon_reached: ProjectionHorizon,
    pub state: GameState,
    /// States captured at each horizon boundary passed through. Consumers
    /// needing an earlier horizon can read from here without re-projecting.
    pub snapshots: Vec<(ProjectionHorizon, GameState)>,
    pub confidence: Confidence,
    pub target_opponent: PlayerId,
}

impl Projection {
    /// Return the snapshot for a specific horizon, if captured.
    pub fn snapshot(&self, horizon: ProjectionHorizon) -> Option<&GameState> {
        self.snapshots
            .iter()
            .find(|(h, _)| *h == horizon)
            .map(|(_, s)| s)
    }
}

/// Cache-compatible projection key. Turn-in-key makes eviction implicit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectionKey {
    pub state_hash: u64,
    pub turn_number: u32,
    pub active_player: PlayerId,
    pub ai_player: PlayerId,
    pub target_opponent: PlayerId,
    pub horizon: ProjectionHorizon,
}

/// Outer dispatch cap. Each dispatch may trigger up to 500 engine-internal
/// auto-pass iterations.
const STEP_CAP: u32 = 256;
/// Wall-clock guard for projection, in milliseconds. The combat path is a
/// heuristic and must fail closed to the pre-projection behavior rather than
/// monopolize the AI turn when the engine path is unusually expensive.
///
/// Interactive callers only. Measurement mode receives a non-expiring budget
/// from [`projection_deadline`] and is bounded by `STEP_CAP` alone, so `cargo
/// ai-gate` verdicts cannot depend on host speed.
///
/// Deliberately PRIVATE: `projection_deadline` is the only producer of a
/// projection budget, so no other module can construct one from this value.
const TIME_CAP_MS: u32 = 15;

/// The wall-clock budget one `project_to` call runs under.
///
/// **Single source of truth** — no caller writes `TIME_CAP_MS` (it is private)
/// and no caller constructs a projection `Deadline` itself.
///
/// Measurement mode is bounded by `STEP_CAP` only — never wall clock. A bail
/// scores 0.0 where a completed projection scores up to +3.0
/// (`policies::evasion_removal_priority::velocity_score`), and that term selects
/// the removal target, so a clock-dependent bail would make host speed an input
/// to `cargo ai-gate` verdicts. Mirrors
/// `planner::PlannerServices::with_deadline`.
///
/// Call this at the point of consumption — the returned `Deadline` snapshots an
/// absolute instant, so binding it to a `let` that outlives one `project_to`
/// call silently shortens (and eventually zeroes) the budget.
pub fn projection_deadline(execution_mode: ExecutionMode) -> Deadline {
    if execution_mode.is_measurement() {
        Deadline::none()
    } else {
        Deadline::after(TIME_CAP_MS)
    }
}

/// Advance from `base` forward until `horizon` is reached on
/// `target_opponent`'s next turn. `base` is cloned; never mutated.
/// Deterministic given `(base_fingerprint, ai_player, target_opponent, horizon)`
/// **and** a non-expiring `deadline`, which [`projection_deadline`] supplies in
/// measurement mode.
pub fn project_to(
    base: &GameState,
    ai_player: PlayerId,
    target_opponent: PlayerId,
    horizon: ProjectionHorizon,
    deadline: Deadline,
) -> Result<Projection, BailReason> {
    let started_turn = base.turn_number;
    // Diagnostic only: feeds BailReason::TimeCapExceeded's `elapsed`. Never
    // compared, never affects a decision. NOTE: it measures time spent INSIDE
    // project_to, while the budget is anchored a few microseconds earlier at the
    // caller's `projection_deadline(..)` call — so on a bail it under-reports the
    // budget actually consumed by (caller-side hash + lock probe). Bounded by
    // that, and it is a log field only. Unreachable in measurement mode, where
    // `Deadline::none()` never expires.
    let started_at = Instant::now();
    let mut state = base.clone();
    let mut snapshots: Vec<(ProjectionHorizon, GameState)> = Vec::new();
    let mut choice_count: u32 = 0;

    // Already-at-horizon short-circuit.
    if reached_horizon(&state, target_opponent, horizon, started_turn) {
        return Ok(Projection {
            horizon_reached: horizon,
            state: state.clone(),
            snapshots: vec![(horizon, state)],
            confidence: Confidence::Exact,
            target_opponent,
        });
    }

    for step in 0..STEP_CAP {
        if deadline.expired() {
            return Err(BailReason::TimeCapExceeded {
                elapsed: started_at.elapsed(),
            });
        }

        capture_snapshots(&state, target_opponent, started_turn, &mut snapshots);

        if reached_horizon(&state, target_opponent, horizon, started_turn) {
            capture_snapshots(&state, target_opponent, started_turn, &mut snapshots);
            let confidence = if choice_count == 0 {
                Confidence::Exact
            } else {
                Confidence::Approximated { choice_count }
            };
            return Ok(Projection {
                horizon_reached: horizon,
                state,
                snapshots,
                confidence,
                target_opponent,
            });
        }

        let (seat, action, is_policy_choice, witnessed_successor) =
            resolve_choice(&state, ai_player, target_opponent)?;
        if is_policy_choice {
            choice_count += 1;
        }

        if let Some(successor) = witnessed_successor {
            state = successor;
        } else {
            // CR 723.5: while controlling another player, the controller makes
            // that player's choices, so the seat holding the decision and the
            // player authorized to submit it are different players. Dispatch as
            // the submitter, but keep the seat as the decision owner: CR 723.3
            // leaves the seat's objects its own, and `semantic_owner` is what
            // the reducer keys them to. Same split
            // `auto_play::run_ai_actions_with_limit` applies to live play.
            let submitter = turn_control::authorized_submitter_for_player(&state, seat);
            apply_interaction_for_simulation(&mut state, submitter, seat, action)
                .map_err(BailReason::EngineRejected)?;
        }

        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return Err(BailReason::GameOverDuringProjection);
        }

        if step == STEP_CAP - 1 {
            return Err(BailReason::StepCapExceeded { steps: STEP_CAP });
        }
    }

    Err(BailReason::StepCapExceeded { steps: STEP_CAP })
}

/// Whether `state` has reached `horizon` on `target_opponent`'s turn.
///
/// Conjunctive predicate: phase match + active-player match + empty stack +
/// active-player holds priority (confirms APNAP triggers resolved) +
/// turn > started_turn (guard against already-at-horizon false positive on
/// entry turn for the wrong opponent).
fn reached_horizon(
    state: &GameState,
    target_opponent: PlayerId,
    horizon: ProjectionHorizon,
    started_turn: u32,
) -> bool {
    if state.active_player != target_opponent {
        return false;
    }
    // Extra-turn guard: only count a BeginCombat that arrives *after* we
    // began projecting, unless we entered projection already on this opponent.
    let started_on_this_opp = state.turn_number == started_turn;
    match horizon {
        ProjectionHorizon::OpponentBeginCombat => {
            if !matches!(state.phase, Phase::BeginCombat) {
                return false;
            }
            if !state.stack.is_empty() {
                return false;
            }
            let priority_ok = matches!(
                &state.waiting_for,
                WaitingFor::Priority { player } if *player == target_opponent
            );
            // Only accept BeginCombat once we've actually advanced (either a
            // new turn, or we were already sitting at the predicate).
            priority_ok && (!started_on_this_opp || is_fresh_begin_combat(state))
        }
        ProjectionHorizon::OpponentAttackersDeclared => {
            // CR 508.1a: creatures_attacked_this_turn tracks declared attackers.
            if state.creatures_attacked_this_turn.is_empty() {
                return false;
            }
            if !state.stack.is_empty() {
                return false;
            }
            matches!(
                &state.waiting_for,
                WaitingFor::Priority { player } if *player == target_opponent
            )
        }
        ProjectionHorizon::OpponentCombatDamage => {
            // CR 510: Combat damage step. After damage is dealt, phase advances
            // but creatures_attacked_this_turn remains populated.
            matches!(state.phase, Phase::CombatDamage | Phase::EndCombat)
                && state.stack.is_empty()
                && matches!(
                    &state.waiting_for,
                    WaitingFor::Priority { player } if *player == target_opponent
                )
        }
    }
}

/// True if BeginningOfCombat triggers have finished resolving for this turn —
/// used as a rough check that we haven't short-circuited at the moment of
/// phase entry before triggers fire.
fn is_fresh_begin_combat(_state: &GameState) -> bool {
    // Stack-empty + priority-to-active already implies this in practice:
    // if BeginCombat triggers existed, they would be on the stack or would
    // have already been passed through.
    true
}

fn capture_snapshots(
    state: &GameState,
    target_opponent: PlayerId,
    started_turn: u32,
    snapshots: &mut Vec<(ProjectionHorizon, GameState)>,
) {
    if reached_horizon(
        state,
        target_opponent,
        ProjectionHorizon::OpponentBeginCombat,
        started_turn,
    ) && !snapshots
        .iter()
        .any(|(h, _)| *h == ProjectionHorizon::OpponentBeginCombat)
    {
        snapshots.push((ProjectionHorizon::OpponentBeginCombat, state.clone()));
    }
    if reached_horizon(
        state,
        target_opponent,
        ProjectionHorizon::OpponentAttackersDeclared,
        started_turn,
    ) && !snapshots
        .iter()
        .any(|(h, _)| *h == ProjectionHorizon::OpponentAttackersDeclared)
    {
        snapshots.push((ProjectionHorizon::OpponentAttackersDeclared, state.clone()));
    }
}

/// Pick a legal action for the currently-waiting player based on projection
/// policy. Returns `(seat, action, is_policy_choice, witnessed_successor)`.
/// `seat` is the semantic decision owner and NOT the authorized submitter —
/// under CR 723.5 turn control they differ, and this function's attacker-policy
/// branch compares it against `target_opponent` in its seat role. The caller
/// derives the submitter at its dispatch. `is_policy_choice` flags non-trivial
/// policy decisions that increment `choice_count`; `witnessed_successor` is a
/// pre-applied payment-continuation state that bypasses that dispatch.
fn resolve_choice(
    state: &GameState,
    ai_player: PlayerId,
    target_opponent: PlayerId,
) -> Result<(PlayerId, GameAction, bool, Option<GameState>), BailReason> {
    // Impossible-mid-game gates.
    match &state.waiting_for {
        WaitingFor::MulliganDecision { .. }
        | WaitingFor::OpeningHandBottomCards { .. }
        | WaitingFor::BetweenGamesSideboard { .. }
        | WaitingFor::BetweenGamesChoosePlayDraw { .. } => {
            return Err(BailReason::MulliganOrSideboardEncountered);
        }
        WaitingFor::GameOver { .. } => {
            return Err(BailReason::GameOverDuringProjection);
        }
        _ => {}
    }

    let acting = state
        .waiting_for
        .acting_player()
        .ok_or_else(|| BailReason::NoLegalAction {
            waiting_for: format!("{:?}", state.waiting_for),
        })?;

    // CR 117.3d: at a priority window whose pass the engine can decide without
    // simulating the boundary, the projection's only policy is "pass if you can"
    // (`pick_pass_or_first`). That is an O(1) question the engine answers
    // directly, so skip the full `legal_actions` enumeration — which builds a
    // `PriorityCastProbe` (a `GameState` clone + `flush_layers` + auto-tap
    // cache), validates every castable spell, activatable ability and land
    // drop, and discards a grouped per-object map.
    //
    // `pass_priority_structurally_legal` is the SAME predicate the engine's
    // `SimulationFilter` pass hatch uses. Do not substitute a local
    // approximation: in particular a `classify_payment_continuation(state) ==
    // NotAffiliated` test is strictly WEAKER — that verdict is compatible with
    // `pending_deferred_life_cost_resume` / `pending_cost_move_resume` still
    // being `Some`, which is exactly when the pass boundary runs a fallible
    // continuation drain.
    //
    // Each parked field carries its own CR annotation on `GameState`, and they
    // are not the same rule: `pending_cost_move_resume` is CR 601.2h (a cost
    // that moves objects, paid in a defined order), `pending_deferred_life_cost_resume`
    // is CR 118.3b + CR 119.4 (a life payment already committed). That ordering,
    // together with CR 601.2g's mana-ability window, is preserved without hoisting
    // `classify_payment_continuation`: at a `Priority` window with both parked
    // fields `None`, that classifier necessarily returns `NotAffiliated`
    // (its deferred-life short-circuit needs a parked root, `Priority` matches
    // none of its `waiting_for` arms, and its parked-cost-move fallback returns
    // `NotAffiliated` when no root is parked), so this fast path can never steal
    // a window the payment witness below owns.
    if let WaitingFor::Priority { player } = state.waiting_for {
        if priority::pass_priority_structurally_legal(state, player) {
            return Ok((acting, GameAction::PassPriority, false, None));
        }
    }

    let actions = legal_actions(state);
    if actions.is_empty() {
        return Err(BailReason::NoLegalAction {
            waiting_for: format!("{:?}", state.waiting_for),
        });
    }

    match classify_payment_continuation(state) {
        PaymentContinuationState::NotAffiliated => {}
        PaymentContinuationState::UnsupportedAffiliated(_) => {
            return Err(BailReason::NoLegalManaPayment);
        }
        PaymentContinuationState::Affiliated(_) => {
            let mut actions = actions;
            actions.sort_by(|left, right| left.cmp_stable(right));
            let batch = witness_payment_continuations(state, &actions);
            let status = batch.status;
            let accepted = actions
                .into_iter()
                .zip(batch.successors)
                .find_map(|(action, successor)| successor.map(|successor| (action, successor)));
            let accepted = match (status, accepted) {
                (_, Some(accepted)) => accepted,
                (PaymentContinuationBatchStatus::Complete, None) => {
                    return Err(BailReason::NoLegalManaPayment);
                }
                (PaymentContinuationBatchStatus::Indeterminate(_), None) => {
                    return Err(BailReason::IncompleteManaPaymentWitness);
                }
                (
                    PaymentContinuationBatchStatus::NotAffiliated
                    | PaymentContinuationBatchStatus::UnsupportedAffiliated(_),
                    None,
                ) => {
                    return Err(BailReason::NoLegalManaPayment);
                }
            };
            return Ok((acting, accepted.0, true, Some(accepted.1.state)));
        }
    }

    // Policy dispatch on WaitingFor kind + actor identity.
    let action = match &state.waiting_for {
        WaitingFor::Priority { .. } => pick_pass_or_first(&actions),

        WaitingFor::DeclareAttackers { .. } => {
            // Opponent (target): maximize attackers against AI for pessimism.
            // AI self: decline all attacks (no recursion into combat AI).
            // Other opponent (multiplayer): decline (only target_opponent's
            // attacks matter for this projection).
            if acting == target_opponent {
                pick_max_attackers_against(&actions, ai_player)
            } else {
                pick_empty_attackers(&actions)
            }
        }

        WaitingFor::DeclareBlockers { .. } => {
            // AI or any player: decline (v0 no-blocks baseline).
            pick_empty_blockers(&actions)
        }

        // CR 701.42b / CR 508.4: deterministic projection for Meld and
        // battlefield-entry attack-target choices. Tactical public play uses the
        // policy/search path; projection only needs a stable legal branch.
        WaitingFor::MeldPairChoice { .. }
        | WaitingFor::MeldAttackTargetChoice { .. }
        | WaitingFor::EntryAttackTargetChoice { .. } => {
            actions
                .first()
                .cloned()
                .ok_or_else(|| BailReason::NoLegalAction {
                    waiting_for: format!("{:?}", state.waiting_for),
                })?
        }

        // CR 118.3 + CR 605.3b: ReturnToHand, Behold, and TapCreatures cost
        // payments project as "first legal payment" (matching the pre-collapse
        // behavior — Discard / Sacrifice / Exile / RemoveCounter PayCost kinds
        // fall through to the catch-all below, as their old variants did).
        WaitingFor::PayCost {
            kind:
                PayCostKind::ReturnToHand
                | PayCostKind::Behold { .. }
                | PayCostKind::TapCreatures { .. },
            ..
        }
        | WaitingFor::DefilerPayment { .. }
        | WaitingFor::CombatTaxPayment { .. }
        | WaitingFor::HarmonizeTapChoice { .. }
        | WaitingFor::AlternativeCastChoice { .. }
        | WaitingFor::UnlessPayment { .. } => {
            // First legal payment. If none exist for a mandatory cost, bail.
            actions
                .first()
                .cloned()
                .ok_or(BailReason::NoLegalManaPayment)?
        }

        // Non-payment color choices retain their established first-legal policy.
        // Affiliated mana-ability choices return through the witness above.
        WaitingFor::ChooseManaColor { choice, .. } => match choice {
            ManaChoicePrompt::SingleColor { options } => options
                .first()
                .copied()
                .map(|color| GameAction::ChooseManaColor {
                    choice: ManaChoice::SingleColor(color),
                    count: 1,
                })
                .ok_or(BailReason::NoLegalManaPayment)?,
            ManaChoicePrompt::Combination { options } => options
                .first()
                .map(|combo| GameAction::ChooseManaColor {
                    choice: ManaChoice::Combination(combo.clone()),
                    count: 1,
                })
                .ok_or(BailReason::NoLegalManaPayment)?,
            ManaChoicePrompt::AnyCombination { count, options } => {
                // Bail on empty options like the sibling arms, rather than
                // fabricating a Colorless pip the engine would reject.
                let color = options
                    .first()
                    .copied()
                    .ok_or(BailReason::NoLegalManaPayment)?;
                GameAction::ChooseManaColor {
                    choice: ManaChoice::Combination(vec![color; *count]),
                    count: 1,
                }
            }
        },

        // CR 107.1c + CR 601.2f: X-value projection picks the maximum legal X.
        // Candidates are emitted in `min..=max` order
        // (`engine::ai_support::candidates`), so the last action is the
        // maximum. Issue #710: projecting X=0 (the previous behavior, shared
        // with the payment arms above) collapsed the search-tree value of every
        // X-cost spell to "does nothing." The engine has already capped `max`
        // to a legally payable amount, so `last()` is always affordable.
        WaitingFor::ChooseXValue { .. } => actions
            .last()
            .cloned()
            .ok_or(BailReason::NoLegalManaPayment)?,

        WaitingFor::ResolutionOptionalPaymentChoice { .. }
        | WaitingFor::OptionalEffectChoice { .. }
        | WaitingFor::OpponentMayChoice { .. }
        | WaitingFor::OptionalCostChoice { .. }
        | WaitingFor::TributeChoice { .. }
        | WaitingFor::CompanionReveal { .. } => {
            // For the actor: pick the "no" option (decline) unless it's the
            // opponent and there's a clearly growth-maximizing yes.
            // Simple v0: always pick first — usually decline.
            actions.first().cloned().unwrap()
        }

        // CR 732.2a: projection must preserve the offer's optionality. Choose the engine's
        // legal decline action rather than fabricating a mandatory declaration.
        WaitingFor::LoopShortcut { .. } => actions
            .iter()
            .find(|action| matches!(action, GameAction::DeclineShortcut))
            .cloned()
            .ok_or_else(|| BailReason::NoLegalAction {
                waiting_for: format!("{:?}", state.waiting_for),
            })?,
        // The finite pre-cast family remains optional in a projection. Declining
        // preserves ordinary priority instead of fabricating a route proposal.
        WaitingFor::PrecastCopyShortcutOffer { .. } => actions
            .iter()
            .find(|action| {
                matches!(
                    action,
                    GameAction::PrecastCopyShortcut {
                        response: engine::types::actions::PrecastCopyShortcutResponse::Decline,
                        ..
                    }
                )
            })
            .cloned()
            .ok_or_else(|| BailReason::NoLegalAction {
                waiting_for: format!("{:?}", state.waiting_for),
            })?,
        // PR-7 Phase 4c (LOW-2): self-preservation via the single-authority
        // `smart_shortcut_response` — Shorten when the polled player has a meaningful
        // way to break the loop, else Accept.
        WaitingFor::RespondToShortcut { player, .. } => GameAction::RespondToShortcut {
            response: engine::ai_support::smart_shortcut_response(state, *player),
        },
        WaitingFor::RespondToPrecastCopyShortcut {
            player,
            epoch,
            breakpoint_ids,
            ..
        } => {
            let response = match engine::ai_support::smart_shortcut_response(state, *player) {
                engine::analysis::loop_check::ShortcutResponse::Shorten { .. } => {
                    breakpoint_ids.first().map_or(
                        engine::types::actions::PrecastCopyShortcutResponse::Accept,
                        |breakpoint_id| {
                            engine::types::actions::PrecastCopyShortcutResponse::Shorten {
                                breakpoint_id: *breakpoint_id,
                            }
                        },
                    )
                }
                engine::analysis::loop_check::ShortcutResponse::Accept => {
                    engine::types::actions::PrecastCopyShortcutResponse::Accept
                }
            };
            GameAction::PrecastCopyShortcut {
                epoch: *epoch,
                response,
            }
        }

        _ => {
            // All remaining variants: first legal action.
            actions.first().cloned().unwrap()
        }
    };

    let is_policy_choice = !matches!(action, GameAction::PassPriority);
    Ok((acting, action, is_policy_choice, None))
}

fn pick_pass_or_first(actions: &[GameAction]) -> GameAction {
    actions
        .iter()
        .find(|a| matches!(a, GameAction::PassPriority))
        .cloned()
        .unwrap_or_else(|| actions[0].clone())
}

fn pick_empty_attackers(actions: &[GameAction]) -> GameAction {
    actions
        .iter()
        .find(|a| matches!(a, GameAction::DeclareAttackers { attacks, .. } if attacks.is_empty()))
        .cloned()
        .unwrap_or_else(|| actions[0].clone())
}

fn pick_empty_blockers(actions: &[GameAction]) -> GameAction {
    actions
        .iter()
        .find(
            |a| matches!(a, GameAction::DeclareBlockers { assignments } if assignments.is_empty()),
        )
        .cloned()
        .unwrap_or_else(|| actions[0].clone())
}

fn pick_max_attackers_against(actions: &[GameAction], ai_player: PlayerId) -> GameAction {
    // From the DeclareAttackers candidate set, pick the variant with the most
    // attackers targeting `ai_player` (pessimistic worst-case).
    let mut best: Option<(usize, &GameAction)> = None;
    for action in actions {
        if let GameAction::DeclareAttackers { attacks, .. } = action {
            let count = attacks
                .iter()
                .filter(|(_, target)| matches!(target, AttackTarget::Player(p) if *p == ai_player))
                .count();
            match best {
                None => best = Some((count, action)),
                Some((best_count, _)) if count > best_count => best = Some((count, action)),
                _ => {}
            }
        }
    }
    best.map(|(_, a)| a.clone())
        .unwrap_or_else(|| actions[0].clone())
}

/// Compute growth per opponent creature across the projection.
/// Uses the `OpponentBeginCombat` snapshot when available (isolates growth
/// signal from attack-feasibility prohibitions like Moat).
pub fn threat_velocity(
    base: &GameState,
    projection: &Projection,
    opponent: PlayerId,
) -> HashMap<ObjectId, VelocitySample> {
    let projected = projection
        .snapshot(ProjectionHorizon::OpponentBeginCombat)
        .unwrap_or(&projection.state);

    let mut samples: HashMap<ObjectId, VelocitySample> = HashMap::new();
    let mut base_seen: std::collections::HashSet<ObjectId> = std::collections::HashSet::new();

    // Pass 1: creatures present in base — Changed or Removed.
    for &id in base.battlefield.iter() {
        let Some(base_obj) = base.objects.get(&id) else {
            continue;
        };
        if base_obj.controller != opponent
            || !base_obj.card_types.core_types.contains(&CoreType::Creature)
        {
            continue;
        }
        base_seen.insert(id);
        let base_power = base_obj.power.unwrap_or(0);
        match projected.objects.get(&id) {
            Some(proj_obj) if projected.battlefield.contains(&id) => {
                let proj_power = proj_obj.power.unwrap_or(0);
                samples.insert(
                    id,
                    VelocitySample::Changed {
                        delta: proj_power - base_power,
                    },
                );
            }
            _ => {
                samples.insert(id, VelocitySample::Removed);
            }
        }
    }

    // Pass 2: new creatures in projection not in base — Appeared.
    for &id in projected.battlefield.iter() {
        if base_seen.contains(&id) {
            continue;
        }
        let Some(proj_obj) = projected.objects.get(&id) else {
            continue;
        };
        if proj_obj.controller != opponent
            || !proj_obj.card_types.core_types.contains(&CoreType::Creature)
        {
            continue;
        }
        samples.insert(
            id,
            VelocitySample::Appeared {
                projected_power: proj_obj.power.unwrap_or(0),
            },
        );
    }

    samples
}

/// Shared full-loop projection fixtures.
///
/// Every pre-existing projection fixture in this crate is *already at its
/// horizon*, so `project_to` short-circuits at the `Confidence::Exact` branch
/// above and never enters its loop. The states built here are deliberately the
/// opposite class: they are NOT at a horizon on entry, so the loop runs real
/// `apply_interaction_for_simulation` dispatches and returns
/// `Confidence::Approximated { choice_count >= 1 }` — the witness that the loop
/// ran rather than the short-circuit.
///
/// Lives beside `project_to` because the invariant these states encode ("this
/// state traverses the loop to its requested horizon") is a property of
/// `project_to`'s own resolution policy, not of any consumer.
#[cfg(test)]
pub(crate) mod projection_fixtures {
    use super::*;
    use engine::game::scenario::GameScenario;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    /// A battlefield creature that can attack on the turn after it was placed:
    /// untapped, not summoning-sick, `entered_battlefield_turn = 1`.
    ///
    /// Mirrors `GameScenario::add_creature` (`scenario.rs:355-391`), which is
    /// the builder recipe for a "pre-existing" (therefore not sick) creature.
    fn spawn_vanilla_creature(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(
            state,
            card_id,
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.entered_battlefield_turn = Some(1);
        obj.summoning_sick = false;
        id
    }

    /// A state that is NOT at any horizon on entry and that `project_to`
    /// traverses to `ProjectionHorizon::OpponentAttackersDeclared` in a handful
    /// of dispatches, with `ai_player = P0` and `target_opponent = P1`.
    ///
    /// Placed at the OPPONENT's own precombat main phase, which is what makes it
    /// cheap and robust:
    ///   * `active_player == P1 == target_opponent`, so no turn boundary is
    ///     crossed — `turns.rs`'s turn-advance never clears
    ///     `creatures_attacked_this_turn` during the traversal.
    ///   * `Phase::PreCombatMain` is AFTER the draw step (CR 500.1 phase order),
    ///     so no library is ever read and both libraries may stay empty. A
    ///     projected draw from an empty library would end the game (CR 704.5b)
    ///     and return `BailReason::GameOverDuringProjection` — a red positive
    ///     arm for the wrong reason.
    ///   * No untap step runs, so tapped/untapped state is exactly as built.
    ///
    /// Traversal, all deterministic:
    ///   0. `Priority { P1 }`  → `pick_pass_or_first` picks `PassPriority`
    ///   1. `Priority { P0 }`  → `PassPriority`; both passed with an empty stack,
    ///      so the engine advances the phase. `auto_advance_once`'s BeginCombat
    ///      arm sees `has_potential_attackers(state) == true` and continues to
    ///      `DeclareAttackers` WITHOUT opening a priority window.
    ///   2. `WaitingFor::DeclareAttackers` with `acting == target_opponent`, so
    ///      `resolve_choice` uses `pick_max_attackers_against(&actions, P0)` and
    ///      declares the bear against P0. `finish_declare_attackers` returns
    ///      `Priority { P1 }` with `creatures_attacked_this_turn` populated and
    ///      an empty stack (CR 508.1 turn-based action).
    ///   3. Loop head: `reached_horizon` is true → `Ok(Projection { .. })`.
    ///
    /// `choice_count >= 1` because the DeclareAttackers dispatch is not a
    /// `PassPriority`, so the result is `Confidence::Approximated` — the witness
    /// that the LOOP ran rather than the already-at-horizon short-circuit, which
    /// hardcodes `Confidence::Exact`.
    pub(crate) fn opponent_turn_precombat_fixture() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(1);
        state.phase = Phase::PreCombatMain;
        // The coherent triple `GameScenario::at_phase` maintains
        // (`scenario.rs:212-222`): phase + waiting_for + priority_player.
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };
        state.stack.clear();
        // creatures_attacked_this_turn stays EMPTY — this is what makes the
        // fixture NOT already-at-horizon.
        state.creatures_attacked_this_turn.clear();

        // P1's attacker. `has_potential_attackers` (`combat.rs`) requires
        // controller == active, Creature, !tapped, no Defender/can't-attack, and
        // `entered_battlefield_turn < turn_number` (or Haste). 1 < 2 holds.
        spawn_vanilla_creature(&mut state, PlayerId(1), "Projection Bear", 2, 2);
        state
    }

    /// A state at P0's declare-attackers step from which BOTH of the following
    /// hold:
    ///   (i)  `search::deterministic_choice` routes to the combat branch and
    ///        `combat_ai` reaches its crackback projection block, and
    ///   (ii) `project_to(.., P0, P1, OpponentAttackersDeclared, ..)` traverses
    ///        to its horizon.
    ///
    /// The tension the construction resolves: (i) needs P1 to have NO untapped
    /// blocker, or `combat_ai`'s `if is_unblockable || opponent_blockers
    /// .is_empty()` short-circuit does not fire and the value heuristic may
    /// decline the attack, emptying `attacking_ids` and skipping the crackback
    /// block. But (ii) needs P1 to HAVE an attack-capable creature on its own
    /// next turn, or `has_potential_attackers` is false at P1's BeginCombat, the
    /// declare-blockers and combat-damage steps are skipped (CR 508.8) and the
    /// horizon is unreachable.
    ///
    /// Resolution: give P1 a **tapped** creature. `opponent_blockers` in
    /// `combat_ai` filters on `!obj.tapped`, so it is invisible to (i); the
    /// projected P1 untap step untaps it, so it satisfies (ii).
    ///
    /// Both libraries are stocked because the traversal crosses into P1's turn
    /// and P1's draw step reads one card (CR 500.1 phase order; `should_skip_draw`
    /// skips only on turn 1 in a 2-player game, and this fixture is on turn 2).
    /// An empty library there ends the game (CR 704.5b) and returns
    /// `BailReason::GameOverDuringProjection`. P0's library is stocked as cheap
    /// insurance; the traversal is not expected to reach a P0 draw.
    ///
    /// Life totals are 20/20 and the lone attacker is 2 power, so neither
    /// `determine_attack_objective` returns `PushLethal` nor
    /// `adversarial_swarm_witness` (gated 2-player in `combat_ai`) certifies a
    /// lethal, declaration-binding attack that would return before the crackback
    /// block. **If either the attacker's power or P1's life is changed, both of
    /// those reach conditions can silently flip.**
    pub(crate) fn ai_turn_declare_attackers_fixture() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.phase = Phase::DeclareAttackers;
        state.players[0].life = 20;
        state.players[1].life = 20;
        state.stack.clear();
        state.creatures_attacked_this_turn.clear();

        // P0's attacker: untapped, not sick.
        let attacker = spawn_vanilla_creature(&mut state, PlayerId(0), "AI Bear", 2, 2);
        debug_assert!(!state.objects[&attacker].tapped);

        // P1's FUTURE attacker: tapped now (so it is not an `opponent_blocker`),
        // untaps during the projected P1 untap step.
        let crackback = spawn_vanilla_creature(&mut state, PlayerId(1), "Crackback Bear", 2, 2);
        state.objects.get_mut(&crackback).unwrap().tapped = true;

        // CR 704.5b insurance for the projected P1 draw step.
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(900 + i),
                PlayerId(0),
                format!("Filler {i}"),
                Zone::Library,
            );
            create_object(
                &mut state,
                CardId(950 + i),
                PlayerId(1),
                format!("Filler {i}"),
                Zone::Library,
            );
        }

        // The engine owns the DeclareAttackers payload shape — do not hand-write
        // its five fields. `build_declare_attackers_waiting_for` derives every
        // one of them from `state` via the single `AttackDeclarationConstraints`
        // authority.
        state.combat = Some(engine::game::combat::CombatState::default());
        state.waiting_for = engine::game::combat::build_declare_attackers_waiting_for(&state);
        state
    }

    /// Fail loudly, with the engine's own bail reason in the message, if `state`
    /// is ALREADY at `horizon` — which would make `project_to` short-circuit and
    /// silently turn every "the loop ran" assertion vacuous.
    ///
    /// Implemented against the public primitive rather than the private
    /// `reached_horizon`: a `Confidence::Exact` result from a fixture that is
    /// supposed to traverse IS the short-circuit, so this is a direct
    /// observation, not a proxy.
    pub(crate) fn assert_traverses_to(
        state: &GameState,
        ai_player: PlayerId,
        target_opponent: PlayerId,
        horizon: ProjectionHorizon,
    ) {
        let result = project_to(state, ai_player, target_opponent, horizon, Deadline::none());
        match &result {
            Ok(projection) => {
                assert_eq!(
                    projection.horizon_reached, horizon,
                    "fixture reached the wrong horizon"
                );
                assert!(
                    matches!(
                        projection.confidence,
                        Confidence::Approximated { choice_count } if choice_count >= 1
                    ),
                    "FIXTURE DEFECT, not a wiring defect: this fixture must TRAVERSE \
                     project_to's loop, but it returned Confidence::Exact — which the \
                     already-at-horizon short-circuit hardcodes and a real traversal \
                     through a DeclareAttackers dispatch cannot produce. Re-derive the \
                     fixture; do not weaken this assertion."
                );
            }
            Err(reason) => panic!(
                "FIXTURE DEFECT, not a wiring defect: project_to could not reach \
                 {horizon:?} under a non-expiring deadline. Bail reason: {reason:?}. \
                 GameOverDuringProjection ⇒ stock the libraries; StepCapExceeded ⇒ the \
                 horizon is not reachable from this state at all; NoLegalAction ⇒ the \
                 waiting_for/priority_player triple is incoherent."
            ),
        }
    }

    /// Oracle text of the permanent that makes
    /// `ProjectionHorizon::OpponentBeginCombat` reachable BY TRAVERSAL.
    ///
    /// Load-bearing, for a structural reason. The engine never RESTS at
    /// `Phase::BeginCombat` holding `WaitingFor::Priority` unless a begin-combat
    /// trigger fires: `auto_advance_once`'s `Phase::BeginCombat` arm
    /// (`crates/engine/src/game/turns.rs`) opens a priority window only when
    /// `process_phase_triggers` reports `triggers_fired`. With no trigger it
    /// either advances straight to `DeclareAttackers` (when
    /// `combat::has_potential_attackers`) or, per CR 508.8, enters
    /// `PostCombatMain` — and neither path stops on `reached_horizon`'s
    /// `OpponentBeginCombat` predicate (active player is the target opponent +
    /// `Phase::BeginCombat` + empty stack + `Priority` held by the target
    /// opponent). Measured, not assumed: with this permanent replaced by a
    /// vanilla creature the traversal never stops at the horizon — it runs on
    /// past the opponent's combat until a library empties and returns
    /// `BailReason::GameOverDuringProjection`.
    ///
    /// With the trigger the traversal ends this way: the trigger fires and goes
    /// on the stack, so the opponent's first `Priority` window fails the
    /// empty-stack conjunct; both players pass; the trigger resolves; CR 117.3b
    /// returns priority to the active player with an empty stack, which is the
    /// horizon.
    ///
    /// The growth clause is not decoration either. It is exactly the
    /// `threat_velocity` signal `policies::evasion_removal_priority::
    /// velocity_score` exists to read, so the projected board differs from the
    /// base board the way a production Ouroboroid-class board does.
    const BEGIN_COMBAT_GROWTH_ORACLE: &str =
        "At the beginning of combat on your turn, put a +1/+1 counter on this creature.";

    /// Seed `scenario` so that a state built from it TRAVERSES `project_to`'s
    /// loop to `ProjectionHorizon::OpponentBeginCombat` for
    /// `(ai_player, target_opponent)`. Returns the growing permanent's id, which
    /// doubles as a removal target whose threat genuinely grows before the
    /// projected combat.
    ///
    /// Unlike Fixtures A and B this is a seeder rather than a whole `GameState`,
    /// because its only consumer needs a state that ALSO satisfies a tactical
    /// policy's own gates (a real pending cast at `WaitingFor::TargetSelection`).
    /// Card-specific setup stays with the policy test; the projection-reachability
    /// knowledge stays here.
    ///
    /// Preconditions the CALLER owns — the ordinary `GameScenario::at_phase`
    /// recipe, not extra ceremony:
    ///   * `ai_player` is the active player and holds priority, and
    ///   * the scenario sits at `Phase::PreCombatMain` on `ai_player`'s turn,
    ///     which is where the production evasion policy scores removal targets.
    ///
    /// What this adds, and why each piece is required:
    ///   * `target_opponent`'s begin-combat trigger permanent — see
    ///     [`BEGIN_COMBAT_GROWTH_ORACLE`]; without it the horizon is unreachable
    ///     from any state at all, not merely awkward to reach.
    ///   * an untapped, non-summoning-sick attacker for `ai_player`, so the
    ///     traversal necessarily dispatches one `WaitingFor::DeclareAttackers`
    ///     choice — `pick_empty_attackers`, since `acting != target_opponent`.
    ///     That action is not `PassPriority`, so `choice_count >= 1` and the
    ///     result is `Confidence::Approximated`: the typed witness
    ///     [`assert_traverses_to`] checks, and the one the already-at-horizon
    ///     short-circuit structurally cannot emit.
    ///   * five cards in each library. The traversal crosses into
    ///     `target_opponent`'s turn, and CR 504.1 has its active player draw a
    ///     card in the draw step, which precedes the combat phase (CR 500.1
    ///     phase order). An empty library there loses the game (CR 704.5b) and
    ///     the projection returns `BailReason::GameOverDuringProjection` instead
    ///     of reaching the horizon.
    pub(crate) fn seed_opponent_begin_combat_horizon(
        scenario: &mut GameScenario,
        ai_player: PlayerId,
        target_opponent: PlayerId,
    ) -> ObjectId {
        let grower = scenario
            .add_creature_from_oracle(
                target_opponent,
                "Projection Grower",
                2,
                2,
                BEGIN_COMBAT_GROWTH_ORACLE,
            )
            .id();
        scenario.add_creature(ai_player, "Projection Bear", 2, 2);
        scenario.with_library_top(
            ai_player,
            &[
                "AI Filler 0",
                "AI Filler 1",
                "AI Filler 2",
                "AI Filler 3",
                "AI Filler 4",
            ],
        );
        scenario.with_library_top(
            target_opponent,
            &[
                "Opp Filler 0",
                "Opp Filler 1",
                "Opp Filler 2",
                "Opp Filler 3",
                "Opp Filler 4",
            ],
        );
        grower
    }

    /// Fail loudly if `permanent` no longer carries the parsed begin-combat
    /// trigger that [`seed_opponent_begin_combat_horizon`]'s traversal depends
    /// on.
    ///
    /// Cheaper and far more specific than waiting for the traversal to fail:
    /// parser drift on [`BEGIN_COMBAT_GROWTH_ORACLE`] would otherwise surface
    /// several hundred dispatches later as a
    /// `BailReason::GameOverDuringProjection`, which reads like an unrelated
    /// library-stocking defect.
    pub(crate) fn assert_begin_combat_trigger_parsed(state: &GameState, permanent: ObjectId) {
        let object = state
            .objects
            .get(&permanent)
            .expect("FIXTURE DEFECT: the seeded begin-combat permanent no longer exists");
        assert!(
            object.base_trigger_definitions.iter().any(|trigger| {
                matches!(trigger.mode, TriggerMode::Phase)
                    && trigger.phase == Some(Phase::BeginCombat)
            }),
            "FIXTURE DEFECT, not a wiring defect: {} no longer parses a TriggerMode::Phase \
             trigger on Phase::BeginCombat, so auto_advance_once's BeginCombat arm will not \
             open the priority window the OpponentBeginCombat horizon needs and the traversal \
             cannot reach it. Re-derive the fixture's Oracle text; do not weaken the guard.",
            object.name
        );
    }

    /// The already-at-horizon counterpart: assert the state short-circuits, so
    /// the determinism claim of a fixture in that class is checked rather than
    /// assumed.
    pub(crate) fn assert_already_at_horizon(
        state: &GameState,
        ai_player: PlayerId,
        target_opponent: PlayerId,
        horizon: ProjectionHorizon,
    ) {
        let result = project_to(state, ai_player, target_opponent, horizon, Deadline::none());
        assert!(
            matches!(&result, Ok(p) if p.horizon_reached == horizon
                && matches!(p.confidence, Confidence::Exact)),
            "fixture must be already-at-horizon (Confidence::Exact, no simulation); got {result:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::scenario::{GameScenario, P0};
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
    use engine::types::game_state::{DeferredLifeCostResume, PendingCast, PendingCostMoveResume};
    use engine::types::identifiers::CardId;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use engine::types::zones::Zone;

    /// T1. Both directions in one test: measurement maps to a non-expiring
    /// budget, interactive maps to a finite one no larger than the 15 ms cap.
    ///
    /// A stub returning `none()` for both modes fails the interactive half; a
    /// stub returning `after(15)` for both fails the measurement half; deleting
    /// the `is_measurement()` branch fails the measurement half.
    ///
    /// No strict lower bound on the interactive budget: `remaining()` uses
    /// `saturating_duration_since`, so a >15 ms scheduling stall between
    /// construction and assertion would yield `Some(0)` and a spurious red.
    /// `is_some()` + `<= 15ms` still discriminates both stubs without the flake.
    #[test]
    fn projection_deadline_nulls_wall_clock_only_in_measurement() {
        let measurement = projection_deadline(ExecutionMode::Measurement { seed: 7 });
        assert!(
            measurement.remaining().is_none(),
            "measurement mode must receive a non-expiring budget, bounded by STEP_CAP alone"
        );
        assert!(
            !measurement.expired(),
            "a non-expiring budget must never report expiry"
        );

        let interactive = projection_deadline(ExecutionMode::Interactive);
        let remaining = interactive
            .remaining()
            .expect("interactive mode must receive a finite wall-clock budget");
        assert!(
            remaining <= Duration::from_millis(15),
            "the interactive budget must not exceed the 15 ms cap; got {remaining:?}"
        );
    }

    /// T2a + T2b + T2c on one fixture: the full-loop state completes under a
    /// non-expiring deadline and bails with the SPECIFIC `TimeCapExceeded`
    /// reason under a pre-expired one.
    ///
    /// Pairing both directions on the identical fixture is what makes the
    /// negative non-vacuous: a bare `is_err()` on a fixture that bails anyway
    /// (`NoLegalAction`, `StepCapExceeded`, `GameOverDuringProjection`) would
    /// pass without the deadline being read at all.
    #[test]
    fn project_to_completes_under_none_and_bails_under_expired() {
        // T2c — instrument witness, asserted first so a broken injection fails
        // here rather than silently downstream.
        assert!(
            Deadline::after(0).expired(),
            "instrument: Deadline::after(0) must be expired on the next read"
        );
        assert!(
            !Deadline::none().expired(),
            "instrument: Deadline::none() must never expire"
        );

        let state = projection_fixtures::opponent_turn_precombat_fixture();

        // Reach guard: this state must TRAVERSE the loop, not short-circuit.
        projection_fixtures::assert_traverses_to(
            &state,
            PlayerId(0),
            PlayerId(1),
            ProjectionHorizon::OpponentAttackersDeclared,
        );

        // T2a — positive: with no wall clock the loop runs to the horizon.
        let completed = project_to(
            &state,
            PlayerId(0),
            PlayerId(1),
            ProjectionHorizon::OpponentAttackersDeclared,
            Deadline::none(),
        )
        .expect("a non-expiring deadline must let the projection complete");
        assert_eq!(
            completed.horizon_reached,
            ProjectionHorizon::OpponentAttackersDeclared
        );
        assert!(
            matches!(
                completed.confidence,
                Confidence::Approximated { choice_count } if choice_count >= 1
            ),
            "the loop must have run: the already-at-horizon short-circuit hardcodes \
             Confidence::Exact and cannot produce Approximated"
        );

        // T2b — negative: a pre-expired deadline bails at the loop head with the
        // specific wall-clock reason, on the SAME fixture proven completable above.
        let bailed = project_to(
            &state,
            PlayerId(0),
            PlayerId(1),
            ProjectionHorizon::OpponentAttackersDeclared,
            Deadline::after(0),
        );
        assert!(
            matches!(bailed, Err(BailReason::TimeCapExceeded { .. })),
            "a pre-expired deadline must bail with TimeCapExceeded specifically, \
             not merely with some error; got {bailed:?}"
        );
    }

    #[test]
    fn projection_declines_optional_loop_shortcut_from_legal_actions() {
        let mut state = GameState::new_two_player(42);
        state.waiting_for = WaitingFor::LoopShortcut {
            proposer: PlayerId(0),
            predicted_winner: Some(PlayerId(1)),
            certificate: engine::analysis::loop_check::LoopCertificate {
                unbounded: vec![],
                win_kind: engine::analysis::loop_check::WinKind::LethalDamage,
                mandatory: false,
                residual_board_delta: engine::analysis::resource::BoardDelta::default(),
                per_cycle: None,
            },
            schema: engine::analysis::decision_template::ShortcutDecisionSchema::default(),
            declaration: None,
        };

        let (_actor, action, is_policy_choice, _successor) =
            resolve_choice(&state, PlayerId(0), PlayerId(1)).expect("the offer has legal actions");
        assert_eq!(action, GameAction::DeclineShortcut);
        assert!(
            is_policy_choice,
            "declining a shortcut is a policy decision"
        );
    }

    #[test]
    fn resolution_optional_payment_projection_roundtrips_issued_action() {
        use engine::game::effects::resolve_ability_chain;
        use engine::types::ability::{
            AbilityCost, CardSelectionMode, DiscardSelfScope, Effect, QuantityExpr,
            ResolvedAbility, TargetFilter,
        };

        let mut scenario = GameScenario::new();
        let source = scenario.add_creature(P0, "Payment Source", 1, 1).id();
        scenario.add_card_to_hand(P0, "Payment Card");
        let mut runner = scenario.build();
        let mut ability = ResolvedAbility::new(
            Effect::PayCost {
                cost: AbilityCost::OneOf {
                    costs: vec![AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: None,
                        selection: CardSelectionMode::Chosen,
                        self_scope: DiscardSelfScope::FromHand,
                    }],
                },
                scale: None,
                payer: TargetFilter::Controller,
            },
            vec![],
            source,
            P0,
        );
        ability.optional = true;
        resolve_ability_chain(runner.state_mut(), &ability, &mut Vec::new(), 0).unwrap();

        let original = runner.state().clone();
        let (actor, action, is_policy_choice, _successor) =
            resolve_choice(&original, P0, PlayerId(1))
                .expect("projection consumes the issued domain");
        assert_eq!(
            action,
            GameAction::ChooseResolutionOptionalPaymentBranch {
                choice: engine::types::ResolutionOptionalPaymentChoice::Decline,
            }
        );
        let json = serde_json::to_value(&action).unwrap();
        assert_eq!(serde_json::from_value::<GameAction>(json).unwrap(), action);
        assert_eq!(
            crate::decision_kind::classify(&original.waiting_for, &action),
            crate::policies::DecisionKind::ActivateAbility
        );
        assert!(is_policy_choice);
        let mut projected = original.clone();
        engine::game::engine::apply(&mut projected, actor, action)
            .expect("projection action re-applies through the production reducer");
        assert!(!matches!(
            projected.waiting_for,
            WaitingFor::ResolutionOptionalPaymentChoice { .. }
        ));
    }

    fn precast_offer_state() -> GameState {
        use std::path::Path;

        use engine::database::card_db::CardDatabase;
        use engine::game::scenario::{GameScenario, P0};
        use engine::game::scenario_db::GameScenarioDbExt;
        use engine::types::game_state::CastPaymentMode;
        use engine::types::zones::Zone;

        const CHAIN_OF_SMOG: &str =
            "Target player discards two cards. That player may copy this spell and may choose a new target for that copy.";

        let db = CardDatabase::from_mtgjson(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/test_fixture.json"),
        )
        .expect("test fixture must contain Witherbloom Apprentice");
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_real_card(P0, "Witherbloom Apprentice", Zone::Battlefield, &db);
        let chain = scenario
            .add_spell_to_hand_from_oracle(P0, "Chain of Smog", false, CHAIN_OF_SMOG)
            .id();

        let mut runner = scenario.build();
        let card_id = runner.state().objects[&chain].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: chain,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("Chain must enter the normal target-selection pipeline");
        runner
            .act(GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Player(P0)),
            })
            .expect("Chain can target its caster");
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::PrecastCopyShortcutOffer { .. }
        ));
        runner.state().clone()
    }

    #[test]
    fn projection_declines_precast_shortcut_offer() {
        let state = precast_offer_state();
        let (_, action, is_policy_choice, _successor) =
            resolve_choice(&state, PlayerId(0), PlayerId(1)).expect("offer has a legal decline");

        assert!(matches!(
            action,
            GameAction::PrecastCopyShortcut {
                response: engine::types::actions::PrecastCopyShortcutResponse::Decline,
                ..
            }
        ));
        assert!(
            is_policy_choice,
            "declining an optional shortcut is a policy choice"
        );
    }

    // ------------------------------------------------------------------
    // Priority fast path (CR 117.3d).
    //
    // Every assertion below is an exact `perf_counters` integer. Nothing here
    // measures time: a wall-clock assertion is the defect class this change
    // exists to remove.
    // ------------------------------------------------------------------

    /// A `Priority` window on P0 with a wide, fully-enumerable board: three
    /// untapped lands and three castable instants. Mirrors the engine's own
    /// `legal_actions_priority_cast_probe_reuses_one_flushed_state_and_one_auto_tap_cache`
    /// fixture, so a full enumeration here demonstrably builds a
    /// `PriorityCastProbe` and its auto-tap source cache.
    fn wide_priority_board() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for _ in 0..3 {
            scenario.add_basic_land(P0, ManaColor::Blue);
        }
        for i in 0..3 {
            scenario
                .add_spell_to_hand(P0, &format!("Blue Spell {i}"), true)
                .with_ability(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                })
                .with_mana_cost(ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 0,
                });
        }
        scenario.build().state().clone()
    }

    /// An empty `Priority` window on P0 outside a main phase.
    ///
    /// **Fixture pin (i) + (ii):** empty hand AND a non-main-phase window, so
    /// `candidates::priority_actions_with_probe` cannot enumerate a `PlayLand`
    /// candidate. `PlayLand` has no structural hatch, so each one would reach
    /// `fallback_simulation` and inflate `state_clone_for_legality` even with a
    /// correct implementation. Never relax the exact counter assertions below to
    /// inequalities; repin the fixture instead.
    fn bare_priority_window() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::Upkeep);
        scenario.build().state().clone()
    }

    /// T9. The fast path removes both clones AND the whole enumeration.
    ///
    /// The three action assertions pass either way and are reach-guards; the
    /// three exact zero counters are the evidence. No fixture pin is needed —
    /// the fast path returns before enumeration, so the lands and spells on this
    /// board only make the test stronger.
    #[test]
    fn priority_fast_path_skips_enumeration_entirely() {
        let state = wide_priority_board();

        engine::game::perf_counters::reset();
        let (_actor, action, is_policy_choice, successor) =
            resolve_choice(&state, PlayerId(0), PlayerId(1)).expect("a bare pass is available");
        let counters = engine::game::perf_counters::snapshot();

        assert_eq!(action, GameAction::PassPriority);
        assert!(!is_policy_choice);
        assert!(successor.is_none());
        assert_eq!(
            counters.priority_cast_probe_builds, 0,
            "the fast path must not build a PriorityCastProbe"
        );
        assert_eq!(
            counters.state_clone_for_legality, 0,
            "the fast path must not pay a legality clone"
        );
        assert_eq!(
            counters.auto_tap_source_cache_builds, 0,
            "the fast path must not build the auto-tap source cache"
        );
    }

    /// T7p. The fast path's gate is a test on the two parked-continuation
    /// FIELDS, not on `classify_payment_continuation`'s verdict.
    ///
    /// `DeferredLifeCostResume::PayAmount` is exactly the divergence: the
    /// classifier reports `NotAffiliated` while the field is still `Some`, which
    /// is the state in which the pass boundary would run a fallible continuation
    /// drain. The explicit `NotAffiliated` assertion is the non-vacuity guard —
    /// without it the test would not be exercising the divergence at all.
    ///
    /// Both counters go to `0` if the fast path is gated on the classifier's
    /// verdict instead of on `pass_priority_structurally_legal`. The returned
    /// action is deliberately NOT asserted: a successful drain legitimately
    /// yields `PassPriority` with `is_policy_choice == false` and no successor,
    /// which is indistinguishable from a fast-path return by shape alone.
    #[test]
    fn priority_fast_path_declines_on_a_parked_deferred_life_root() {
        let mut state = bare_priority_window();
        state.pending_deferred_life_cost_resume = Some(DeferredLifeCostResume::PayAmount {
            player: PlayerId(0),
            total: 0,
            resume_at_resolution_depth: 0,
        });
        assert_eq!(
            classify_payment_continuation(&state),
            PaymentContinuationState::NotAffiliated,
            "non-vacuity: this fixture must be one where the verdict gate and the field gate disagree"
        );

        engine::game::perf_counters::reset();
        let _ = resolve_choice(&state, PlayerId(0), PlayerId(1));
        let counters = engine::game::perf_counters::snapshot();

        assert_eq!(
            counters.priority_cast_probe_builds, 1,
            "the fast path must have declined, so the full enumeration ran"
        );
        assert_eq!(
            counters.state_clone_for_legality, 1,
            "the hatch must also have declined, deferring the pass to one simulation"
        );
    }

    /// T10. The fallback path does not drift when the fast path declines.
    ///
    /// Passes either way by construction — a no-drift guard, not evidence of the
    /// fix. Assertion (i) goes red if the fast path is made unconditional;
    /// assertion (ii) goes red if the enumeration path is removed.
    ///
    /// Asserted on the whole `Result`: with `priority_player` desynced every
    /// candidate is refused by the reducer, so `Err(NoLegalAction)` is a
    /// legitimate outcome and must not be unwrapped.
    #[test]
    fn priority_fallback_path_runs_when_the_fast_path_declines() {
        let mut state = wide_priority_board();
        state.priority_player = PlayerId(1);

        engine::game::perf_counters::reset();
        let result = resolve_choice(&state, PlayerId(0), PlayerId(1));
        let counters = engine::game::perf_counters::snapshot();

        assert!(
            !matches!(result, Ok((_, GameAction::PassPriority, ..))),
            "the fast path must not fire when `pass_priority_legality` refuses"
        );
        assert_eq!(
            counters.priority_cast_probe_builds, 1,
            "the enumeration path must actually have run"
        );
    }

    /// T11. A parked payment carrier at a `Priority` window still routes to the
    /// payment witness, not to the fast path.
    ///
    /// Passes on the unfixed tree — a no-drift guard. It is what fails if the
    /// `waiting_for` dispatch is hoisted above `classify_payment_continuation`.
    /// Under this change it is doubly protected (the field gate refuses this
    /// state before the classifier is consulted), but it stays because it pins
    /// the observable contract rather than the implementation route.
    #[test]
    fn parked_payment_carrier_still_routes_to_the_payment_witness() {
        let mut state = bare_priority_window();
        let pending = PendingCast::new(
            ObjectId(9200),
            CardId(9200),
            ResolvedAbility::new(Effect::NoOp, vec![], ObjectId(9200), PlayerId(0)),
            ManaCost::NoCost,
        );
        state.pending_cost_move_resume = Some(PendingCostMoveResume::ActivationMillPayment {
            player: PlayerId(0),
            pending: Box::new(pending),
        });
        assert!(
            matches!(
                classify_payment_continuation(&state),
                PaymentContinuationState::Affiliated(_)
            ),
            "non-vacuity: the fixture must actually reach the witness branch"
        );

        match resolve_choice(&state, PlayerId(0), PlayerId(1)) {
            Ok((_, action, _, successor)) => {
                assert!(
                    successor.is_some(),
                    "an affiliated payment window must return a witnessed successor, got {action:?}"
                );
            }
            Err(BailReason::NoLegalManaPayment | BailReason::IncompleteManaPaymentWitness) => {}
            other => panic!("expected the payment-witness branch, got {other:?}"),
        }
    }

    #[test]
    fn projection_horizon_is_copy_hash() {
        // Sanity: the enum is used as a HashMap key and in Copy contexts.
        let h = ProjectionHorizon::OpponentBeginCombat;
        let _copy = h;
        let mut set = std::collections::HashSet::new();
        set.insert(h);
        assert!(set.contains(&ProjectionHorizon::OpponentBeginCombat));
    }

    #[test]
    fn velocity_sample_variants() {
        let changed = VelocitySample::Changed { delta: 3 };
        let removed = VelocitySample::Removed;
        let appeared = VelocitySample::Appeared { projected_power: 5 };
        assert_ne!(changed, removed);
        assert_ne!(changed, appeared);
    }

    /// Build a minimal two-player state with one opponent creature.
    fn state_with_opp_creature(name: &str, power: i32) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(power);
        (state, id)
    }

    #[test]
    fn velocity_classifies_unchanged_creature_as_changed_zero() {
        // A vanilla creature without triggers should report Changed { delta: 0 }
        // when base and projection are identical (no growth).
        let (base, id) = state_with_opp_creature("Vanilla Bear", 2);
        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: base.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, base.clone())],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(
            samples.get(&id),
            Some(&VelocitySample::Changed { delta: 0 })
        );
    }

    #[test]
    fn velocity_classifies_grown_creature() {
        // Simulate Ouroboroid effect: same ObjectId, higher projected power.
        let (base, id) = state_with_opp_creature("Scaly", 1);
        let mut projected = base.clone();
        projected.objects.get_mut(&id).unwrap().power = Some(9);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: projected.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, projected)],
            confidence: Confidence::Approximated { choice_count: 1 },
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(
            samples.get(&id),
            Some(&VelocitySample::Changed { delta: 8 })
        );
    }

    #[test]
    fn velocity_classifies_removed_creature() {
        // Creature exists in base but is gone from projection (destroyed mid-turn).
        let (base, id) = state_with_opp_creature("Doomed", 3);
        let mut projected = base.clone();
        // Remove from battlefield (mirrors what sacrifice/destroy does structurally).
        projected.battlefield.retain(|&bid| bid != id);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: projected.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, projected)],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(samples.get(&id), Some(&VelocitySample::Removed));
    }

    #[test]
    fn velocity_classifies_appeared_token() {
        // Opponent creates a token during projection (Ophiomancer-style).
        let (base, _original_id) = state_with_opp_creature("Host", 2);
        let mut projected = base.clone();
        let token_id = create_object(
            &mut projected,
            CardId(99),
            PlayerId(1),
            "Snake Token".to_string(),
            Zone::Battlefield,
        );
        let obj = projected.objects.get_mut(&token_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(1);
        obj.toughness = Some(1);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: projected.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, projected)],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&base, &projection, PlayerId(1));
        assert_eq!(
            samples.get(&token_id),
            Some(&VelocitySample::Appeared { projected_power: 1 })
        );
    }

    #[test]
    fn velocity_ignores_ai_controlled_creatures() {
        // AI's own creatures shouldn't appear in opponent velocity samples.
        let mut state = GameState::new_two_player(42);
        let ai_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "AI Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ai_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);

        let projection = Projection {
            horizon_reached: ProjectionHorizon::OpponentBeginCombat,
            state: state.clone(),
            snapshots: vec![(ProjectionHorizon::OpponentBeginCombat, state.clone())],
            confidence: Confidence::Exact,
            target_opponent: PlayerId(1),
        };
        let samples = threat_velocity(&state, &projection, PlayerId(1));
        assert!(
            !samples.contains_key(&ai_id),
            "AI creatures must not appear in opponent velocity samples"
        );
    }

    #[test]
    fn projection_key_includes_turn_for_implicit_invalidation() {
        // Two keys identical except for turn_number must hash differently,
        // so stale entries from prior turns never serve a current lookup.
        let k1 = ProjectionKey {
            state_hash: 12345,
            turn_number: 3,
            active_player: PlayerId(0),
            ai_player: PlayerId(0),
            target_opponent: PlayerId(1),
            horizon: ProjectionHorizon::OpponentBeginCombat,
        };
        let k2 = ProjectionKey {
            turn_number: 4,
            ..k1
        };
        assert_ne!(k1, k2);
    }
}
