use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use engine::ai_support::{
    build_decision_context, AiDecisionContext, CandidateAction, TacticalClass,
};
use engine::game::engine::apply_as_current;
use engine::game::players;
use engine::types::game_state::{DayNight, GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::card_hints::should_play_now_with_facts;
use crate::cast_facts::cast_facts_for_action;
use crate::config::{AiConfig, OpponentModel, PlannerMode};
use crate::eval::{
    evaluate_for_planner, evaluate_state, strategic_intent, threat_level, StrategicIntent,
};
use crate::policies::context::PolicyContext;
use crate::policies::PolicyRegistry;

#[derive(Debug, Clone)]
pub struct RankedCandidate {
    pub candidate: CandidateAction,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub struct SearchBudget {
    pub max_nodes: u32,
    pub nodes_evaluated: u32,
    deadline: engine::util::Deadline,
}

impl SearchBudget {
    pub fn new(max_nodes: u32) -> Self {
        Self {
            max_nodes,
            nodes_evaluated: 0,
            deadline: engine::util::Deadline::none(),
        }
    }

    pub fn with_time_limit(max_nodes: u32, duration: web_time::Duration) -> Self {
        Self::with_deadline(
            max_nodes,
            engine::util::Deadline::after(duration.as_millis() as u32),
        )
    }

    /// Construct a budget with a shared [`engine::util::Deadline`] — the
    /// canonical primitive for time-bounded operations in the engine. Use
    /// this when the caller already holds a `Deadline` (e.g., propagating
    /// one top-level deadline across multiple search passes).
    pub fn with_deadline(max_nodes: u32, deadline: engine::util::Deadline) -> Self {
        Self {
            max_nodes,
            nodes_evaluated: 0,
            deadline,
        }
    }

    pub fn exhausted(&self) -> bool {
        self.nodes_evaluated >= self.max_nodes || self.deadline.expired()
    }

    pub fn tick(&mut self) {
        self.nodes_evaluated += 1;
    }
}

#[derive(Debug, Clone)]
pub struct PolicyPrior {
    pub candidate: CandidateAction,
    pub prior: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ValueEstimate {
    pub value: f64,
    pub intent: StrategicIntent,
}

#[derive(Debug, Clone)]
pub struct PlannerEvaluation {
    pub priors: Vec<PolicyPrior>,
    pub value: ValueEstimate,
}

/// Compute a lightweight hash of the game-relevant fields of a GameState.
/// This replaces the previous approach of serializing the entire state to JSON
/// and hashing the string, which was O(megabytes) per call. This version hashes
/// ~300 bytes of scalars and is 10-50x faster.
pub fn quick_state_hash(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();

    // Core game flow scalars
    state.turn_number.hash(&mut hasher);
    state.active_player.hash(&mut hasher);
    state.phase.hash(&mut hasher);
    state.priority_player.hash(&mut hasher);
    state.lands_played_this_turn.hash(&mut hasher);
    state.priority_pass_count.hash(&mut hasher);
    state.next_object_id.hash(&mut hasher);
    state.spells_cast_this_turn.hash(&mut hasher);

    // Day/night (affects transformed creature characteristics).
    // DayNight doesn't derive Hash, so match explicitly for the 3-way distinction
    // (Option discriminant alone would conflate Some(Day) with Some(Night)).
    match &state.day_night {
        None => 0u8.hash(&mut hasher),
        Some(DayNight::Day) => 1u8.hash(&mut hasher),
        Some(DayNight::Night) => 2u8.hash(&mut hasher),
    }
    // Monarch (extra card draw)
    state.monarch.hash(&mut hasher);
    // Command zone (commander availability)
    state.command_zone.hash(&mut hasher);

    // Players (life, energy, zone contents, mana).
    //
    // Hand and graveyard contents are hashed by ObjectId (not just length)
    // because the candidate-action set depends on *which* cards are in those
    // zones — castable spells in hand, flashback / escape / delve / unearth
    // candidates in the graveyard. Two search-tree positions differing only
    // by which card was drawn would otherwise collide in `candidate_cache`
    // and return cached candidates that reference cards no longer present
    // (silent decision corruption: AI tries to cast cards it doesn't have,
    // or skips ones it just drew).
    //
    // Library length stays as a count: the order is only AI-decision-relevant
    // for narrow top-card-visibility effects (Future Sight, scry-then-act on
    // a known top card), which carry their own state through other channels.
    // Hashing the full library every node would dominate the hash cost.
    for player in &state.players {
        player.life.hash(&mut hasher);
        player.energy.hash(&mut hasher);
        player.hand.len().hash(&mut hasher);
        for &id in &player.hand {
            id.hash(&mut hasher);
        }
        player.library.len().hash(&mut hasher);
        player.graveyard.len().hash(&mut hasher);
        for &id in &player.graveyard {
            id.hash(&mut hasher);
        }
        player.mana_pool.total().hash(&mut hasher);
        for unit in &player.mana_pool.mana {
            unit.color.hash(&mut hasher);
        }
    }

    // Zone compositions
    state.battlefield.hash(&mut hasher);
    state.exile.len().hash(&mut hasher);
    state.stack.len().hash(&mut hasher);
    for entry in &state.stack {
        entry.source_id.hash(&mut hasher);
        entry.controller.hash(&mut hasher);
    }

    // Combat state (attacker/blocker counts during combat phases)
    if let Some(combat) = &state.combat {
        combat.attackers.len().hash(&mut hasher);
        combat.blocker_assignments.len().hash(&mut hasher);
    }

    // Transient continuous effects (pump spells, granted abilities).
    // Hash count + source IDs for distinguishing "cast Giant Growth" from "didn't".
    state.transient_continuous_effects.len().hash(&mut hasher);
    for effect in &state.transient_continuous_effects {
        effect.source_id.hash(&mut hasher);
    }

    // Delayed triggers (pending future effects)
    state.delayed_triggers.len().hash(&mut hasher);

    // Pending state (continuations, replacements, triggers affect game flow)
    state.pending_continuation.is_some().hash(&mut hasher);
    state.pending_replacement.is_some().hash(&mut hasher);
    state.pending_trigger.is_some().hash(&mut hasher);

    // Active restrictions (damage prevention, casting restrictions)
    state.restrictions.len().hash(&mut hasher);

    // Battlefield object state (tapped, P/T, damage, controller, counters)
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            obj.tapped.hash(&mut hasher);
            obj.power.hash(&mut hasher);
            obj.toughness.hash(&mut hasher);
            obj.damage_marked.hash(&mut hasher);
            obj.controller.hash(&mut hasher);
            // Counters: HashMap iteration order is non-deterministic,
            // so hash count + sorted entries for stability.
            // Sort by as_str() to break ties between Generic variants.
            obj.counters.len().hash(&mut hasher);
            if !obj.counters.is_empty() {
                let mut counter_entries: Vec<_> = obj.counters.iter().collect();
                counter_entries.sort_by_key(|(ct, _)| ct.as_str());
                for (ct, count) in counter_entries {
                    ct.hash(&mut hasher);
                    count.hash(&mut hasher);
                }
            }
        }
    }

    hasher.finish()
}

/// Cache key for `AiDecisionContext` — combines `quick_state_hash` (board
/// state) with the full `WaitingFor` payload that drives `candidate_actions`.
///
/// `quick_state_hash` alone is NOT sufficient: `candidate_actions` dispatches
/// on `state.waiting_for` (e.g., `Priority` vs `TargetSelection` vs
/// `ModeChoice`), so two states with identical boards but different
/// `waiting_for` would collide in a hash keyed only on board state and return
/// a cached context populated with wrong candidates. Including the full
/// `WaitingFor` payload in the key (via its `Debug` serialization) is
/// conservative — the Debug output is a complete structural fingerprint and
/// is stable within a run.
pub fn candidate_cache_key(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();
    quick_state_hash(state).hash(&mut hasher);
    // Hash the full Debug form of waiting_for. Debug on WaitingFor is a
    // structural serialization (derived), so distinct variants and payloads
    // produce distinct strings.
    format!("{:?}", state.waiting_for).hash(&mut hasher);
    hasher.finish()
}

#[derive(Debug, Clone, Default)]
pub struct UtilityVector {
    pub self_value: f64,
    pub opponent_pressures: Vec<f64>,
    pub elimination_bonus: f64,
    pub crackback_risk: f64,
}

pub trait UtilityReducer: Send + Sync {
    fn reduce(&self, vector: &UtilityVector) -> f64;
}

#[derive(Debug, Clone, Copy)]
pub struct DuelUtilityReducer;

impl UtilityReducer for DuelUtilityReducer {
    fn reduce(&self, vector: &UtilityVector) -> f64 {
        vector.self_value
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ThreatWeightedUtilityReducer;

impl UtilityReducer for ThreatWeightedUtilityReducer {
    fn reduce(&self, vector: &UtilityVector) -> f64 {
        let pressure_cost = vector.opponent_pressures.iter().sum::<f64>() * 0.2;
        vector.self_value + vector.elimination_bonus - vector.crackback_risk - pressure_cost
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SampledReplyUtilityReducer;

impl UtilityReducer for SampledReplyUtilityReducer {
    fn reduce(&self, vector: &UtilityVector) -> f64 {
        let pressure_cost = vector.opponent_pressures.iter().sum::<f64>() * 0.15;
        vector.self_value + vector.elimination_bonus - vector.crackback_risk - pressure_cost
    }
}

pub struct PlannerServices<'a> {
    pub ai_player: PlayerId,
    pub config: &'a AiConfig,
    pub policies: &'a PolicyRegistry,
    pub context: crate::context::AiContext,
    pub utility_reducer: Box<dyn UtilityReducer + 'a>,
    eval_cache: HashMap<u64, f64>,
    /// Search-scoped candidate cache keyed by `candidate_cache_key(state)`
    /// (board state + full `waiting_for` payload — see the function's doc
    /// for why `quick_state_hash` alone is not sufficient).
    /// Sibling search nodes at the same game position reuse a previously
    /// built `AiDecisionContext` instead of re-running `candidate_actions`.
    /// Scope is the `PlannerServices` lifetime — one per `choose_action` call
    /// — so stale entries from prior turns never match.
    candidate_cache: HashMap<u64, std::sync::Arc<AiDecisionContext>>,
    /// Top-level wall-clock deadline mirrored onto services so every hot-path
    /// function (rollouts, tactical_score, evaluate_state_quiesced) can bail
    /// without threading `SearchBudget` everywhere. Populated from the caller's
    /// time budget at construction time; `Deadline::none()` when no budget.
    pub deadline: engine::util::Deadline,
}

impl<'a> PlannerServices<'a> {
    pub fn new(
        ai_player: PlayerId,
        config: &'a AiConfig,
        policies: &'a PolicyRegistry,
        context: crate::context::AiContext,
    ) -> Self {
        let utility_reducer: Box<dyn UtilityReducer + 'a> = match config.search.opponent_model {
            OpponentModel::DeterministicBestReply if config.player_count <= 2 => {
                Box::new(DuelUtilityReducer)
            }
            OpponentModel::DeterministicBestReply | OpponentModel::ThreatWeightedReply => {
                Box::new(ThreatWeightedUtilityReducer)
            }
            OpponentModel::SampledReply => Box::new(SampledReplyUtilityReducer),
        };

        let deadline = match (config.search.deterministic, config.search.time_budget_ms) {
            (false, Some(ms)) => engine::util::Deadline::after(ms),
            _ => engine::util::Deadline::none(),
        };
        // Mirror the same deadline onto AiContext so policies (which only see
        // PolicyContext → AiContext) can gate expensive work — specifically
        // the `velocity_score` opponent-turn projection that costs ~1.5s on
        // large multi-player states.
        let mut context = context;
        context.deadline = deadline;
        Self {
            ai_player,
            config,
            policies,
            context,
            utility_reducer,
            eval_cache: HashMap::new(),
            candidate_cache: HashMap::new(),
            deadline,
        }
    }

    /// Convenience constructor without deck analysis — for tests and non-deck-aware paths.
    pub fn new_default(
        ai_player: PlayerId,
        config: &'a AiConfig,
        policies: &'a PolicyRegistry,
    ) -> Self {
        Self::new(
            ai_player,
            config,
            policies,
            crate::context::AiContext::empty(&config.weights),
        )
    }

    /// Build an `AiDecisionContext` for `state`, reusing a cached one when a
    /// prior search node hit the same `quick_state_hash`. Siblings at the same
    /// game position in a search tree share the result — `candidate_actions`
    /// is not cheap, and search revisits positions often (especially in
    /// beam + rollout configurations).
    pub fn build_decision_context(
        &mut self,
        state: &GameState,
    ) -> std::sync::Arc<AiDecisionContext> {
        // MUST use candidate_cache_key, NOT quick_state_hash: the latter omits
        // state.waiting_for, which is the dispatch key for candidate_actions.
        // Using the wrong hash collides states with identical boards but
        // different WaitingFor (e.g. Priority vs TargetSelection), returning
        // cached candidates from the wrong state.
        let key = candidate_cache_key(state);
        if let Some(hit) = self.candidate_cache.get(&key) {
            return std::sync::Arc::clone(hit);
        }
        let ctx = std::sync::Arc::new(build_decision_context(state));
        self.candidate_cache
            .insert(key, std::sync::Arc::clone(&ctx));
        ctx
    }

    pub fn validate_candidates(
        &self,
        state: &GameState,
        candidates: Vec<CandidateAction>,
    ) -> Vec<CandidateAction> {
        // PassPriority is always legal during Priority (skip simulation for perf),
        // but during ManaPayment it means "finalize payment" which can fail if the
        // player can't actually pay the cost (e.g., Thalia tax makes it unaffordable).
        let pass_always_valid = matches!(
            state.waiting_for,
            engine::types::game_state::WaitingFor::Priority { .. }
        );
        // DecideOptionalCost { pay: false } is always legal — declining an optional
        // cost cannot fail since it just proceeds with the base cost.
        let is_optional_cost = matches!(
            state.waiting_for,
            engine::types::game_state::WaitingFor::OptionalCostChoice { .. }
        );
        candidates
            .into_iter()
            .filter(|candidate| match &candidate.action {
                engine::types::actions::GameAction::PassPriority if pass_always_valid => true,
                engine::types::actions::GameAction::ChooseTarget { .. } => true,
                engine::types::actions::GameAction::DecideOptionalCost { pay: false }
                    if is_optional_cost =>
                {
                    true
                }
                _ => {
                    let mut sim = state.clone();
                    apply_as_current(&mut sim, candidate.action.clone()).is_ok()
                }
            })
            .collect()
    }

    pub fn apply_candidate(
        &self,
        state: &GameState,
        candidate: &CandidateAction,
    ) -> Option<GameState> {
        apply_candidate(state, candidate)
    }

    pub fn evaluate_state(&self, state: &GameState) -> f64 {
        self.evaluate_with_strategy(state)
    }

    /// Cached evaluation: returns a previously computed result if the state hash matches,
    /// avoiding redundant evaluation of identical positions reached via different action orders.
    pub fn evaluate_state_cached(&mut self, state: &GameState) -> f64 {
        let hash = quick_state_hash(state);
        if let Some(&cached) = self.eval_cache.get(&hash) {
            return cached;
        }
        let value = self.evaluate_with_strategy(state);
        if self.eval_cache.len() < 256 {
            self.eval_cache.insert(hash, value);
        }
        value
    }

    /// Evaluate state with both tactical and strategic dimensions.
    /// Tactical eval (evaluate_state) is context-free and uses adjusted weights.
    /// Strategic dimensions (synergy, zone quality, card advantage) use AiContext.
    fn evaluate_with_strategy(&self, state: &GameState) -> f64 {
        let weights = self.context.adjusted_weights.for_turn(state.turn_number);
        let tactical = evaluate_state(state, self.ai_player, weights);

        let synergy = self
            .context
            .synergy_graph()
            .board_synergy_bonus(state, self.ai_player)
            * weights.synergy;

        let zones = crate::zone_eval::zone_bonus(
            state,
            self.ai_player,
            self.context.deck_profile.archetype,
        ) * weights.zone_quality;

        let card_adv =
            crate::card_advantage::differential(state, self.ai_player) * weights.card_advantage;

        tactical + synergy + zones + card_adv + self.threat_adjustment(state)
    }

    /// Adjust evaluation based on opponent threat probabilities.
    /// Penalizes positions where the AI is vulnerable to likely opponent threats:
    /// tapping out against counterspells, or overextending into board wipes.
    fn threat_adjustment(&self, state: &GameState) -> f64 {
        let Some(threat) = &self.context.opponent_threat else {
            return 0.0;
        };

        let penalties = &self.config.policy_penalties;
        let probs = &threat.probabilities;
        let mut adjustment = 0.0;

        // Penalize tapping out when opponent likely has countermagic.
        let ai_mana = crate::zone_eval::available_mana(state, self.ai_player);
        if ai_mana <= 1 && probs.counterspell > 0.3 {
            adjustment += penalties.threat_counter_tapout_penalty * probs.counterspell;
        }

        // Penalize overextending when opponent likely has board wipe.
        let ai_creatures = state
            .battlefield
            .iter()
            .filter(|&&id| {
                state.objects.get(&id).is_some_and(|obj| {
                    obj.controller == self.ai_player
                        && obj
                            .card_types
                            .core_types
                            .contains(&engine::types::card_type::CoreType::Creature)
                })
            })
            .count();
        if ai_creatures >= 3 && probs.board_wipe > 0.2 {
            adjustment += penalties.threat_wipe_overextend_penalty * probs.board_wipe;
        }

        adjustment
    }

    pub fn evaluate_for_planner(&self, state: &GameState) -> ValueEstimate {
        let weights = self.context.adjusted_weights.for_turn(state.turn_number);
        evaluate_for_planner(state, self.ai_player, weights)
    }

    /// Quiescence search: resolve forced actions and mechanical choices until the
    /// position is "quiet" (empty stack with a genuine priority decision, or game
    /// over). This prevents the horizon effect where the search evaluates
    /// mid-stack-resolution positions that misleadingly penalize spell casting.
    ///
    /// Handles three categories of non-strategic actions:
    /// 1. Forced priority passes (only PassPriority is legal)
    /// 2. Single-action states (only one legal action exists)
    /// 3. Deterministic mechanical choices (scry, surveil, discard-to-hand-size, etc.)
    ///    resolved via `deterministic_choice`
    ///
    /// Capped at MAX_QUIESCE_STEPS to prevent runaway loops from cascading triggers.
    fn quiesce(&self, state: &GameState) -> GameState {
        const MAX_QUIESCE_STEPS: u32 = 20;

        let mut sim = state.clone();
        for _ in 0..MAX_QUIESCE_STEPS {
            if matches!(sim.waiting_for, WaitingFor::GameOver { .. }) {
                break;
            }

            let ctx = build_decision_context(&sim);

            // No candidates: nothing to do
            if ctx.candidates.is_empty() {
                break;
            }

            // Case 1: All candidates are PassPriority — resolve the stack
            let all_pass = ctx
                .candidates
                .iter()
                .all(|c| matches!(c.action, engine::types::actions::GameAction::PassPriority));
            if all_pass {
                if apply_as_current(&mut sim, engine::types::actions::GameAction::PassPriority)
                    .is_err()
                {
                    break;
                }
                continue;
            }

            // Case 2: Only one legal action — apply it (forced move)
            if ctx.candidates.len() == 1 {
                if apply_as_current(&mut sim, ctx.candidates[0].action.clone()).is_err() {
                    break;
                }
                continue;
            }

            // Case 3: Deterministic mechanical choice (scry, surveil, mulligan, etc.)
            // These are non-strategic decisions that can be resolved with heuristics.
            let actions: Vec<_> = ctx.candidates.iter().map(|c| c.action.clone()).collect();
            let acting_player = sim.waiting_for.acting_player().unwrap_or(self.ai_player);
            if let Some(action) =
                crate::search::deterministic_choice(&sim, acting_player, self.config, &actions)
            {
                if apply_as_current(&mut sim, action).is_err() {
                    break;
                }
                continue;
            }

            // Genuine decision point — stop quiescence
            break;
        }
        sim
    }

    /// Evaluate a leaf state with quiescence: if the stack is non-empty and only
    /// forced passes remain, resolve through them before evaluating.
    /// Once the wall-clock deadline is blown, skip quiescence — the cached
    /// static eval is a good-enough approximation and quiescence can itself
    /// clone + simulate state through the stack.
    pub fn evaluate_state_quiesced(&mut self, state: &GameState) -> f64 {
        if state.stack.is_empty() || self.deadline.expired() {
            return self.evaluate_state_cached(state);
        }
        let quiesced = self.quiesce(state);
        self.evaluate_state_cached(&quiesced)
    }

    /// Evaluate a leaf state for utility with quiescence.
    pub fn quiesced_leaf_eval(&mut self, state: &GameState) -> f64 {
        if state.stack.is_empty() || self.deadline.expired() {
            let value = self.evaluate_for_planner(state);
            return self.reduce_utility(state, &value);
        }
        let quiesced = self.quiesce(state);
        let value = self.evaluate_for_planner(&quiesced);
        self.reduce_utility(&quiesced, &value)
    }

    pub fn tactical_score(
        &self,
        state: &GameState,
        ctx: &AiDecisionContext,
        candidate: &CandidateAction,
        scoring_player: PlayerId,
    ) -> f64 {
        let cast_facts = cast_facts_for_action(state, &candidate.action, scoring_player);
        let mut score = should_play_now_with_facts(
            state,
            &candidate.action,
            scoring_player,
            cast_facts.as_ref(),
        );
        let intent = strategic_intent(state, scoring_player);
        let policy_ctx = PolicyContext {
            state,
            decision: ctx,
            candidate,
            ai_player: scoring_player,
            config: self.config,
            context: &self.context,
            cast_facts,
        };
        score += self.policies.score(&policy_ctx);

        match candidate.metadata.tactical_class {
            TacticalClass::Pass => {
                score -= 0.1;
                if matches!(
                    intent,
                    StrategicIntent::Develop | StrategicIntent::PushLethal
                ) {
                    score -= 0.15;
                }
            }
            TacticalClass::Mana => score -= 0.05,
            TacticalClass::Land if matches!(intent, StrategicIntent::Develop) => score += 0.2,
            TacticalClass::Attack if matches!(intent, StrategicIntent::PushLethal) => score += 0.3,
            TacticalClass::Block if matches!(intent, StrategicIntent::Stabilize) => score += 0.25,
            _ => {}
        }

        score
    }

    pub fn policy_priors(
        &self,
        state: &GameState,
        ctx: &AiDecisionContext,
        candidates: &[CandidateAction],
        scoring_player: PlayerId,
    ) -> Vec<PolicyPrior> {
        self.policies.priors(
            state,
            ctx,
            candidates,
            scoring_player,
            self.config,
            &self.context,
        )
    }

    pub fn planner_evaluation(&mut self, state: &GameState) -> PlannerEvaluation {
        let ctx = self.build_decision_context(state);
        let candidates = self.validate_candidates(state, ctx.candidates.clone());
        let scoring_player = state.waiting_for.acting_player().unwrap_or(self.ai_player);
        PlannerEvaluation {
            priors: self.policy_priors(state, &ctx, &candidates, scoring_player),
            value: self.evaluate_for_planner(state),
        }
    }

    pub fn utility_vector(&self, state: &GameState, value: &ValueEstimate) -> UtilityVector {
        let opponents = players::opponents(state, self.ai_player);
        let elimination_bonus = opponents
            .iter()
            .filter(|&&opp| state.players[opp.0 as usize].life <= 0)
            .count() as f64
            * 25.0;
        let opponent_pressures: Vec<f64> = opponents
            .iter()
            .map(|&opp| threat_level(state, self.ai_player, opp) * 10.0)
            .collect();
        let crackback_risk = opponent_pressures.iter().sum::<f64>()
            - state.players[self.ai_player.0 as usize].life.max(0) as f64;

        UtilityVector {
            self_value: value.value,
            opponent_pressures,
            elimination_bonus,
            crackback_risk: crackback_risk.max(0.0),
        }
    }

    pub fn reduce_utility(&self, state: &GameState, value: &ValueEstimate) -> f64 {
        self.utility_reducer
            .reduce(&self.utility_vector(state, value))
    }

    pub fn rollout_estimate(&mut self, state: &GameState, depth: u32) -> f64 {
        // CR-agnostic: if the wall-clock budget is blown, short-circuit to the
        // cheap leaf evaluator rather than descending further. Without this
        // bail, rollout recursion ignores `time_budget_ms` entirely.
        if self.deadline.expired() {
            return self.quiesced_leaf_eval(state);
        }
        if depth == 0 || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return self.quiesced_leaf_eval(state);
        }

        let evaluation = self.planner_evaluation(state);
        if evaluation.priors.is_empty() {
            return self.quiesced_leaf_eval(state);
        }

        let rollout_player = state.waiting_for.acting_player().unwrap_or(self.ai_player);
        let sample_count = self.config.search.rollout_samples.max(1) as usize;
        let mut priors = evaluation.priors;
        priors.sort_by(|a, b| {
            b.prior
                .partial_cmp(&a.prior)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let candidates = priors.into_iter().take(sample_count);
        let is_maximizing = rollout_player == self.ai_player;
        candidates
            .filter_map(|prior| {
                let sim = self.apply_candidate(state, &prior.candidate)?;
                let continuation = self.rollout_estimate(&sim, depth - 1);
                Some(continuation + (prior.prior * 0.05))
            })
            .reduce(|best, value| {
                if is_maximizing {
                    best.max(value)
                } else {
                    best.min(value)
                }
            })
            .unwrap_or_else(|| self.quiesced_leaf_eval(state))
    }
}

pub trait ContinuationPlanner {
    fn evaluate_after_action(
        &mut self,
        state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64;
}

#[derive(Debug, Clone, Copy)]
pub struct BeamContinuationPlanner {
    pub depth: u32,
    pub rollout_depth: u32,
}

impl BeamContinuationPlanner {
    fn search_value(
        &self,
        state: &GameState,
        depth: u32,
        mut alpha: f64,
        mut beta: f64,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        budget.tick();
        if depth == 0 {
            return services.rollout_estimate(state, self.rollout_depth);
        }
        if budget.exhausted() || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return services.evaluate_state_quiesced(state);
        }

        let ctx = services.build_decision_context(state);
        // Skip upfront validation in beam search — invalid candidates are handled
        // by apply_candidate returning None in the loop below. This avoids cloning
        // the state once per candidate just to test validity.
        // (planner_evaluation retains validation for MCTS expansion correctness.)
        if ctx.candidates.is_empty() {
            return services.evaluate_state_quiesced(state);
        }

        let node_player = state.waiting_for.acting_player();
        let is_maximizing = node_player.is_none_or(|player| player == services.ai_player);
        let scoring_player = node_player.unwrap_or(services.ai_player);
        let ranked = rank_candidates(
            ctx.candidates.clone(),
            |candidate| services.tactical_score(state, &ctx, candidate, scoring_player),
            services.config.search.max_branching as usize,
        );

        // Alpha-beta pruning: explicit loop for early cutoff.
        // Move ordering from rank_candidates (best-first) maximizes pruning effectiveness.
        let mut best = if is_maximizing {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };

        for ranked in ranked {
            // Bail mid-loop on wall-clock budget: the outer beam can be wide
            // (branching × depth), so checking only at entry lets a single node
            // burn the full deadline before bubbling back up.
            if services.deadline.expired() {
                break;
            }
            let Some(sim) = services.apply_candidate(state, &ranked.candidate) else {
                continue;
            };
            let value = self.search_value(&sim, depth - 1, alpha, beta, services, budget)
                + (ranked.score * 0.05);

            if is_maximizing {
                best = best.max(value);
                alpha = alpha.max(best);
            } else {
                best = best.min(value);
                beta = beta.min(best);
            }

            if alpha >= beta {
                break;
            }
        }

        if best.is_infinite() {
            services.evaluate_state_quiesced(state)
        } else {
            best
        }
    }
}

impl ContinuationPlanner for BeamContinuationPlanner {
    fn evaluate_after_action(
        &mut self,
        state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        if self.depth == 0 {
            services.evaluate_state_quiesced(state)
        } else {
            self.search_value(
                state,
                self.depth,
                f64::NEG_INFINITY,
                f64::INFINITY,
                services,
                budget,
            )
        }
    }
}

pub fn build_continuation_planner(config: &AiConfig) -> Box<dyn ContinuationPlanner> {
    match config.search.planner_mode {
        PlannerMode::BeamOnly => Box::new(BeamContinuationPlanner {
            depth: 0,
            rollout_depth: 0,
        }),
        PlannerMode::BeamPlusRollout => Box::new(BeamContinuationPlanner {
            depth: config.search.max_depth.saturating_sub(1),
            rollout_depth: config.search.rollout_depth,
        }),
    }
}

pub fn rank_candidates<F>(
    candidates: impl IntoIterator<Item = CandidateAction>,
    mut scorer: F,
    limit: usize,
) -> Vec<RankedCandidate>
where
    F: FnMut(&CandidateAction) -> f64,
{
    let mut ranked: Vec<RankedCandidate> = candidates
        .into_iter()
        .map(|candidate| RankedCandidate {
            score: scorer(&candidate),
            candidate,
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    ranked.truncate(limit);
    ranked
}

pub fn apply_candidate(state: &GameState, candidate: &CandidateAction) -> Option<GameState> {
    let mut sim = state.clone();
    apply_as_current(&mut sim, candidate.action.clone()).ok()?;
    Some(sim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::actions::GameAction;
    use engine::types::card_type::CoreType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;

    use crate::config::{create_config, AiDifficulty, Platform};

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    #[test]
    fn rank_candidates_sorts_and_limits() {
        let candidates = vec![
            CandidateAction {
                action: GameAction::PassPriority,
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Pass,
                },
            },
            CandidateAction {
                action: GameAction::MulliganDecision { keep: true },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            },
        ];

        let ranked = rank_candidates(
            candidates,
            |candidate| match candidate.action {
                GameAction::MulliganDecision { .. } => 2.0,
                _ => 1.0,
            },
            1,
        );

        assert_eq!(ranked.len(), 1);
        assert!(matches!(
            ranked[0].candidate.action,
            GameAction::MulliganDecision { .. }
        ));
    }

    #[test]
    fn search_budget_tracks_node_count() {
        let mut budget = SearchBudget::new(3);
        assert!(!budget.exhausted());
        budget.tick();
        budget.tick();
        budget.tick();
        assert!(budget.exhausted());
    }

    #[test]
    fn search_budget_with_time_limit_expires() {
        let budget = SearchBudget::with_time_limit(1000, web_time::Duration::from_millis(0));
        // Zero-duration budget should be immediately exhausted
        assert!(budget.exhausted());
    }

    #[test]
    fn search_budget_time_limit_does_not_override_node_limit() {
        // Large time budget but tiny node budget — node limit should still trigger
        let mut budget = SearchBudget::with_time_limit(2, web_time::Duration::from_secs(60));
        assert!(!budget.exhausted());
        budget.tick();
        budget.tick();
        assert!(budget.exhausted());
    }

    #[test]
    fn planner_services_produce_positive_normalized_priors() {
        let state = make_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let services = PlannerServices::new_default(PlayerId(0), &config, &policies);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                target_slots: Vec::new(),
                target_constraints: Vec::new(),
                selection: Default::default(),
                source_id: None,
                description: None,
            },
            candidates: Vec::new(),
        };
        let candidates = vec![
            CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Player(PlayerId(0))),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Target,
                },
            },
            CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Player(PlayerId(1))),
                },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Target,
                },
            },
        ];

        let priors = services.policy_priors(&state, &decision, &candidates, PlayerId(0));
        assert_eq!(priors.len(), 2);
        assert!(priors
            .iter()
            .all(|prior| prior.prior.is_finite() && prior.prior > 0.0));
        assert!(priors[1].prior > priors[0].prior);
    }

    #[test]
    fn quiesce_is_noop_on_empty_stack() {
        let state = make_state();
        assert!(state.stack.is_empty());
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let services = PlannerServices::new_default(PlayerId(0), &config, &policies);
        let quiesced = services.quiesce(&state);
        assert!(quiesced.stack.is_empty());
        // Board state should be identical
        assert_eq!(quiesced.battlefield.len(), state.battlefield.len());
        assert_eq!(quiesced.players[0].hand.len(), state.players[0].hand.len());
    }

    #[test]
    fn quiesce_resolves_creature_spell_on_stack() {
        use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
        use engine::types::mana::{ManaCost, ManaCostShard};

        let mut state = make_state();
        state.lands_played_this_turn = 1;

        // Add a forest on the battlefield for player 0
        let land_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.controller = PlayerId(0);
        obj.tapped = true; // Already tapped to pay for the creature

        // Add a creature as an object (it'll be on the stack)
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Stack,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
        }

        // Put the creature on the stack
        state.stack.push_back(StackEntry {
            id: creature_id,
            source_id: creature_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(200),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        // Both players have priority, only PassPriority is legal
        // (creature spell on stack, no instant-speed responses available)
        let battlefield_before = state.battlefield.len();

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let services = PlannerServices::new_default(PlayerId(0), &config, &policies);
        let quiesced = services.quiesce(&state);

        // After quiescence, the stack should be resolved
        assert!(
            quiesced.stack.is_empty(),
            "Stack should be empty after quiescence, got {} entries",
            quiesced.stack.len()
        );
        // Creature should now be on the battlefield
        assert!(
            quiesced.battlefield.len() > battlefield_before,
            "Creature should have entered the battlefield: before={}, after={}",
            battlefield_before,
            quiesced.battlefield.len()
        );
    }

    #[test]
    fn quiesced_leaf_eval_credits_pending_creature() {
        use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
        use engine::types::mana::{ManaCost, ManaCostShard};

        let mut state = make_state();
        state.lands_played_this_turn = 1;

        // Add a tapped forest for player 0
        let land_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.controller = PlayerId(0);
            obj.tapped = true;
        }

        // State A: creature in hand (baseline)
        let creature_in_hand = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&creature_in_hand).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
        }

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services_a = PlannerServices::new_default(PlayerId(0), &config, &policies);
        let eval_hand = services_a.evaluate_state_quiesced(&state);

        // State B: same creature on the stack (post-cast)
        let mut state_b = state.clone();
        // Move creature from hand to stack
        state_b.players[0].hand.retain(|&id| id != creature_in_hand);
        let obj = state_b.objects.get_mut(&creature_in_hand).unwrap();
        obj.zone = Zone::Stack;
        state_b.stack.push_back(StackEntry {
            id: creature_in_hand,
            source_id: creature_in_hand,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(200),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut services_b = PlannerServices::new_default(PlayerId(0), &config, &policies);
        let eval_stack = services_b.evaluate_state_quiesced(&state_b);

        // With quiescence, casting a creature should be valued AT LEAST as well as
        // holding it in hand (actually better, since it'll be on the battlefield).
        assert!(
            eval_stack >= eval_hand - 0.5,
            "Creature on stack should evaluate similarly to in hand after quiescence. \
             Stack eval: {eval_stack}, hand eval: {eval_hand}, delta: {}",
            eval_stack - eval_hand
        );
    }
}
