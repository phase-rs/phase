use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};

use engine::ai_support::{
    build_decision_context, AiDecisionContext, CandidateAction, TacticalClass,
};
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::players;
use engine::types::actions::GameAction;
use engine::types::counter::{has_positive_counters, positive_counter_entries};
use engine::types::game_state::{DayNight, GameState, WaitingFor};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use crate::card_hints::should_play_now_with_facts;
use crate::cast_facts::cast_facts_for_action;
use crate::config::{AiConfig, OpponentModel};
use crate::eval::{
    evaluate_for_planner, evaluate_state, strategic_intent, threat_level, StrategicIntent,
};
use crate::policies::context::{PolicyContext, PriorsEnv, SearchDepth};
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

    // Pending state (resolution frames, replacements, triggers affect game
    // flow). The typed stack may retain an ability continuation below its
    // active prompt, so nonempty stacks hash their full ordered wire shape
    // rather than inspecting a non-top frame. The overwhelmingly common empty
    // stack stays allocation-free on this hot cache-key path.
    let has_resolution_frames = !state.resolution_stack.is_empty();
    has_resolution_frames.hash(&mut hasher);
    if has_resolution_frames {
        let resolution_stack = serde_json::to_value(&state.resolution_stack)
            .expect("resolution stack serializes for the planner cache key");
        hash_json_value(&resolution_stack, &mut hasher);
    }
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
            // Counters: HashMap iteration order is non-deterministic, so hash
            // count + sorted positive-count entries for stability. Internal
            // zero-count map keys are absent markers under the engine's counter
            // model and must not perturb AI cache keys.
            // Sort by as_str() to break ties between Generic variants.
            if has_positive_counters(&obj.counters) {
                let mut counter_entries: Vec<_> = positive_counter_entries(&obj.counters).collect();
                counter_entries.sort_by_key(|(counter_type, _)| counter_type.as_str());
                counter_entries.len().hash(&mut hasher);
                for (counter_type, count) in counter_entries {
                    counter_type.hash(&mut hasher);
                    count.hash(&mut hasher);
                }
            } else {
                0usize.hash(&mut hasher);
            }
        }
    }

    hasher.finish()
}

/// Fold one object's keywords into the position hash. Discriminant-first
/// (`KeywordKind: Copy + Hash`) so unit keywords allocate nothing; parameterized
/// payloads are serde-folded because AI tactical scoring reads them in a
/// value-relevant way (Ward cost -> `tactical_gate` `GateDecision::Reject`,
/// tactical_gate.rs; Protection/Enchant/Ward -> `policies/**`), which flows into
/// beam ranking + rollout priors -> the TT'd search value. A bare discriminant
/// would alias e.g. Ward(2) with Ward(100).
fn fold_object_keywords(keywords: &[Keyword], hasher: &mut DefaultHasher) {
    for kw in keywords {
        kw.kind().hash(hasher);
        // Serde-fold the payload only for keywords not provably parameterless.
        // `promote_keyword_kind` is the engine's canonical parameterless-kind
        // registry; a keyword that equals its own promotion carries no payload,
        // so the discriminant above already captures it. Anything else (incl. a
        // future new keyword) serde-folds -> sound by default.
        match Keyword::promote_keyword_kind(kw.kind()) {
            Some(unit) if &unit == kw => {} // parameterless -> discriminant suffices
            _ => hash_json_value(
                &serde_json::to_value(kw).expect("keyword serializes"),
                hasher,
            ),
        }
    }
}

/// Position hash for the transposition table. Field dependency is a strict
/// **superset** of `quick_state_hash`, adding the axes a *bound-returning* TT
/// cannot tolerate aliasing on that the search's own caches don't already
/// protect (a wrong TT hit skips a whole subtree, unlike a wrong eval-cache hit
/// which only perturbs one leaf). See the TT design notes for the per-axis
/// disposition and the two-cache (uncapped candidate_cache / capped eval_cache)
/// argument that makes the omitted axes safe.
pub fn search_position_hash(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();
    // Base: reuse the existing digest, then extend its field dependency.
    quick_state_hash(state).hash(&mut hasher);

    // Exile contents (cast-from-exile candidate gen reads these; quick_state_hash
    // hashes only exile.len()). Mirror the graveyard treatment.
    for &id in &state.exile {
        id.hash(&mut hasher);
    }

    // Full library ordering per player (top-of-library cast / draw-horizon lines).
    for player in &state.players {
        for &id in &player.library {
            id.hash(&mut hasher);
        }
    }

    // commander_damage: read by eval commander-threat in 3+ player games; the
    // eval_cache 256-cap means this eval-leaf read is not reliably memoized.
    // Serialize but not Hash -> serde-fold. Empty in non-commander games -> skipped.
    if !state.commander_damage.is_empty() {
        hash_json_value(
            &serde_json::to_value(&state.commander_damage).expect("commander_damage serializes"),
            &mut hasher,
        );
    }

    // Stack entry targets/modes: NOT covered by either cache (post-apply
    // divergence — e.g. "Shock target A" vs "Shock target B" share source_id +
    // controller). Empty stack (common) -> skipped.
    for entry in &state.stack {
        hash_json_value(
            &serde_json::to_value(entry).expect("stack entry serializes"),
            &mut hasher,
        );
    }

    // Per-battlefield-object: summoning sickness (available_mana eval leaf) and
    // keywords (unit discriminant -> combat eval; parameterized payload -> tactical
    // gate/policies). Both are value-relevant under the capped eval_cache / beam.
    for &obj_id in &state.battlefield {
        if let Some(obj) = state.objects.get(&obj_id) {
            obj.summoning_sick.hash(&mut hasher);
            if !obj.keywords.is_empty() {
                fold_object_keywords(&obj.keywords, &mut hasher);
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
/// a cached context populated with wrong candidates. Include the full
/// `WaitingFor` payload as a canonical structural projection; map keys are
/// sorted so hash-equal waiting states do not depend on `HashMap` iteration
/// order.
pub fn candidate_cache_key(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();
    quick_state_hash(state).hash(&mut hasher);
    hash_waiting_for(&state.waiting_for, &mut hasher);
    hasher.finish()
}

/// TT key: stronger position hash + full `WaitingFor` payload, so a maximizing
/// node and a minimizing node at the same board never share an entry.
pub fn transposition_key(state: &GameState) -> u64 {
    let mut hasher = DefaultHasher::new();
    search_position_hash(state).hash(&mut hasher);
    hash_waiting_for(&state.waiting_for, &mut hasher);
    hasher.finish()
}

/// Alpha-beta bound classification of a stored search value (typed — never a
/// pair of bools).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtBound {
    /// Exact minimax value (window was not cut).
    Exact,
    /// Fail-high: true value is >= `value` (node returned >= beta).
    LowerBound,
    /// Fail-low: true value is <= `value` (node never exceeded alpha).
    UpperBound,
}

#[derive(Debug, Clone, Copy)]
pub struct TtEntry {
    pub depth: u32,
    pub value: f64,
    pub bound: TtBound,
}

/// Defensive ceiling on the transposition table. A single decision searches
/// <= `max_nodes` (<=96) interior nodes per rung across a few ID rungs, so this
/// never binds in practice; it mirrors the `eval_cache` 256-entry guard idiom.
const TT_CAPACITY: usize = 4096;

/// Ply depth covered by the killer-move table. `max_depth` is <= 3 on every
/// difficulty/platform (see `config.rs`), so 8 leaves ample margin; killers
/// recorded past this ply are simply not tracked (inert, never wrong).
const MAX_KILLER_PLY: usize = 8;

/// Per-rung iterative-deepening witness: did the depth-N rung complete, and how
/// much of its node budget it consumed. AI-local search-*quality* record (not an
/// engine perf counter — `perf_counters.rs` is out of scope). A saturated rung
/// is one where `nodes_used >= max_nodes` (`SearchBudget::tick` increments
/// unconditionally while `exhausted()` checks `>=`, so the counter can equal or
/// overshoot the cap by one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RungStat {
    pub depth: u32,
    pub completed: bool,
    pub nodes_used: u32,
    pub max_nodes: u32,
}

fn hash_waiting_for(waiting_for: &WaitingFor, hasher: &mut impl Hasher) {
    let value = serde_json::to_value(waiting_for).expect("WaitingFor serializes");
    hash_json_value(&value, hasher);
}

fn hash_json_value(value: &serde_json::Value, hasher: &mut impl Hasher) {
    match value {
        serde_json::Value::Null => 0u8.hash(hasher),
        serde_json::Value::Bool(value) => {
            1u8.hash(hasher);
            value.hash(hasher);
        }
        serde_json::Value::Number(value) => {
            2u8.hash(hasher);
            if let Some(value) = value.as_i64() {
                0u8.hash(hasher);
                value.hash(hasher);
            } else if let Some(value) = value.as_u64() {
                1u8.hash(hasher);
                value.hash(hasher);
            } else if let Some(value) = value.as_f64() {
                2u8.hash(hasher);
                value.to_bits().hash(hasher);
            }
        }
        serde_json::Value::String(value) => {
            3u8.hash(hasher);
            value.hash(hasher);
        }
        serde_json::Value::Array(values) => {
            4u8.hash(hasher);
            values.len().hash(hasher);
            for value in values {
                hash_json_value(value, hasher);
            }
        }
        serde_json::Value::Object(entries) => {
            5u8.hash(hasher);
            entries.len().hash(hasher);
            let mut entries: Vec<_> = entries.iter().collect();
            entries.sort_by_key(|(left, _)| *left);
            for (key, value) in entries {
                key.hash(hasher);
                hash_json_value(value, hasher);
            }
        }
    }
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
    /// Interior-node search-value memo with alpha-beta bound + depth semantics
    /// (distinct from `eval_cache`, a value-only *leaf* memo). Scope is the
    /// `PlannerServices` lifetime — one decision — so no cross-turn staleness.
    transposition_table: HashMap<u64, TtEntry>,
    /// Count of TT cutoffs served this decision. Not an engine perf counter
    /// (perf_counters.rs is out of scope); a local witness that a re-search was
    /// actually skipped, used by the TT-hit regression test.
    pub tt_hits: u32,
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
    /// CR-free search heuristic: the last 2 moves (indexed by ply-from-root) that
    /// caused a beta cutoff. Decision-scoped like the TT — rebuilt per
    /// determinized sample (`score_candidates_core`), so killers never leak
    /// across sampled worlds. `GameAction` is `PartialEq` but not `Hash`, so a
    /// fixed 2-slot array compared by `==` is the idiomatic fit (no serialized
    /// key on the hot path; that key exists for cross-sample HashMap merging).
    /// `pub(crate)` for cross-module witness tests (mirrors `tt_hits`).
    pub(crate) killers: [[Option<GameAction>; 2]; MAX_KILLER_PLY],
    /// Witness: beta cutoffs recorded this decision. Not an engine perf counter;
    /// the reach-guard that a cutoff branch actually executed (see V1/V8 tests).
    pub beta_cutoffs: u32,
    /// Witness: times a killer was present in a beam and rotated forward. The
    /// direct efficacy signal for the within-beam-vs-rescue follow-up decision.
    pub killer_orderings: u32,
    /// Witness: one entry per iterative-deepening rung, pushed by
    /// `run_iterative_deepening`. Evidence that ordering work converts to
    /// realized search depth (did the deep rung complete, with how much
    /// node headroom).
    pub rung_stats: Vec<RungStat>,
}

impl<'a> PlannerServices<'a> {
    pub fn new(
        ai_player: PlayerId,
        config: &'a AiConfig,
        policies: &'a PolicyRegistry,
        context: crate::context::AiContext,
    ) -> Self {
        Self::with_deadline(ai_player, config, policies, context, None)
    }

    /// Like [`PlannerServices::new`] but lets the caller supply a shared
    /// wall-clock ceiling (`deadline_override`) instead of deriving a fresh one
    /// from `config.search.time_budget_ms`. The determinized ensemble
    /// (`score_candidates_with_session`) uses this so all K per-sample searches
    /// share ONE `Deadline::after(time_budget_ms)` — aggregate latency stays
    /// bounded at ~budget rather than K x budget.
    ///
    /// Measurement mode ALWAYS wins: the override is ignored and the deadline is
    /// `Deadline::none()` (bounded solely by node/depth budgets), matching
    /// `new`'s measurement semantics so `cargo ai-gate` / duel-suite runs stay
    /// byte-deterministic regardless of K. `None` reproduces `new` exactly.
    ///
    /// The chosen deadline is mirrored onto BOTH `self.deadline` (hot-path
    /// rollout/eval bail-out) AND `self.context.deadline` (so policies gating on
    /// `AiContext`, e.g. the `velocity_score` opponent-turn projection, see the
    /// same ceiling). Missing the context mirror would let per-sample projections
    /// run unbounded, reintroducing K x latency.
    pub fn with_deadline(
        ai_player: PlayerId,
        config: &'a AiConfig,
        policies: &'a PolicyRegistry,
        context: crate::context::AiContext,
        deadline_override: Option<engine::util::Deadline>,
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

        let deadline = if config.execution_mode.is_measurement() {
            // Measurement mode is bounded by node/depth only — never wall clock,
            // even under an override (keeps ai-gate deterministic across K).
            engine::util::Deadline::none()
        } else {
            match deadline_override {
                Some(shared) => shared,
                None => match config.search.time_budget_ms {
                    Some(ms) => engine::util::Deadline::after(ms),
                    None => engine::util::Deadline::none(),
                },
            }
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
            transposition_table: HashMap::new(),
            tt_hits: 0,
            candidate_cache: HashMap::new(),
            deadline,
            killers: Default::default(),
            beta_cutoffs: 0,
            killer_orderings: 0,
            rung_stats: Vec::new(),
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
                // MulliganDecision is always valid — the engine generates Keep and
                // Mulligan as the complete legal set; neither can fail apply_as_current.
                // Skipping the clone+simulate eliminates ~2 state clones per mulligan step.
                engine::types::actions::GameAction::MulliganDecision { .. } => true,
                _ => {
                    engine::game::perf_counters::record_state_clone_for_legality();
                    let mut sim = state.clone();
                    apply_as_current_for_simulation(&mut sim, candidate.action.clone()).is_ok()
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

    /// Sample up to `sample_count` legal continuations, prior-ranked with
    /// legality backfill: candidates are sorted by prior (desc), applied in
    /// order, and illegal ones (`apply_candidate` → None) are skipped WITHOUT
    /// consuming a sample slot, so a high-prior illegal candidate does not
    /// starve the sample. Returns each surviving continuation paired with the
    /// prior that produced it. `&self` — only reads via `apply_candidate`.
    fn sample_backfilled_continuations(
        &self,
        state: &GameState,
        mut priors: Vec<PolicyPrior>,
        sample_count: usize,
    ) -> Vec<(f64, GameState)> {
        priors.sort_by(|a, b| {
            b.prior
                .partial_cmp(&a.prior)
                .unwrap_or(std::cmp::Ordering::Equal)
                // Issue #4878: without this tie-break, a prior tie falls back
                // to `priors`' pre-sort order, ultimately traceable to the
                // engine's unsorted `candidate_actions(state)` — mirrors
                // search.rs:1956/2205 and the sibling fix in `rank_candidates`.
                .then_with(|| a.candidate.action.cmp_stable(&b.candidate.action))
        });
        // Bounded rewrite of the terminal `filter_map(...).take(sample_count)`:
        // stop on the sample cap OR once the wall-clock deadline is blown, while
        // preserving legality backfill exactly (an illegal candidate is skipped
        // WITHOUT consuming a sample slot). Byte-identical to the old iterator
        // when the deadline is live — same set of applied candidates, same order,
        // same result — because `deadline.expired()` is always false then.
        let mut out = Vec::with_capacity(sample_count);
        for prior in priors {
            if out.len() == sample_count || self.deadline.expired() {
                break;
            }
            if let Some(sim) = self.apply_candidate(state, &prior.candidate) {
                out.push((prior.prior, sim));
            }
        }
        out
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

    /// Probe the TT. Returns `Some(value)` only when a stored entry proves an
    /// alpha-beta cutoff for the current window at sufficient depth — otherwise
    /// `None` (caller searches normally). Exhaustive match over `TtBound`.
    fn tt_probe(&mut self, key: u64, depth: u32, alpha: f64, beta: f64) -> Option<f64> {
        let entry = *self.transposition_table.get(&key)?;
        if entry.depth < depth {
            return None; // shallower than we need — not trustworthy for this rung
        }
        let hit = match entry.bound {
            TtBound::Exact => Some(entry.value),
            TtBound::LowerBound if entry.value >= beta => Some(entry.value),
            TtBound::UpperBound if entry.value <= alpha => Some(entry.value),
            _ => None,
        };
        if hit.is_some() {
            self.tt_hits += 1;
        }
        hit
    }

    /// Store a search result. `alpha_orig`/`beta_orig` (the ORIGINAL window
    /// captured before the alpha-beta loop mutated `alpha`/`beta`) classify the
    /// bound. Classifying against the *original* window on both sides is required
    /// because `search_value` is true minimax (not negamax): at a minimizing node
    /// `beta` is lowered during search, so a fully-searched exact value would
    /// satisfy `value >= (mutated) beta` and be mislabeled `LowerBound`, silently
    /// losing TT hits at min nodes. Depth-preferred replacement: keep a strictly
    /// deeper existing entry; otherwise insert (respecting the capacity ceiling
    /// for new keys).
    fn tt_store(&mut self, key: u64, depth: u32, value: f64, alpha_orig: f64, beta_orig: f64) {
        let bound = if value <= alpha_orig {
            TtBound::UpperBound // fail-low
        } else if value >= beta_orig {
            TtBound::LowerBound // fail-high
        } else {
            TtBound::Exact
        };
        match self.transposition_table.get(&key) {
            Some(existing) if existing.depth > depth => {} // keep the deeper entry
            _ if self.transposition_table.len() >= TT_CAPACITY
                && !self.transposition_table.contains_key(&key) => {} // cap guard
            _ => {
                self.transposition_table.insert(
                    key,
                    TtEntry {
                        depth,
                        value,
                        bound,
                    },
                );
            }
        }
    }

    /// Record `action` as the primary killer at `ply` using standard two-slot
    /// replacement: shift slot 0 -> slot 1, then store into slot 0. No-op when
    /// `action` already occupies slot 0 (keeps slot 1 as the second-best killer)
    /// or when `ply` exceeds the tracked depth.
    fn record_killer(&mut self, ply: usize, action: &GameAction) {
        if ply >= MAX_KILLER_PLY {
            return;
        }
        if self.killers[ply][0].as_ref() == Some(action) {
            return; // already the primary killer — don't evict the secondary
        }
        self.killers[ply][1] = self.killers[ply][0].take();
        self.killers[ply][0] = Some(action.clone());
    }

    /// Reorder-only killer heuristic: stable-rotate candidates whose action
    /// matches a killer slot at `ply` to the front of the already-truncated beam
    /// (slot 0 before slot 1), preserving relative order among killers and among
    /// non-killers. **Scores are never modified** — killers change only visit
    /// order, so the value function (`ranked.score` uses at planner/mod.rs and
    /// search.rs) is untouched. Increments `killer_orderings` only when a killer
    /// was actually present in the beam.
    fn order_killers_first(&mut self, ply: usize, ranked: &mut [RankedCandidate]) {
        if ply >= MAX_KILLER_PLY {
            return;
        }
        let killers = &self.killers[ply];
        // 0 = primary killer, 1 = secondary killer, 2 = non-killer. A stable sort
        // on this key rotates killers to the front while preserving the relative
        // order of every other candidate (and of the two killer slots).
        let rank = |r: &RankedCandidate| -> u8 {
            if killers[0].as_ref() == Some(&r.candidate.action) {
                0
            } else if killers[1].as_ref() == Some(&r.candidate.action) {
                1
            } else {
                2
            }
        };
        if ranked.iter().any(|r| rank(r) < 2) {
            ranked.sort_by_key(rank);
            self.killer_orderings += 1;
        }
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
            // Search wall-clock bound (CR-agnostic): once the deadline is blown,
            // stop resolving the stack. A single quiesce can otherwise run up to
            // MAX_QUIESCE_STEPS uninterruptible applies (e.g. a finite
            // mega-cascade resolution) after the budget has expired, because the
            // callers only check the deadline at entry. Returning the
            // partially-quiesced state to the cheap leaf evaluator is the same
            // approximation the deadline makes elsewhere. This breaks AFTER the
            // crossing apply (at loop-top on the next iteration), so quiesce is
            // bounded to at most one in-flight apply past expiry. It does NOT
            // bound intra-step cost inside `deterministic_choice` (Case 3); low
            // risk because the Ugin cascade resolves via Case 1 forced-pass.
            if self.deadline.expired() {
                break;
            }

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
                if apply_as_current_for_simulation(
                    &mut sim,
                    engine::types::actions::GameAction::PassPriority,
                )
                .is_err()
                {
                    break;
                }
                continue;
            }

            // Case 2: Only one legal action — apply it (forced move)
            if ctx.candidates.len() == 1 {
                if apply_as_current_for_simulation(&mut sim, ctx.candidates[0].action.clone())
                    .is_err()
                {
                    break;
                }
                continue;
            }

            // Case 3: Deterministic mechanical choice (scry, surveil, mulligan, etc.)
            // These are non-strategic decisions that can be resolved with heuristics.
            let actions: Vec<_> = ctx.candidates.iter().map(|c| c.action.clone()).collect();
            let acting_player = sim.waiting_for.acting_player().unwrap_or(self.ai_player);
            if let Some(action) = crate::search::deterministic_choice(
                &sim,
                acting_player,
                self.config,
                &actions,
                None,
            ) {
                if apply_as_current_for_simulation(&mut sim, action).is_err() {
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
        search_depth: SearchDepth,
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
            search_depth,
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
        search_depth: SearchDepth,
    ) -> Vec<PolicyPrior> {
        let env = PriorsEnv {
            state,
            decision: ctx,
            ai_player: scoring_player,
            config: self.config,
            context: &self.context,
            search_depth,
        };
        self.policies.priors(&env, candidates)
    }

    pub fn planner_evaluation(&mut self, state: &GameState) -> PlannerEvaluation {
        // Rollout leaf skips upfront legality validation — illegal candidates are
        // dropped at apply time by `sample_backfilled_continuations`
        // (`apply_candidate` → None), mirroring the beam path. This removes one
        // clone-and-apply-per-candidate probe (`state_clone_for_legality`).
        let ctx = self.build_decision_context(state);
        let scoring_player = state.waiting_for.acting_player().unwrap_or(self.ai_player);
        PlannerEvaluation {
            // Rollout leaf: every node reached here is deep lookahead, never the
            // committed decision, so board-wide/affordability policies self-gate.
            priors: self.policy_priors(
                state,
                &ctx,
                &ctx.candidates,
                scoring_player,
                SearchDepth::Lookahead,
            ),
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
        let continuations =
            self.sample_backfilled_continuations(state, evaluation.priors, sample_count);
        if continuations.is_empty() {
            return self.quiesced_leaf_eval(state);
        }
        // CR-agnostic: if the budget was blown while sampling continuations,
        // short-circuit to the cheap leaf eval instead of descending the rollout
        // tree. Closes the post-sampling path the reporter flagged; the top-of-
        // function guard covers a deadline already expired on entry.
        if self.deadline.expired() {
            return self.quiesced_leaf_eval(state);
        }
        let is_maximizing = rollout_player == self.ai_player;
        continuations
            .into_iter()
            .map(|(prior, sim)| self.rollout_estimate(&sim, depth - 1) + (prior * 0.05))
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
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search_value(
        &self,
        state: &GameState,
        depth: u32,
        ply: usize,
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

        let key = transposition_key(state);
        if let Some(v) = services.tt_probe(key, depth, alpha, beta) {
            return v; // re-search skipped
        }
        // Capture the ORIGINAL window before the alpha-beta loop mutates it, so
        // `tt_store` classifies the bound against the window this node opened with
        // on BOTH sides (min-node `beta` lowering must not mislabel an Exact value).
        let alpha_orig = alpha;
        let beta_orig = beta;

        let ctx = services.build_decision_context(state);
        // Skip upfront validation in beam search — invalid candidates are handled
        // by apply_candidate returning None in the loop below. This avoids cloning
        // the state once per candidate just to test validity. The rollout leaf
        // path (planner_evaluation → sample_backfilled_continuations) applies the
        // same skip: illegal candidates are dropped when apply_candidate returns
        // None during backfill sampling, not by an upfront clone-per-candidate probe.
        if ctx.candidates.is_empty() {
            return services.evaluate_state_quiesced(state);
        }

        let node_player = state.waiting_for.acting_player();
        let is_maximizing = node_player.is_none_or(|player| player == services.ai_player);
        let scoring_player = node_player.unwrap_or(services.ai_player);
        let mut ranked = rank_candidates(
            ctx.candidates.clone(),
            // Interior beam node: `search_value` is always entered ≥1 ply below
            // the decision root, so move-ordering scoring runs in lookahead.
            |candidate| {
                services.tactical_score(
                    state,
                    &ctx,
                    candidate,
                    scoring_player,
                    SearchDepth::Lookahead,
                )
            },
            services.config.search.max_branching as usize,
        );
        // Move ordering: try killer moves (prior beta-cutoff causers at this ply)
        // first to maximize alpha-beta pruning. Reorder-only — candidate scores
        // are never mutated (they leak into the value function below).
        services.order_killers_first(ply, &mut ranked);

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
            let value = self.search_value(&sim, depth - 1, ply + 1, alpha, beta, services, budget)
                + (ranked.score * 0.05);

            if is_maximizing {
                best = best.max(value);
                alpha = alpha.max(best);
            } else {
                best = best.min(value);
                beta = beta.min(best);
            }

            if alpha >= beta {
                // Beta cutoff: record the cutting move as a killer for this ply
                // and count the cutoff (both witness-only; scores untouched).
                services.beta_cutoffs += 1;
                services.record_killer(ply, &ranked.candidate.action);
                break;
            }
        }

        let result = if best.is_infinite() {
            services.evaluate_state_quiesced(state)
        } else {
            best
        };
        // Budget-truncation guard: only memoize a fully-explored node.
        // `budget.exhausted()` includes `deadline.expired()`, so a node that broke
        // early on the mid-loop deadline bail is NOT stored — only genuinely
        // completed nodes enter the TT.
        if !budget.exhausted() {
            services.tt_store(key, depth, result, alpha_orig, beta_orig);
        }
        result
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
            // Ply-from-root starts at 0 for the rung root: a killer recorded at
            // ply 1 of one rung is reused at ply 1 of the next (chess convention).
            self.search_value(
                state,
                self.depth,
                0,
                f64::NEG_INFINITY,
                f64::INFINITY,
                services,
                budget,
            )
        }
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
            // Issue #4878: without this tie-break, equal-score candidates fall
            // back to `ranked`'s pre-sort (enumeration) order, which is not
            // guaranteed stable across processes — mirrors search.rs:1956/2205.
            .then_with(|| a.candidate.action.cmp_stable(&b.candidate.action))
    });
    ranked.truncate(limit);
    ranked
}

pub fn apply_candidate(state: &GameState, candidate: &CandidateAction) -> Option<GameState> {
    let mut sim = state.clone();
    apply_as_current_for_simulation(&mut sim, candidate.action.clone()).ok()?;
    Some(sim)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::combat::BlockRequirement;
    use engine::game::perf_counters;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        ResolvedAbility, TargetRef,
    };
    use engine::types::actions::{GameAction, MulliganChoice};
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterType;
    use engine::types::game_state::{
        CommanderDamageEntry, PendingContinuation, StackEntry, StackEntryKind, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::WardCost;
    use engine::types::mana::ManaColor;
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use std::collections::HashMap;

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
    fn with_deadline_mirrors_override_onto_context() {
        // N1: the shared ensemble deadline must reach BOTH `self.deadline` AND
        // `context.deadline`. Use a non-measurement config whose own
        // `time_budget_ms` is None so, absent the override, BOTH deadlines would
        // be `none()` (remaining() == None). Supplying the override then proves
        // the mirror specifically: if the code set only `self.deadline`,
        // `context.deadline` would remain `none()` and this test would fail.
        let mut config = create_config(AiDifficulty::Hard, Platform::Native);
        config.search.time_budget_ms = None; // no config-derived deadline
        let policies = crate::policies::PolicyRegistry::shared();
        let context = crate::context::AiContext::empty(&config.weights);
        let services = PlannerServices::with_deadline(
            PlayerId(0),
            &config,
            policies,
            context,
            Some(engine::util::Deadline::after(60_000)),
        );
        assert!(
            services.deadline.remaining().is_some(),
            "override must reach self.deadline"
        );
        assert!(
            services.context.deadline.remaining().is_some(),
            "override must be MIRRORED onto context.deadline (N1)"
        );
    }

    #[test]
    fn with_deadline_measurement_ignores_override() {
        // Measurement mode is bounded by node/depth only: the override must be
        // dropped so `cargo ai-gate` stays byte-deterministic across K samples.
        let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(7);
        let policies = crate::policies::PolicyRegistry::shared();
        let context = crate::context::AiContext::empty(&config.weights);
        let services = PlannerServices::with_deadline(
            PlayerId(0),
            &config,
            policies,
            context,
            Some(engine::util::Deadline::after(60_000)),
        );
        assert!(services.deadline.remaining().is_none());
        assert!(services.context.deadline.remaining().is_none());
    }

    /// Serve reconstruction ≡ planner leaf eval. The harvested `FeatureRow`
    /// weighted by the archetype-adjusted weights the planner actually applies,
    /// plus the TWO serve-time carve-outs (`energy_offset`, `threat_adjustment`),
    /// must equal `evaluate_with_strategy` to 1e-9. This pins the train/serve
    /// invariant: a future `evaluate_with_strategy` term without a matching
    /// `FeatureRow` field would break this identity loudly. Runs a reach-guard
    /// pair — one config where `threat_adjustment` is provably nonzero, one where
    /// it is zero — and asserts the energy term is non-vacuous (`p0.energy > 0`).
    #[test]
    fn serve_reconstruction_equals_planner_leaf_eval() {
        use crate::context::AiContext;
        use crate::deck_profile::DeckArchetype;
        use crate::duel_suite::harvest::FeatureRow;
        use crate::threat_profile::{ThreatProbabilities, ThreatProfile};
        use engine::game::DeckEntry;
        use engine::types::card::CardFace;
        use engine::types::card_type::{CardType, CoreType};

        // A small aggressive deck so `adjusted_weights` genuinely differs from the
        // base set (exercises the archetype-adjusted path, not a no-op remap).
        let deck: Vec<DeckEntry> = (0..8)
            .map(|i| DeckEntry {
                card: CardFace {
                    name: format!("Goblin {i}"),
                    card_type: CardType {
                        core_types: vec![CoreType::Creature],
                        subtypes: vec!["Goblin".to_string()],
                        ..Default::default()
                    },
                    ..Default::default()
                },
                count: 4,
            })
            .collect();

        // Mid-game, non-terminal state from p0's perspective, energy > 0, and NO
        // lands for p0 so `available_mana(p0) <= 1` makes the counter-tapout
        // penalty reachable.
        let mut state = make_state();
        state.turn_number = 5;
        state.players[0].life = 17;
        state.players[1].life = 12;
        state.players[0].energy = 4;
        for (owner, power, toughness) in [
            (PlayerId(0), 2, 1),
            (PlayerId(0), 3, 3),
            (PlayerId(1), 4, 4),
        ] {
            let card_id = CardId(state.next_object_id);
            let id = create_object(
                &mut state,
                card_id,
                owner,
                "Body".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(power);
            obj.toughness = Some(toughness);
        }

        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = crate::policies::PolicyRegistry::shared();

        // Case A: opponent_threat present with a high counterspell probability →
        // threat_adjustment is nonzero.
        let mut ctx_threat = AiContext::analyze_for_player(
            &deck,
            &config.weights,
            &config.archetype_multipliers,
            PlayerId(0),
        );
        ctx_threat.opponent_threat = Some(ThreatProfile {
            probabilities: ThreatProbabilities {
                counterspell: 0.9,
                ..Default::default()
            },
            opponent_archetype: DeckArchetype::Control,
            category_pools: Default::default(),
            pool_size: 0,
            hand_size: 0,
        });
        let services = PlannerServices::new(PlayerId(0), &config, policies, ctx_threat);

        let turn = state.turn_number;
        let weights = services.context.adjusted_weights.for_turn(turn);
        let row = FeatureRow::extract(&state, &services.context.session, PlayerId(0))
            .expect("mid-game state is non-terminal");
        let threat_adj = services.threat_adjustment(&state);
        let reconstructed = row.weighted_total(weights) + row.energy_offset + threat_adj;
        let planner_eval = services.evaluate_with_strategy(&state);

        assert!(
            row.energy_offset > 0.0,
            "energy term must be non-vacuous (p0.energy > 0)"
        );
        assert!(
            threat_adj.abs() > 0.0,
            "reach-guard: threat_adjustment must be provably nonzero here, got {threat_adj}"
        );
        assert!(
            (planner_eval - reconstructed).abs() < 1e-9,
            "serve reconstruction diverged: planner={planner_eval} reconstructed={reconstructed}"
        );

        // Case B: no opponent_threat → threat_adjustment == 0, identity still holds
        // (the reach-guard's zero pole).
        let ctx_none = AiContext::analyze_for_player(
            &deck,
            &config.weights,
            &config.archetype_multipliers,
            PlayerId(0),
        );
        let services_none = PlannerServices::new(PlayerId(0), &config, policies, ctx_none);
        let weights_none = services_none.context.adjusted_weights.for_turn(turn);
        let row_none = FeatureRow::extract(&state, &services_none.context.session, PlayerId(0))
            .expect("non-terminal");
        let threat_adj_none = services_none.threat_adjustment(&state);
        let reconstructed_none = row_none.weighted_total(weights_none) + row_none.energy_offset;
        let planner_eval_none = services_none.evaluate_with_strategy(&state);

        assert_eq!(
            threat_adj_none, 0.0,
            "no opponent_threat ⇒ threat_adjustment is exactly zero"
        );
        assert!(
            (planner_eval_none - reconstructed_none).abs() < 1e-9,
            "serve reconstruction (no threat) diverged: planner={planner_eval_none} \
             reconstructed={reconstructed_none}"
        );
    }

    #[test]
    fn candidate_cache_key_hashes_waiting_for_payload() {
        let state = make_state();
        let mut same = state.clone();
        let mut different = state.clone();
        different.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        assert_eq!(candidate_cache_key(&state), candidate_cache_key(&same));
        assert_ne!(candidate_cache_key(&state), candidate_cache_key(&different));

        same.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert_eq!(candidate_cache_key(&state), candidate_cache_key(&same));
    }

    #[test]
    fn candidate_cache_key_canonicalizes_waiting_for_maps() {
        let mut left_targets = HashMap::new();
        left_targets.insert(ObjectId(10), vec![ObjectId(1), ObjectId(2)]);
        left_targets.insert(ObjectId(20), vec![ObjectId(3)]);
        let mut left_requirements = HashMap::new();
        left_requirements.insert(
            ObjectId(10),
            BlockRequirement {
                count: 2,
                sources: vec![],
            },
        );
        left_requirements.insert(
            ObjectId(20),
            BlockRequirement {
                count: 1,
                sources: vec![],
            },
        );

        let mut right_targets = HashMap::new();
        right_targets.insert(ObjectId(20), vec![ObjectId(3)]);
        right_targets.insert(ObjectId(10), vec![ObjectId(1), ObjectId(2)]);
        let mut right_requirements = HashMap::new();
        right_requirements.insert(
            ObjectId(20),
            BlockRequirement {
                count: 1,
                sources: vec![],
            },
        );
        right_requirements.insert(
            ObjectId(10),
            BlockRequirement {
                count: 2,
                sources: vec![],
            },
        );

        let mut left = make_state();
        left.waiting_for = WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![ObjectId(30)],
            valid_block_targets: left_targets,
            block_requirements: left_requirements,
            blocker_constraints: Default::default(),
        };

        let mut right = make_state();
        right.waiting_for = WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![ObjectId(30)],
            valid_block_targets: right_targets,
            block_requirements: right_requirements,
            blocker_constraints: Default::default(),
        };

        assert_eq!(candidate_cache_key(&left), candidate_cache_key(&right));
    }

    #[test]
    fn rank_candidates_sorts_and_limits() {
        let candidates = vec![
            CandidateAction {
                action: GameAction::PassPriority,
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
            },
            CandidateAction {
                action: GameAction::MulliganDecision {
                    choice: MulliganChoice::Keep,
                },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
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

    /// Issue #4878: on an exact score tie, `rank_candidates`' tie-break must
    /// be `GameAction::cmp_stable` (a total order), not `ranked`'s pre-sort
    /// (encounter) order. Reverting the `.then_with(cmp_stable)` tie-break
    /// makes this test flip: `sort_by` is stable, so two calls differing only
    /// in input order would then rank the same tied pair differently.
    #[test]
    fn rank_candidates_ties_break_by_cmp_stable_not_encounter_order() {
        let pass = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
        };
        let mulligan = CandidateAction {
            action: GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let tied_scorer = |_candidate: &CandidateAction| 1.0;

        let forward = rank_candidates(vec![pass.clone(), mulligan.clone()], tied_scorer, 2);
        let reversed = rank_candidates(vec![mulligan, pass], tied_scorer, 2);

        let forward_actions: Vec<_> = forward.iter().map(|r| r.candidate.action.clone()).collect();
        let reversed_actions: Vec<_> = reversed
            .iter()
            .map(|r| r.candidate.action.clone())
            .collect();

        assert_eq!(
            forward_actions, reversed_actions,
            "tied candidates must rank identically regardless of input encounter order"
        );
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
    fn quick_state_hash_ignores_stale_zero_count_counter_entries() {
        let mut absent = make_state();
        let object_id = create_object(
            &mut absent,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        absent
            .objects
            .get_mut(&object_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut stale = absent.clone();
        stale
            .objects
            .get_mut(&object_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 0);

        let mut positive = absent.clone();
        positive
            .objects
            .get_mut(&object_id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        assert_eq!(quick_state_hash(&absent), quick_state_hash(&stale));
        assert_ne!(quick_state_hash(&absent), quick_state_hash(&positive));
    }

    #[test]
    fn quick_state_hash_distinguishes_typed_resolution_stack() {
        let state = make_state();
        let mut suspended = state.clone();
        let continuation = PendingContinuation::new(
            Box::new(ResolvedAbility::new(
                Effect::NoOp,
                Vec::new(),
                ObjectId(0),
                PlayerId(0),
            )),
            &suspended,
        );
        suspended.park_ability_continuation(continuation);

        // Revert-failing: omitting the ordered typed frame stack aliases an
        // active resolution with an otherwise identical priority state.
        assert_ne!(quick_state_hash(&state), quick_state_hash(&suspended));
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
                trigger_controller: None,
                trigger_event: None,
                trigger_events: Vec::new(),
                target_slots: Vec::new(),
                mode_labels: Vec::new(),
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
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
            },
            CandidateAction {
                action: GameAction::ChooseTarget {
                    target: Some(engine::types::ability::TargetRef::Player(PlayerId(1))),
                },
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
            },
        ];

        let priors = services.policy_priors(
            &state,
            &decision,
            &candidates,
            PlayerId(0),
            SearchDepth::Lookahead,
        );
        assert_eq!(priors.len(), 2);
        assert!(priors.iter().all(|prior| prior.prior.is_finite()));
        assert!(priors.iter().any(|prior| prior.prior > 0.0));
        assert_eq!(priors[0].prior, 0.0);
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

    /// A state with a single Grizzly Bears creature spell on the stack, priced so
    /// only `PassPriority` is legal for both players — so `quiesce` Case 1 resolves
    /// it in one apply. Shared by the resolution test and the deadline-bound tests.
    fn creature_spell_on_stack_state() -> GameState {
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

        state
    }

    #[test]
    fn quiesce_resolves_creature_spell_on_stack() {
        // Both players have priority, only PassPriority is legal
        // (creature spell on stack, no instant-speed responses available)
        let state = creature_spell_on_stack_state();
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

    /// V1: `quiesce` must break at loop top once the search deadline is blown,
    /// leaving the resolvable stack UNCHANGED. Paired with a live-deadline
    /// positive that DOES resolve, so the negative is non-vacuous. Deleting the
    /// `if self.deadline.expired() { break; }` guard makes the negative fail.
    #[test]
    fn quiesce_honors_expired_deadline() {
        let state = creature_spell_on_stack_state();
        let stack_before = state.stack.len();
        assert!(stack_before > 0, "fixture must have a non-empty stack");

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        // Refinement V1/V2: the deadline lives on PlannerServices and is HONORED
        // only in non-measurement mode. `with_deadline` force-overrides any
        // injected deadline to `none()` under measurement, which would make the
        // guard inert and the negative assertion vacuous.
        assert!(
            !config.execution_mode.is_measurement(),
            "config must be non-measurement so the injected deadline is honored"
        );
        let policies = PolicyRegistry::default();

        // Positive reach-guard (non-vacuity): a live deadline resolves the stack.
        let live = PlannerServices::with_deadline(
            PlayerId(0),
            &config,
            &policies,
            crate::context::AiContext::empty(&config.weights),
            Some(engine::util::Deadline::none()),
        );
        let resolved = live.quiesce(&state);
        assert!(
            resolved.stack.len() < stack_before,
            "non-vacuity: quiesce with a live deadline must resolve the stack \
             (before={stack_before}, after={})",
            resolved.stack.len()
        );

        // Negative (the fix): an expired deadline leaves the stack untouched.
        let expired = PlannerServices::with_deadline(
            PlayerId(0),
            &config,
            &policies,
            crate::context::AiContext::empty(&config.weights),
            Some(engine::util::Deadline::after(0)),
        );
        // Witness: the deadline is genuinely expired before quiesce runs, so a
        // pass is not a broken probe that never entered the loop body.
        assert!(
            expired.deadline.expired(),
            "witness: the injected deadline must be expired so the guard fires"
        );
        let unchanged = expired.quiesce(&state);
        assert_eq!(
            unchanged.stack.len(),
            stack_before,
            "revert-failing: quiesce must break at loop top on an expired deadline"
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

    /// Fixture: player 0 has a playable land in hand and an open land drop, so
    /// candidate generation yields a `PlayLand` candidate. That candidate hits
    /// `validate_candidates`' clone arm (mod.rs `_ =>`), giving these tests a
    /// live legality-probe path to measure against.
    fn make_state_with_land() -> GameState {
        let mut state = make_state();
        state.lands_played_this_turn = 0;
        let land_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        obj.controller = PlayerId(0);
        state
    }

    // T1: planner_evaluation must not clone-and-apply per candidate for legality
    // (Edit 1 removed the validate_candidates probe from the rollout leaf).
    #[test]
    fn planner_evaluation_records_zero_legality_clones() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(PlayerId(0), &config, &policies);

        // Pre-warm the candidate cache and the generation-side recorders
        // (filter.rs:137, mana_abilities.rs:1256, costs.rs:1080) so the measured
        // windows below capture only legality-probe clones, not one-time
        // generation costs. build_decision_context is cached thereafter.
        let ctx = services.build_decision_context(&state);

        // Reach-guard: the fixture's PlayLand candidate drives validate_candidates
        // into its clone arm, proving the probe path is live for this input.
        perf_counters::reset();
        let _validated = services.validate_candidates(&state, ctx.candidates.clone());
        assert!(
            perf_counters::snapshot().state_clone_for_legality >= 1,
            "reach-guard: validate_candidates must clone at least once for the PlayLand candidate"
        );

        // Measure: planner_evaluation must not probe legality.
        perf_counters::reset();
        let _eval = services.planner_evaluation(&state);
        assert_eq!(
            perf_counters::snapshot().state_clone_for_legality,
            0,
            "planner_evaluation must not clone-and-apply per candidate for legality"
        );
    }

    // T2: sample_backfilled_continuations sorts by prior (desc), skips illegal
    // candidates WITHOUT consuming a sample slot, and pairs each survivor with
    // its own prior.
    #[test]
    fn sample_backfilled_continuations_backfills_past_illegal_high_prior() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let services = PlannerServices::new_default(PlayerId(0), &config, &policies);

        let illegal = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: ObjectId(99999),
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Utility),
        };
        let legal = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
        };

        // Reach-guards: prove the fixtures take the intended apply branch.
        assert!(
            services.apply_candidate(&state, &illegal).is_none(),
            "reach-guard: the illegal candidate must fail apply (→ None)"
        );
        assert!(
            services.apply_candidate(&state, &legal).is_some(),
            "reach-guard: the legal candidate must apply cleanly (→ Some)"
        );

        let illegal_prior = PolicyPrior {
            candidate: illegal.clone(),
            prior: 0.9,
        };
        let legal_prior = PolicyPrior {
            candidate: legal.clone(),
            prior: 0.1,
        };

        // sample_count=1: the high-prior illegal candidate is backfilled past, and
        // the single surviving slot carries the legal candidate. A take-then-filter
        // implementation would take the illegal top, drop it, and return [].
        let sampled = services.sample_backfilled_continuations(
            &state,
            vec![illegal_prior.clone(), legal_prior.clone()],
            1,
        );
        assert_eq!(
            sampled.len(),
            1,
            "backfill must fill the single slot with the legal candidate"
        );
        assert_eq!(
            sampled[0].0, 0.1,
            "the surviving continuation must carry the legal candidate's prior"
        );

        // Sibling: two legal candidates below the illegal top → both survive at count=2.
        let legal_a = PolicyPrior {
            candidate: legal.clone(),
            prior: 0.2,
        };
        let legal_b = PolicyPrior {
            candidate: legal.clone(),
            prior: 0.15,
        };
        let two = services.sample_backfilled_continuations(
            &state,
            vec![illegal_prior.clone(), legal_a, legal_b],
            2,
        );
        assert_eq!(two.len(), 2, "both legal candidates survive backfill");
        assert!(
            two.iter().all(|(p, _)| *p <= 0.2),
            "only the legal priors (<= 0.2) survive; the illegal 0.9 top is dropped"
        );

        // Sibling: empty priors → empty result.
        assert!(services
            .sample_backfilled_continuations(&state, Vec::new(), 1)
            .is_empty());

        // Sibling: all-illegal → empty result (nothing to backfill from).
        assert!(
            services
                .sample_backfilled_continuations(
                    &state,
                    vec![illegal_prior.clone(), illegal_prior.clone()],
                    2,
                )
                .is_empty(),
            "all-illegal priors yield no continuations"
        );
    }

    // T3′: rollout_estimate at its production entry point must not probe legality.
    // PINNED to a land-only empty-stack fixture: every sampled continuation leaves
    // an empty stack, so the depth-0 leaf never enters quiesce → build_decision_context
    // (mod.rs:584), which would otherwise fire the generation recorders.
    #[test]
    fn rollout_estimate_probe_free_at_production_entry() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(PlayerId(0), &config, &policies);

        // Pre-warm the generation recorders (cached thereafter).
        let _ = services.build_decision_context(&state);

        // Reach-guard A: the production leaf eval yields priors.
        let eval = services.planner_evaluation(&state);
        assert!(
            !eval.priors.is_empty(),
            "reach-guard A: planner_evaluation must produce priors"
        );

        // Reach-guard B: sampled continuations exist AND all leave an empty stack.
        let sample_count = services.config.search.rollout_samples.max(1) as usize;
        let continuations =
            services.sample_backfilled_continuations(&state, eval.priors.clone(), sample_count);
        assert!(
            !continuations.is_empty(),
            "reach-guard B: at least one legal continuation must be sampled"
        );
        assert!(
            continuations.iter().all(|(_, sim)| sim.stack.is_empty()),
            "T3′ requires an empty-stack fixture so the depth-0 leaf never enters \
             quiesce → build_decision_context (mod.rs:584)"
        );

        // Reach-guard C: deadline unexpired — closes the mod.rs early-return.
        assert!(
            !services.deadline.expired(),
            "reach-guard C: the deadline must be live so rollout descends"
        );

        // Measure: the production rollout entry must not probe legality.
        perf_counters::reset();
        let v = services.rollout_estimate(&state, 1);
        assert!(v.is_finite(), "rollout must return a finite estimate");
        assert_eq!(
            perf_counters::snapshot().state_clone_for_legality,
            0,
            "rollout_estimate must not clone-and-apply per candidate for legality"
        );
    }

    /// V2: `sample_backfilled_continuations` must apply ZERO continuations once
    /// the deadline is blown (Edit 2), and `rollout_estimate` must still return a
    /// finite leaf estimate. Paired with a live-deadline positive that DOES sample,
    /// so the negative is non-vacuous. Reverting Edit 2 makes the negative fail.
    #[test]
    fn sampling_honors_expired_deadline() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        assert!(
            !config.execution_mode.is_measurement(),
            "config must be non-measurement so the injected deadline is honored"
        );
        let policies = PolicyRegistry::default();

        // Reach-guard (non-vacuity): with a LIVE deadline, sampling produces
        // continuations, so the expired-deadline empty result is meaningful.
        let mut live = PlannerServices::new_default(PlayerId(0), &config, &policies);
        let eval = live.planner_evaluation(&state);
        assert!(
            !eval.priors.is_empty(),
            "reach-guard: planner_evaluation must produce priors"
        );
        let sample_count = live.config.search.rollout_samples.max(1) as usize;
        let live_conts =
            live.sample_backfilled_continuations(&state, eval.priors.clone(), sample_count);
        assert!(
            !live_conts.is_empty(),
            "non-vacuity: a live deadline must sample at least one continuation"
        );

        // Negative (the fix): an expired deadline applies zero continuations.
        let mut expired = PlannerServices::with_deadline(
            PlayerId(0),
            &config,
            &policies,
            crate::context::AiContext::empty(&config.weights),
            Some(engine::util::Deadline::after(0)),
        );
        assert!(
            expired.deadline.expired(),
            "witness: the injected deadline must be expired so the guard fires"
        );
        let empty =
            expired.sample_backfilled_continuations(&state, eval.priors.clone(), sample_count);
        assert!(
            empty.is_empty(),
            "revert-failing (Edit 2): sample_backfilled_continuations must apply \
             zero continuations once the deadline is blown, got {}",
            empty.len()
        );

        // rollout_estimate short-circuits to the leaf eval under an expired
        // deadline (top-of-function guard subsumes Edit 3 under static expiry).
        let v = expired.rollout_estimate(&state, 3);
        assert!(
            v.is_finite(),
            "rollout_estimate must return a finite leaf estimate under an expired deadline"
        );
    }

    // ===== Transposition-table / iterative-deepening tests (pipeline 5) =====

    fn add_bf_creature(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.entered_battlefield_turn = Some(1);
        obj.summoning_sick = false;
        id
    }

    /// A land whose only ability is `{T}: Add {G}` — a mana ability (no target,
    /// no loyalty), so it resolves without the stack and the player retains
    /// priority. Tapping two of these commutes: the resulting board is identical
    /// regardless of order, giving a genuine transposition.
    fn add_mana_land(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.entered_battlefield_turn = Some(1);
        obj.summoning_sick = false;
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        let abilities = std::sync::Arc::make_mut(&mut obj.abilities);
        abilities.push(ability);
        id
    }

    fn hard_services<'a>(
        config: &'a AiConfig,
        policies: &'a PolicyRegistry,
    ) -> PlannerServices<'a> {
        PlannerServices::new_default(PlayerId(0), config, policies)
    }

    // Row 1: a unit-keyword-only difference that `quick_state_hash` aliases must
    // be distinguished by `search_position_hash`'s discriminant fold.
    #[test]
    fn search_position_hash_distinguishes_granted_keyword() {
        let mut a = make_state();
        let id = add_bf_creature(&mut a, PlayerId(0));
        let mut b = a.clone();
        b.objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::Flying);

        // Reach-guard: the base hash aliases these two (keywords are not hashed
        // by quick_state_hash), so the `_ne!` below is non-vacuous.
        assert_eq!(quick_state_hash(&a), quick_state_hash(&b));
        // Revert-failing: dropping the keyword fold collapses these to equal.
        assert_ne!(search_position_hash(&a), search_position_hash(&b));
    }

    // Row 1e: a parameterized keyword payload (Ward cost) — the hybrid serde
    // branch. A discriminant-only fold would alias Ward(2) with Ward(100).
    #[test]
    fn search_position_hash_distinguishes_ward_cost() {
        let mut a = make_state();
        let id = add_bf_creature(&mut a, PlayerId(0));
        a.objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::Ward(WardCost::PayLife(2)));
        let mut b = a.clone();
        b.objects.get_mut(&id).unwrap().keywords[0] = Keyword::Ward(WardCost::PayLife(100));

        // Reach-guard 1: base hash aliases (keyword payloads not hashed there).
        assert_eq!(quick_state_hash(&a), quick_state_hash(&b));
        // Reach-guard 2: discriminants are equal, so a discriminant-only fold
        // WOULD collide — proving the `_ne!` exercises the serde payload branch.
        assert_eq!(
            a.objects.get(&id).unwrap().keywords[0].kind(),
            b.objects.get(&id).unwrap().keywords[0].kind()
        );
        // Revert-failing for the hybrid: replacing the fold body with a pure
        // `kw.kind().hash()` collapses Ward(2)/Ward(100) to equal.
        assert_ne!(search_position_hash(&a), search_position_hash(&b));
    }

    // Row 1b: stack entry targets/modes (post-apply divergence) — quick_state_hash
    // hashes only source_id + controller of each stack entry.
    #[test]
    fn search_position_hash_distinguishes_stack_targets() {
        let src = ObjectId(500);
        let ctrl = PlayerId(0);
        let mut a = make_state();
        a.stack.push_back(StackEntry {
            id: src,
            source_id: src,
            controller: ctrl,
            kind: StackEntryKind::ActivatedAbility {
                source_id: src,
                ability: ResolvedAbility::new(Effect::NoOp, vec![], src, ctrl),
            },
        });
        let mut b = a.clone();
        if let StackEntryKind::ActivatedAbility { ability, .. } = &mut b.stack[0].kind {
            ability.targets = vec![TargetRef::Object(ObjectId(999))];
        }

        // Reach-guard: only source_id + controller are hashed by quick_state_hash.
        assert_eq!(quick_state_hash(&a), quick_state_hash(&b));
        assert_ne!(search_position_hash(&a), search_position_hash(&b));
    }

    // Row 1c: commander_damage amounts.
    #[test]
    fn search_position_hash_distinguishes_commander_damage() {
        let mut a = make_state();
        a.commander_damage.push(CommanderDamageEntry {
            player: PlayerId(0),
            commander: ObjectId(7),
            damage: 3,
        });
        let mut b = a.clone();
        b.commander_damage[0].damage = 9;

        assert_eq!(quick_state_hash(&a), quick_state_hash(&b));
        assert_ne!(search_position_hash(&a), search_position_hash(&b));
    }

    // Row 1d: exile contents (equal length, different ObjectIds).
    #[test]
    fn search_position_hash_distinguishes_exile_contents() {
        let mut a = make_state();
        a.exile.push_back(ObjectId(11));
        a.exile.push_back(ObjectId(12));
        let mut b = make_state();
        b.exile.push_back(ObjectId(21));
        b.exile.push_back(ObjectId(22));

        // Reach-guard: quick_state_hash hashes exile.len() only -> equal length aliases.
        assert_eq!(quick_state_hash(&a), quick_state_hash(&b));
        assert_ne!(search_position_hash(&a), search_position_hash(&b));
    }

    // Row 2: library ordering (top-of-library cast / draw-horizon lines).
    #[test]
    fn search_position_hash_distinguishes_library_order() {
        let mut a = make_state();
        {
            let lib = &mut a.players[0].library;
            lib.clear();
            lib.push_back(ObjectId(1));
            lib.push_back(ObjectId(2));
            lib.push_back(ObjectId(3));
        }
        let mut b = a.clone();
        {
            let lib = &mut b.players[0].library;
            lib.clear();
            lib.push_back(ObjectId(2));
            lib.push_back(ObjectId(1));
            lib.push_back(ObjectId(3));
        }

        // Reach-guard: quick_state_hash hashes library.len() only.
        assert_eq!(quick_state_hash(&a), quick_state_hash(&b));
        assert_ne!(search_position_hash(&a), search_position_hash(&b));
    }

    // Row 2b: per-object summoning sickness.
    #[test]
    fn search_position_hash_distinguishes_summoning_sick() {
        let mut a = make_state();
        let id = add_bf_creature(&mut a, PlayerId(0));
        a.objects.get_mut(&id).unwrap().summoning_sick = false;
        let mut b = a.clone();
        b.objects.get_mut(&id).unwrap().summoning_sick = true;

        assert_eq!(quick_state_hash(&a), quick_state_hash(&b));
        assert_ne!(search_position_hash(&a), search_position_hash(&b));
    }

    // Row 8b: the keyword fold path is exercised on a keyword-DENSE board (both
    // the discriminant fast path and the serde payload branch) and stays cheap.
    #[test]
    fn search_position_hash_keyword_dense_stays_fast() {
        let mut state = make_state();
        for i in 0..400 {
            let owner = if i % 2 == 0 { PlayerId(0) } else { PlayerId(1) };
            let id = add_bf_creature(&mut state, owner);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.keywords.push(Keyword::Flying); // discriminant fast path
            if i % 50 == 0 {
                obj.keywords.push(Keyword::Ward(WardCost::PayLife(2))); // serde branch
            }
        }
        let start = std::time::Instant::now();
        for _ in 0..50 {
            let _ = search_position_hash(&state);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 500,
            "keyword-dense position hash too slow ({elapsed:?}) — the fold regressed"
        );
    }

    // Row 4: probe soundness under bounds + depth — exhaustive over all three
    // `TtBound` arms and the depth-insufficient arm.
    #[test]
    fn tt_probe_respects_depth_and_bound() {
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);

        // Too shallow: entry depth 1, caller needs depth 2 -> None.
        services.transposition_table.insert(
            1,
            TtEntry {
                depth: 1,
                value: 5.0,
                bound: TtBound::Exact,
            },
        );
        assert_eq!(
            services.tt_probe(1, 2, f64::NEG_INFINITY, f64::INFINITY),
            None
        );
        // Exact always hits at sufficient depth, regardless of window.
        assert_eq!(services.tt_probe(1, 1, 0.0, 10.0), Some(5.0));

        // LowerBound (fail-high) hits iff value >= beta.
        services.transposition_table.insert(
            2,
            TtEntry {
                depth: 2,
                value: 8.0,
                bound: TtBound::LowerBound,
            },
        );
        assert_eq!(services.tt_probe(2, 2, 0.0, 8.0), Some(8.0));
        assert_eq!(services.tt_probe(2, 2, 0.0, 9.0), None);

        // UpperBound (fail-low) hits iff value <= alpha.
        services.transposition_table.insert(
            3,
            TtEntry {
                depth: 2,
                value: 2.0,
                bound: TtBound::UpperBound,
            },
        );
        assert_eq!(services.tt_probe(3, 2, 2.0, 10.0), Some(2.0));
        assert_eq!(services.tt_probe(3, 2, 1.0, 10.0), None);
    }

    // Row 4b: store-time bound classification is correct for all three arms.
    #[test]
    fn tt_store_classifies_bound() {
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);

        // Window (alpha_orig = 0.0, beta_orig = 10.0).
        services.tt_store(10, 2, -5.0, 0.0, 10.0); // value <= alpha_orig -> fail-low
        services.tt_store(20, 2, 15.0, 0.0, 10.0); // value >= beta_orig  -> fail-high
        services.tt_store(30, 2, 5.0, 0.0, 10.0); // in-window          -> Exact

        assert_eq!(
            services.transposition_table.get(&10).unwrap().bound,
            TtBound::UpperBound
        );
        assert_eq!(
            services.transposition_table.get(&20).unwrap().bound,
            TtBound::LowerBound
        );
        assert_eq!(
            services.transposition_table.get(&30).unwrap().bound,
            TtBound::Exact
        );
    }

    // Row 4b (min-node beta_orig sub-case): a fully-searched MINIMIZING root must
    // store `Exact`, not `LowerBound`. At a min node `beta` is lowered to `best`
    // during search; if `search_value` passed the MUTATED beta to `tt_store`, the
    // exact value would satisfy `value >= beta` and be mislabeled `LowerBound`.
    // The root's incoming beta_orig is +inf, so the correct classification is
    // Exact — this assertion flips to LowerBound if the beta_orig fix is reverted.
    #[test]
    fn search_value_min_node_stores_exact_against_beta_orig() {
        let mut state = make_state();
        // Opponent (PlayerId 1) has priority -> the root is a MINIMIZING node for
        // ai_player 0. A tappable mana land gives the node real branching.
        add_mana_land(&mut state, PlayerId(1));
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let mut budget = SearchBudget::new(100_000);
        let planner = BeamContinuationPlanner {
            depth: 2,
            rollout_depth: 1,
        };

        let value = planner.search_value(
            &state,
            2,
            0,
            f64::NEG_INFINITY,
            f64::INFINITY,
            &mut services,
            &mut budget,
        );
        assert!(value.is_finite());

        let key = transposition_key(&state);
        let entry = services
            .transposition_table
            .get(&key)
            .expect("the fully-searched min root must be stored in the TT");
        assert_eq!(
            entry.bound,
            TtBound::Exact,
            "min-node exact value must classify Exact against beta_orig(+inf), \
             not LowerBound against the mutated beta"
        );
    }

    // Row 3: the TT probe is wired into `search_value` and a sufficient-depth hit
    // skips the whole expansion (the memoization win). The probe runs at
    // `search_value` entry — BEFORE `build_decision_context` and candidate
    // expansion — so a pre-seeded entry for the node's own position makes the real
    // search short-circuit to the stored value. (This is the robust way to drive
    // the probe: genuine same-decision transpositions do not occur through the
    // production candidate pipeline — the only commuting priority actions are mana
    // abilities, which the AI candidate generator withholds at priority, and every
    // other action leaves an order-dependent stack that `search_position_hash`
    // folds. Row 4b drives the *store* side through a real search.)
    #[test]
    fn transposition_hit_skips_research() {
        let state = make_state();
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let mut budget = SearchBudget::new(100_000);

        // Seed the node's own position at ample depth with an Exact bound.
        let key = transposition_key(&state);
        let stored = 42.0;
        services.transposition_table.insert(
            key,
            TtEntry {
                depth: 5,
                value: stored,
                bound: TtBound::Exact,
            },
        );

        let planner = BeamContinuationPlanner {
            depth: 2,
            rollout_depth: 1,
        };
        let value = planner.search_value(
            &state,
            2,
            0,
            f64::NEG_INFINITY,
            f64::INFINITY,
            &mut services,
            &mut budget,
        );

        // Revert-failing: without the probe the search expands and returns a
        // computed eval (not `stored`), and `tt_hits` never increments.
        assert_eq!(
            value, stored,
            "a sufficient-depth Exact hit must short-circuit expansion to the stored value"
        );
        assert!(
            services.tt_hits > 0,
            "the wired probe must count the cutoff"
        );
        // Only one node was entered (the probe cut it off before recursing).
        assert!(
            budget.nodes_evaluated <= 1,
            "the hit must skip the subtree, not search it"
        );
    }

    // Row 3 negative sibling: with no matching entry, a real search stores but never
    // probes a hit, so `tt_hits` stays 0 — proving the counter tracks real cutoffs,
    // not spurious increments.
    #[test]
    fn search_without_matching_entry_yields_zero_tt_hits() {
        let state = make_state();
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let mut budget = SearchBudget::new(100_000);
        let planner = BeamContinuationPlanner {
            depth: 1,
            rollout_depth: 1,
        };
        let _ = planner.search_value(
            &state,
            1,
            0,
            f64::NEG_INFINITY,
            f64::INFINITY,
            &mut services,
            &mut budget,
        );
        assert_eq!(
            services.tt_hits, 0,
            "no seeded/matching entry -> the probe never serves a cutoff"
        );
    }

    // Row 6 (planner part): measurement mode holds a `none()` deadline that never
    // expires, so the ID ceiling is pinned by config alone.
    #[test]
    fn measurement_mode_deadline_is_none() {
        let config = create_config(AiDifficulty::Hard, Platform::Native).into_measurement(42);
        let policies = PolicyRegistry::default();
        let services = hard_services(&config, &policies);
        assert!(
            !services.deadline.expired(),
            "measurement deadline is none() -> expired() is always false"
        );
    }

    // ---- U2: move ordering (killers) + witness counters ----

    fn ranked_candidate(action: GameAction, score: f64) -> RankedCandidate {
        RankedCandidate {
            candidate: CandidateAction {
                action,
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
            },
            score,
        }
    }

    // V1: a beta cutoff records the cutting move as the ply's primary killer.
    #[test]
    fn beta_cutoff_records_cutting_action_as_killer() {
        let mut state = make_state();
        add_mana_land(&mut state, PlayerId(0)); // >= 2 candidates (tap + pass)
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);

        // Reconstruct the exact interior ordering the search visits (killers are
        // empty, so `order_killers_first` is a no-op). Under a collapsed window
        // the cut fires after the FIRST applicable candidate in rank order.
        let ctx = services.build_decision_context(&state);
        let scoring_player = state
            .waiting_for
            .acting_player()
            .unwrap_or(services.ai_player);
        let expected_ranked = rank_candidates(
            ctx.candidates.clone(),
            |c| services.tactical_score(&state, &ctx, c, scoring_player, SearchDepth::Lookahead),
            config.search.max_branching as usize,
        );
        let cutting = expected_ranked
            .iter()
            .find(|r| apply_candidate(&state, &r.candidate).is_some())
            .map(|r| r.candidate.action.clone())
            .expect("fixture must have an applicable candidate");

        let planner = BeamContinuationPlanner {
            depth: 1,
            rollout_depth: 1,
        };
        let mut budget = SearchBudget::new(100_000);
        // Collapsed window (alpha == beta) forces alpha >= beta after the first
        // applied candidate — a deterministic cutoff.
        let _ = planner.search_value(&state, 1, 0, 0.0, 0.0, &mut services, &mut budget);

        // Reach-guard: the cutoff branch actually executed (non-vacuous).
        assert!(
            services.beta_cutoffs > 0,
            "reach-guard: a beta cutoff must have fired"
        );
        // Revert-failing: removing `record_killer` at the break leaves this None.
        assert_eq!(
            services.killers[0][0].as_ref(),
            Some(&cutting),
            "the beta-cutting action must be recorded as the ply-0 primary killer"
        );
    }

    // V2: killer ordering rotates matching candidates to the front of the beam
    // WITHOUT mutating any score, and only counts when a killer is present.
    #[test]
    fn killer_ordering_reorders_within_beam_without_touching_scores() {
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();

        // Beam of 3, already best-first by score; the killer is ranked LAST.
        let control = vec![
            ranked_candidate(GameAction::PassPriority, 3.0),
            ranked_candidate(GameAction::CancelCast, 2.0),
            ranked_candidate(GameAction::DeclineShortcut, 1.0),
        ];
        // Full (action -> score-bits) map — every key AND value, not set equality.
        let score_map = |b: &[RankedCandidate]| -> std::collections::BTreeMap<String, u64> {
            b.iter()
                .map(|r| (format!("{:?}", r.candidate.action), r.score.to_bits()))
                .collect()
        };

        // Primary: last-ranked killer rotates to the front, scores untouched.
        let mut services = hard_services(&config, &policies);
        services.record_killer(0, &GameAction::DeclineShortcut);
        let mut beam = control.clone();
        services.order_killers_first(0, &mut beam);
        assert_eq!(
            beam[0].candidate.action,
            GameAction::DeclineShortcut,
            "the seeded killer is rotated to the front"
        );
        assert_eq!(
            score_map(&beam),
            score_map(&control),
            "reorder is score-preserving (every action keeps its exact score)"
        );
        assert_eq!(
            services.killer_orderings, 1,
            "a present killer counts as exactly one ordering"
        );

        // Sibling: with two killers, slot 0 must precede slot 1.
        let mut services2 = hard_services(&config, &policies);
        services2.record_killer(0, &GameAction::DeclineShortcut); // -> slot 0
        services2.record_killer(0, &GameAction::CancelCast); // -> slot 0, prev -> slot 1
        let mut beam2 = control.clone();
        services2.order_killers_first(0, &mut beam2);
        assert_eq!(
            beam2[0].candidate.action,
            GameAction::CancelCast,
            "slot-0 killer orders first"
        );
        assert_eq!(
            beam2[1].candidate.action,
            GameAction::DeclineShortcut,
            "slot-1 killer orders second"
        );

        // Hostile: a killer absent from the candidate set leaves order untouched
        // (the `==` match miss path) and never counts an ordering.
        let mut services3 = hard_services(&config, &policies);
        services3.record_killer(
            0,
            &GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        );
        let mut beam3 = control.clone();
        services3.order_killers_first(0, &mut beam3);
        let actions = |b: &[RankedCandidate]| {
            b.iter()
                .map(|r| r.candidate.action.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            actions(&beam3),
            actions(&control),
            "an absent killer leaves the beam order identical to control"
        );
        assert_eq!(
            services3.killer_orderings, 0,
            "no killer present in the beam => no ordering counted"
        );
    }

    // V7a: freshly constructed services carry no killers and zeroed witnesses.
    // Combined with the per-sample construction site in `score_candidates_core`,
    // this pins that every determinized sample starts clean (no cross-world leak).
    #[test]
    fn fresh_services_has_clean_killers_and_witnesses() {
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let policies = PolicyRegistry::default();
        let services = hard_services(&config, &policies);
        assert!(
            services
                .killers
                .iter()
                .all(|ply| ply.iter().all(Option::is_none)),
            "fresh services must have no killers"
        );
        assert_eq!(services.beta_cutoffs, 0);
        assert_eq!(services.killer_orderings, 0);
        assert!(services.rung_stats.is_empty());
    }

    // V8: the cutoff counter tracks real cutoffs, not spurious increments.
    #[test]
    fn beta_cutoff_counter_is_honest() {
        let mut state = make_state();
        add_mana_land(&mut state, PlayerId(0)); // >= 2 candidates
        let policies = PolicyRegistry::default();

        // Positive: collapsed window at a maximizing node forces a cutoff.
        let config = create_config(AiDifficulty::Hard, Platform::Native);
        let mut services = hard_services(&config, &policies);
        let mut budget = SearchBudget::new(100_000);
        let planner = BeamContinuationPlanner {
            depth: 1,
            rollout_depth: 1,
        };
        let _ = planner.search_value(&state, 1, 0, 0.0, 0.0, &mut services, &mut budget);
        assert!(
            services.beta_cutoffs > 0,
            "a real cutoff must increment the counter"
        );

        // Negative: `max_branching == 1` under a full (-inf, +inf) window can
        // never cut — a single sibling cannot push alpha past beta. Counter == 0.
        let mut narrow_config = create_config(AiDifficulty::Hard, Platform::Native);
        narrow_config.search.max_branching = 1;
        let mut services_n = hard_services(&narrow_config, &policies);
        let mut budget_n = SearchBudget::new(100_000);
        let planner_n = BeamContinuationPlanner {
            depth: 2,
            rollout_depth: 1,
        };
        let _ = planner_n.search_value(
            &state,
            2,
            0,
            f64::NEG_INFINITY,
            f64::INFINITY,
            &mut services_n,
            &mut budget_n,
        );
        assert_eq!(
            services_n.beta_cutoffs, 0,
            "a single-width full-window search cannot produce a cutoff"
        );
    }
}
