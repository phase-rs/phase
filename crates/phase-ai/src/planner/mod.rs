use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use engine::ai_support::{
    build_decision_context, classify_payment_continuation, witness_payment_continuations,
    AiDecisionContext, CandidateAction, PaymentContinuationBatchStatus, PaymentContinuationState,
    TacticalClass,
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
    pub(crate) payment_successor: Option<GameState>,
    /// A reducer-certified downstream evaluation that reaches beyond the
    /// ordinary one-action root simulation. It is an alternative continuation
    /// witness, not a tactical policy score.
    pub(crate) continuation_witness: Option<f64>,
}

impl RankedCandidate {
    pub fn new(candidate: CandidateAction, score: f64) -> Self {
        Self {
            candidate,
            score,
            payment_successor: None,
            continuation_witness: None,
        }
    }

    pub(crate) fn with_payment_successor(
        candidate: CandidateAction,
        score: f64,
        state: GameState,
    ) -> Self {
        Self {
            candidate,
            score,
            payment_successor: Some(state),
            continuation_witness: None,
        }
    }

    pub(crate) fn with_continuation_witness(mut self, value: f64) -> Self {
        self.continuation_witness = value.is_finite().then_some(value);
        self
    }

    /// Preserve any proven continuation through the existing tactical-score
    /// beam order. The terminal witness is deliberately not scaled here: beam
    /// truncation must not discard a reducer-proven route before search can
    /// compare it with an ordinary continuation.
    pub(crate) fn beam_priority(&self) -> f64 {
        self.continuation_witness
            .filter(|witness| witness.is_finite())
            .map_or(self.score, |witness| self.score.max(witness))
    }

    pub(crate) fn root_score(&self, tactical_weight: f64) -> f64 {
        self.continuation_witness
            .filter(|witness| witness.is_finite())
            .map_or(self.score * tactical_weight, |witness| {
                (self.score * tactical_weight).max(witness)
            })
    }
}

/// A raw engine candidate plus the first reducer successor already witnessed
/// for an affiliated payment root. This stays private to planning: the engine
/// owns both the carrier classification and the finalization proof.
#[derive(Debug, Clone)]
pub(crate) struct PreparedCandidate {
    /// Position in the engine-issued action list. This is the provenance key
    /// for payment certificates; equivalent GameActions are not interchangeable.
    pub source_index: usize,
    pub candidate: CandidateAction,
    pub payment_successor: Option<GameState>,
}

/// Fail closed for affiliated payment states before ranking or width limits.
///
/// The accepted successor is retained so a planning edge does not apply its
/// first reducer action once for the witness and again for search.
pub(crate) fn prepare_payment_candidates(
    state: &GameState,
    candidates: impl IntoIterator<Item = CandidateAction>,
) -> Vec<PreparedCandidate> {
    let candidates: Vec<_> = candidates.into_iter().collect();
    match classify_payment_continuation(state) {
        PaymentContinuationState::NotAffiliated => candidates
            .into_iter()
            .enumerate()
            .map(|(source_index, candidate)| PreparedCandidate {
                source_index,
                candidate,
                payment_successor: None,
            })
            .collect(),
        PaymentContinuationState::UnsupportedAffiliated(_) => Vec::new(),
        PaymentContinuationState::Affiliated(_) => {
            let actions: Vec<_> = candidates
                .iter()
                .map(|candidate| candidate.action.clone())
                .collect();
            let batch = witness_payment_continuations(state, &actions);
            match batch.status {
                PaymentContinuationBatchStatus::Complete => candidates
                    .into_iter()
                    .enumerate()
                    .zip(batch.successors)
                    .filter_map(|((source_index, candidate), accepted)| {
                        accepted.map(|accepted| PreparedCandidate {
                            source_index,
                            candidate,
                            payment_successor: Some(accepted.state),
                        })
                    })
                    .collect(),
                PaymentContinuationBatchStatus::Indeterminate(_) => {
                    if batch.successors.iter().any(Option::is_some) {
                        candidates
                            .into_iter()
                            .enumerate()
                            .zip(batch.successors)
                            .filter_map(|((source_index, candidate), accepted)| {
                                accepted.map(|accepted| PreparedCandidate {
                                    source_index,
                                    candidate,
                                    payment_successor: Some(accepted.state),
                                })
                            })
                            .collect()
                    } else {
                        candidates
                            .into_iter()
                            .enumerate()
                            .filter_map(|(source_index, candidate)| {
                                matches!(candidate.action, GameAction::CancelCast).then_some(
                                    PreparedCandidate {
                                        source_index,
                                        candidate,
                                        payment_successor: None,
                                    },
                                )
                            })
                            .collect()
                    }
                }
                PaymentContinuationBatchStatus::NotAffiliated
                | PaymentContinuationBatchStatus::UnsupportedAffiliated(_) => Vec::new(),
            }
        }
    }
}

pub(crate) fn rank_prepared_candidates<F>(
    candidates: impl IntoIterator<Item = PreparedCandidate>,
    mut scorer: F,
    limit: usize,
) -> Vec<RankedCandidate>
where
    F: FnMut(&CandidateAction) -> f64,
{
    let mut ranked: Vec<RankedCandidate> = candidates
        .into_iter()
        .map(|prepared| {
            let score = scorer(&prepared.candidate);
            match prepared.payment_successor {
                Some(state) => {
                    RankedCandidate::with_payment_successor(prepared.candidate, score, state)
                }
                None => RankedCandidate::new(prepared.candidate, score),
            }
        })
        .collect();
    ranked.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.candidate.action.cmp_stable(&b.candidate.action))
    });
    ranked.truncate(limit);
    ranked
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
    pub(crate) payment_successor: Option<GameState>,
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
        for unit in player.mana_pool.units() {
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

/// Ceiling on the child `GameState` snapshots one decision's successor memo
/// may retain. This is the memo's memory budget: every successful stored edge
/// keeps one full snapshot alive until the decision ends.
///
/// Retention is first-come, which keeps the shallow edges every rung
/// re-walks. Once the ceiling is reached an edge is simulated on every visit
/// and its child is untracked, which yields the same states search produced
/// before the memo existed (a quiescence chain leaving the tracked region pays
/// one extra state clone), so the ceiling changes cost and never the result.
///
/// 16 is a measured choice, not a derived one. Workload: `ai-duel`, profiling
/// build, Hard, red against green, seed 7, `--batch 4`, compared with the
/// build that has no memo (average game 5.36 s, peak RSS 1.50 GB):
///
/// | ceiling | average game | peak RSS |
/// |---------|--------------|----------|
/// | 0       | -7%          | +0-1%    |
/// | 8       | -12%         | +1.5%    |
/// | 16      | -14%         | +3%      |
/// | 32      | -16%         | +6%      |
/// | 64      | -17%         | +13%     |
/// | 128     | -17%         | +16%     |
/// | 256     | -16%         | +31%     |
/// | 2048    | -15%         | +65%     |
///
/// Without a binding ceiling one decision retained up to 528 snapshots and 462
/// decision contexts, and that retention is the whole RSS increase. 41% of
/// the successor hits land on the first 16 snapshots a decision admits, 64% on
/// the first 32 and 85% on the first 64. At ceiling 0 no position below a
/// root is tracked. The same runs produced byte-identical game output at
/// every ceiling. Run-to-run spread on the average game is about two points:
/// a second set of three alternating runs at 16 measured -12%.
const SUCCESSOR_STATE_CAPACITY: usize = 16;

/// Defensive ceiling on the successor edges one decision may memoize, failed
/// applies included. A failed edge retains no snapshot, only its `GameAction`,
/// so it is not charged to `SUCCESSOR_STATE_CAPACITY`; this separate ceiling
/// keeps the edge lists bounded even for a search that keeps failing. The
/// workload measured for `SUCCESSOR_STATE_CAPACITY` stored no failed apply at
/// all, so 256 is headroom rather than a tuned value.
const SUCCESSOR_EDGE_CAPACITY: usize = 256;

/// Identity of one position inside a single decision's search.
///
/// Issued only by [`PlannerServices`]. An id denotes exactly one `GameState`
/// value: the root state handed to [`PlannerServices::new_search_root`], or
/// the state reached from a tracked parent by cloning it and applying one fixed
/// action with `apply_as_current_for_simulation`. Anything keyed by an id is
/// therefore exact, unlike the lossy hash-keyed `candidate_cache` and
/// `eval_cache`. Ids never outlive the `PlannerServices` that issued them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SearchNodeId(u32);

/// The memoized outcome of applying one action at a tracked position: the
/// child's identity and state, or `None` when the apply failed.
type TrackedSuccessor = Option<(SearchNodeId, Arc<GameState>)>;

/// Local witnesses for the per-decision search memos. Not engine perf counters
/// (`perf_counters.rs` is out of scope); they exist so tests can prove that a
/// repeated simulation or context build was actually skipped.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SearchMemoWitness {
    /// Successor requests answered from the memo without simulating.
    pub successor_hits: u32,
    /// Successor requests that ran their simulation closure.
    pub successor_simulations: u32,
    /// Decision-context requests answered from the per-position memo.
    pub context_hits: u32,
    /// Decision-context requests that ran the engine's context builder.
    pub context_builds: u32,
    /// Highest number of successor edges held by the memo at once, failed
    /// applies included. Not a measure of memory: see `peak_retained_states`.
    pub peak_successor_edges: usize,
    /// Highest number of child `GameState` snapshots held by the successor
    /// memo at once (one per stored successful edge). This is the quantity
    /// the memo's capacity bounds.
    pub peak_retained_states: usize,
    /// Highest number of decision contexts held by the per-position memo at
    /// once, roots included.
    pub peak_node_contexts: usize,
}

/// Where quiescence currently stands. It starts by borrowing the caller's
/// state, moves onto shared memoized successors while the position is tracked,
/// and takes ownership only when a step has to be applied in place.
enum QuiesceCursor<'s> {
    /// The caller's state, untouched so far.
    Borrowed(&'s GameState),
    /// A memoized successor shared with the successor memo.
    Shared(Arc<GameState>),
    /// A private copy that steps are applied to in place, boxed so the
    /// cursor itself stays pointer-sized.
    Owned(Box<GameState>),
}

impl QuiesceCursor<'_> {
    /// The state the cursor currently points at.
    fn state(&self) -> &GameState {
        match self {
            Self::Borrowed(state) => state,
            Self::Shared(state) => state,
            Self::Owned(state) => state,
        }
    }

    /// Take a private copy of the current state (cloning unless the cursor
    /// already owns one) and hand it out for an in-place apply.
    fn make_owned(&mut self) -> &mut GameState {
        if !matches!(self, Self::Owned(_)) {
            *self = Self::Owned(Box::new(self.state().clone()));
        }
        match self {
            Self::Owned(state) => state,
            Self::Borrowed(_) | Self::Shared(_) => {
                unreachable!("the cursor was made owned just above")
            }
        }
    }

    /// Consume the cursor into an owned state, cloning only when it does not
    /// already own one.
    #[cfg(test)]
    fn into_owned(self) -> GameState {
        match self {
            Self::Borrowed(state) => state.clone(),
            Self::Shared(state) => GameState::clone(&state),
            Self::Owned(state) => *state,
        }
    }
}

/// Outcome of one forced quiescence step.
enum QuiesceStep {
    /// The action applied; the payload is the identity of the new position when
    /// it is tracked.
    Advanced(Option<SearchNodeId>),
    /// The action failed to apply; quiescence stops here.
    Failed,
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
    /// Exact successor memo: for each tracked position, the actions already
    /// applied there and what each produced. `GameAction` is `PartialEq` but
    /// not `Hash`, so each position keeps a short `Vec` compared by `==` (the
    /// same reasoning as `killers`). The map is used for keyed lookup only and
    /// is never iterated. Scope is the `PlannerServices` lifetime — one
    /// decision — so nothing crosses determinized samples or decisions.
    successors: HashMap<SearchNodeId, Vec<(GameAction, TrackedSuccessor)>>,
    /// Number of edges currently held by `successors`, failed applies included.
    /// Never exceeds `successor_edge_capacity`.
    stored_successor_edges: usize,
    /// Number of child snapshots currently held by `successors`: its
    /// successful edges. Never exceeds `successor_state_capacity`.
    retained_successor_states: usize,
    /// Next unissued [`SearchNodeId`]. Ids only ever increase; one is issued
    /// per root and one per retained snapshot, so the count is bounded by
    /// `successor_state_capacity` plus the number of roots.
    next_search_node: u32,
    /// Ceiling on `retained_successor_states`, initialised from
    /// `SUCCESSOR_STATE_CAPACITY`. Once it is reached no further edge is
    /// stored, successful or not. A field so tests can lower it; `0` turns the
    /// successor memo off, which leaves every position below a root untracked.
    pub(crate) successor_state_capacity: usize,
    /// Ceiling on `stored_successor_edges`, initialised from
    /// `SUCCESSOR_EDGE_CAPACITY`. A field so tests can lower it.
    successor_edge_capacity: usize,
    /// Exact per-position decision contexts, keyed by tracked position. Keyed
    /// lookup only, never iterated. Holds at most
    /// `successor_state_capacity + 1` entries: a key is a tracked child (one
    /// per retained snapshot, so at most `successor_state_capacity` of them)
    /// or a root, and production search issues one root per decision.
    /// `exact_decision_context` enforces the same ceiling on insert, so the
    /// bound also holds for a caller that issues more roots.
    node_contexts: HashMap<SearchNodeId, Arc<AiDecisionContext>>,
    /// Witness counters for the memos above. Not engine perf counters; local
    /// evidence that repeated work was actually skipped, used by the memo
    /// regression tests.
    pub memo_witness: SearchMemoWitness,
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
            successors: HashMap::new(),
            stored_successor_edges: 0,
            retained_successor_states: 0,
            next_search_node: 0,
            successor_state_capacity: SUCCESSOR_STATE_CAPACITY,
            successor_edge_capacity: SUCCESSOR_EDGE_CAPACITY,
            node_contexts: HashMap::new(),
            memo_witness: SearchMemoWitness::default(),
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
        self.build_decision_context_at(None, state)
    }

    /// [`PlannerServices::build_decision_context`] for a position that may be
    /// tracked. The hash-keyed `candidate_cache` is consulted first and wins,
    /// so its insert and hit history is the same whether or not `node` is
    /// tracked; only on a miss is the context taken from
    /// [`PlannerServices::exact_decision_context`], which equals a fresh build.
    fn build_decision_context_at(
        &mut self,
        node: Option<SearchNodeId>,
        state: &GameState,
    ) -> Arc<AiDecisionContext> {
        // MUST use candidate_cache_key, NOT quick_state_hash: the latter omits
        // state.waiting_for, which is the dispatch key for candidate_actions.
        // Using the wrong hash collides states with identical boards but
        // different WaitingFor (e.g. Priority vs TargetSelection), returning
        // cached candidates from the wrong state.
        let key = candidate_cache_key(state);
        if let Some(hit) = self.candidate_cache.get(&key) {
            return Arc::clone(hit);
        }
        let ctx = self.exact_decision_context(node, state);
        self.candidate_cache.insert(key, Arc::clone(&ctx));
        ctx
    }

    /// The engine's decision context for exactly `state`. A tracked `node`
    /// answers from the per-position memo after its first build; an untracked
    /// one builds every time. The builder reads nothing but the state, so the
    /// memoized value equals a fresh build. Never reads or writes
    /// `candidate_cache`. `node`, when given, must identify `state`.
    ///
    /// The memo holds at most `successor_state_capacity + 1` contexts (every
    /// tracked child plus one root). A tracked position past that ceiling,
    /// which takes more than one root to reach, is built on every request like
    /// an untracked one.
    fn exact_decision_context(
        &mut self,
        node: Option<SearchNodeId>,
        state: &GameState,
    ) -> Arc<AiDecisionContext> {
        if let Some(node) = node {
            if let Some(hit) = self.node_contexts.get(&node) {
                self.memo_witness.context_hits = self.memo_witness.context_hits.saturating_add(1);
                return Arc::clone(hit);
            }
        }
        self.memo_witness.context_builds = self.memo_witness.context_builds.saturating_add(1);
        let ctx = Arc::new(build_decision_context(state));
        if let Some(node) = node {
            if self.node_contexts.len() <= self.successor_state_capacity {
                self.node_contexts.insert(node, Arc::clone(&ctx));
                self.memo_witness.peak_node_contexts = self
                    .memo_witness
                    .peak_node_contexts
                    .max(self.node_contexts.len());
            }
        }
        ctx
    }

    /// Start tracking a new search root and return its identity. The caller
    /// owns the association between the id and the root state: every later
    /// call that passes this id must pass that same state.
    pub(crate) fn new_search_root(&mut self) -> SearchNodeId {
        let id = SearchNodeId(self.next_search_node);
        self.next_search_node = self.next_search_node.wrapping_add(1);
        id
    }

    /// The successor of `parent` under `action`, simulating at most once per
    /// tracked `(parent, action)` pair.
    ///
    /// `simulate` must be "clone the parent's state and apply `action` with
    /// `apply_as_current_for_simulation`", returning `None` when the apply
    /// fails. A tracked parent must not be payment-affiliated, or `simulate`
    /// must return `None` for it, so an entry means the same thing whichever
    /// caller wrote it.
    ///
    /// - Untracked parent (`None`): always simulates; the child is untracked.
    /// - Tracked hit: returns the stored result (`None` for a stored failure)
    ///   without calling `simulate`.
    /// - Tracked miss: simulates and stores the result, failures included,
    ///   unless the memo already retains `successor_state_capacity` child
    ///   snapshots or holds `successor_edge_capacity` edges; then nothing is
    ///   stored and the child is untracked.
    ///
    /// Only a stored success retains a snapshot and counts toward
    /// `successor_state_capacity`. A stored failure counts toward
    /// `successor_edge_capacity` alone.
    pub(crate) fn successor(
        &mut self,
        parent: Option<SearchNodeId>,
        action: &GameAction,
        simulate: impl FnOnce() -> Option<GameState>,
    ) -> Option<(Option<SearchNodeId>, Arc<GameState>)> {
        let Some(parent) = parent else {
            self.memo_witness.successor_simulations =
                self.memo_witness.successor_simulations.saturating_add(1);
            return simulate().map(|state| (None, Arc::new(state)));
        };
        if let Some((_, stored)) = self
            .successors
            .get(&parent)
            .and_then(|edges| edges.iter().find(|(known, _)| known == action))
        {
            self.memo_witness.successor_hits = self.memo_witness.successor_hits.saturating_add(1);
            return stored
                .as_ref()
                .map(|(child, state)| (Some(*child), Arc::clone(state)));
        }
        self.memo_witness.successor_simulations =
            self.memo_witness.successor_simulations.saturating_add(1);
        let simulated = simulate();
        if self.retained_successor_states >= self.successor_state_capacity
            || self.stored_successor_edges >= self.successor_edge_capacity
        {
            return simulated.map(|state| (None, Arc::new(state)));
        }
        let tracked: TrackedSuccessor = simulated.map(|state| {
            let child = SearchNodeId(self.next_search_node);
            self.next_search_node = self.next_search_node.wrapping_add(1);
            self.retained_successor_states += 1;
            (child, Arc::new(state))
        });
        self.successors
            .entry(parent)
            .or_default()
            .push((action.clone(), tracked.clone()));
        self.stored_successor_edges += 1;
        self.memo_witness.peak_successor_edges = self
            .memo_witness
            .peak_successor_edges
            .max(self.stored_successor_edges);
        self.memo_witness.peak_retained_states = self
            .memo_witness
            .peak_retained_states
            .max(self.retained_successor_states);
        tracked.map(|(child, state)| (Some(child), state))
    }

    /// The one search edge: the state reached from `state` by `candidate`.
    ///
    /// A carried `payment_successor` is returned as-is with an untracked child
    /// and no memo access, because equivalent actions are not interchangeable
    /// across payment certificates. Otherwise the edge is exactly
    /// `apply_candidate(state, candidate)`, memoized under `node` when the
    /// position is tracked. `node`, when given, must identify `state`.
    pub(crate) fn successor_edge(
        &mut self,
        node: Option<SearchNodeId>,
        state: &GameState,
        candidate: &CandidateAction,
        payment_successor: Option<GameState>,
    ) -> Option<(Option<SearchNodeId>, Arc<GameState>)> {
        match payment_successor {
            Some(successor) => Some((None, Arc::new(successor))),
            None => self.successor(node, &candidate.action, || {
                apply_candidate(state, candidate)
            }),
        }
    }

    pub fn validate_candidates(
        &self,
        state: &GameState,
        candidates: Vec<CandidateAction>,
    ) -> Vec<CandidateAction> {
        candidates
            .into_iter()
            .filter(|candidate| Self::candidate_is_valid(state, candidate))
            .collect()
    }

    /// Preserve batch-certificate provenance across the ordinary legality gate.
    pub(crate) fn validate_prepared_candidates(
        &self,
        state: &GameState,
        candidates: Vec<PreparedCandidate>,
    ) -> Vec<PreparedCandidate> {
        candidates
            .into_iter()
            .filter(|candidate| {
                candidate.payment_successor.is_some()
                    || Self::candidate_is_valid(state, &candidate.candidate)
            })
            .collect()
    }

    fn candidate_is_valid(state: &GameState, candidate: &CandidateAction) -> bool {
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
        match &candidate.action {
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
        }
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
    /// prior that produced it and with the identity of the position it reached
    /// (`None` when untracked). `&mut self` because each continuation goes
    /// through the successor memo under `node`, which must identify `state`
    /// when given; a prior's carried payment successor is moved out and never
    /// memoized.
    fn sample_backfilled_continuations(
        &mut self,
        node: Option<SearchNodeId>,
        state: &GameState,
        mut priors: Vec<PolicyPrior>,
        sample_count: usize,
    ) -> Vec<(f64, Option<SearchNodeId>, Arc<GameState>)> {
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
            let sim = self.successor_edge(node, state, &prior.candidate, prior.payment_successor);
            if let Some((child, sim)) = sim {
                out.push((prior.prior, child, sim));
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
    ///
    /// Untracked owned form of [`PlannerServices::quiesce_at`], kept for tests.
    #[cfg(test)]
    fn quiesce(&mut self, state: &GameState) -> GameState {
        self.quiesce_at(None, state).into_owned()
    }

    /// Apply one forced quiescence `action` at the cursor's position.
    ///
    /// When the position is tracked and not payment-affiliated, the step goes
    /// through the successor memo: the cursor moves onto the shared successor
    /// and nothing is cloned or applied on a revisit. Otherwise (untracked,
    /// affiliated, or a tracked apply that failed) the cursor takes ownership
    /// and the action is applied in place; on failure the cursor is left
    /// holding the state exactly as the failed in-place apply left it.
    fn quiesce_step(
        &mut self,
        node: Option<SearchNodeId>,
        cursor: &mut QuiesceCursor<'_>,
        action: GameAction,
    ) -> QuiesceStep {
        if node.is_some()
            && matches!(
                classify_payment_continuation(cursor.state()),
                PaymentContinuationState::NotAffiliated
            )
        {
            let shared = self.successor(node, &action, || {
                let mut sim = cursor.state().clone();
                apply_as_current_for_simulation(&mut sim, action.clone()).ok()?;
                Some(sim)
            });
            if let Some((child, successor)) = shared {
                *cursor = QuiesceCursor::Shared(successor);
                return QuiesceStep::Advanced(child);
            }
        }
        match apply_as_current_for_simulation(cursor.make_owned(), action) {
            Ok(_) => QuiesceStep::Advanced(None),
            Err(_) => QuiesceStep::Failed,
        }
    }

    /// Quiescence from a position that may be tracked (see
    /// [`PlannerServices::quiesce`] for what it resolves). Returns a cursor at
    /// the quiet position: still borrowing `state` when no step was taken.
    ///
    /// Every break (deadline, game over, no candidates, step cap) and every
    /// choice is recomputed on each call. Only two pure things are shared for a
    /// tracked position: its decision context and the successor of
    /// `(position, chosen action)`. The quiesced result itself is never
    /// memoized, so a revisit that chooses differently simply follows a
    /// different edge. `node`, when given, must identify `state`.
    fn quiesce_at<'s>(
        &mut self,
        mut node: Option<SearchNodeId>,
        state: &'s GameState,
    ) -> QuiesceCursor<'s> {
        const MAX_QUIESCE_STEPS: u32 = 20;

        let mut cursor = QuiesceCursor::Borrowed(state);
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

            if matches!(cursor.state().waiting_for, WaitingFor::GameOver { .. }) {
                break;
            }

            let ctx = self.exact_decision_context(node, cursor.state());

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
                match self.quiesce_step(
                    node,
                    &mut cursor,
                    engine::types::actions::GameAction::PassPriority,
                ) {
                    QuiesceStep::Advanced(next) => {
                        node = next;
                        continue;
                    }
                    QuiesceStep::Failed => break,
                }
            }

            // Case 2: Only one legal action — apply it (forced move)
            if ctx.candidates.len() == 1 {
                match self.quiesce_step(node, &mut cursor, ctx.candidates[0].action.clone()) {
                    QuiesceStep::Advanced(next) => {
                        node = next;
                        continue;
                    }
                    QuiesceStep::Failed => break,
                }
            }

            // Case 3: Deterministic mechanical choice (scry, surveil, mulligan, etc.)
            // These are non-strategic decisions that can be resolved with heuristics.
            let actions: Vec<_> = ctx.candidates.iter().map(|c| c.action.clone()).collect();
            let acting_player = cursor
                .state()
                .waiting_for
                .acting_player()
                .unwrap_or(self.ai_player);
            if let Some(action) = crate::search::deterministic_choice(
                cursor.state(),
                acting_player,
                self.config,
                &actions,
                None,
            ) {
                match self.quiesce_step(node, &mut cursor, action) {
                    QuiesceStep::Advanced(next) => {
                        node = next;
                        continue;
                    }
                    QuiesceStep::Failed => break,
                }
            }

            // Genuine decision point — stop quiescence
            break;
        }
        cursor
    }

    /// Evaluate a leaf state with quiescence: if the stack is non-empty and only
    /// forced passes remain, resolve through them before evaluating.
    /// Once the wall-clock deadline is blown, skip quiescence — the cached
    /// static eval is a good-enough approximation and quiescence can itself
    /// clone + simulate state through the stack.
    pub fn evaluate_state_quiesced(&mut self, state: &GameState) -> f64 {
        self.evaluate_state_quiesced_at(None, state)
    }

    /// [`PlannerServices::evaluate_state_quiesced`] for a position that may be
    /// tracked; `node`, when given, must identify `state`. The quiesced state
    /// is evaluated by reference, so a revisit of a tracked position clones
    /// nothing.
    pub(crate) fn evaluate_state_quiesced_at(
        &mut self,
        node: Option<SearchNodeId>,
        state: &GameState,
    ) -> f64 {
        if state.stack.is_empty() || self.deadline.expired() {
            return self.evaluate_state_cached(state);
        }
        let quiesced = self.quiesce_at(node, state);
        self.evaluate_state_cached(quiesced.state())
    }

    /// Evaluate a leaf state for utility with quiescence.
    pub fn quiesced_leaf_eval(&mut self, state: &GameState) -> f64 {
        self.quiesced_leaf_eval_at(None, state)
    }

    /// [`PlannerServices::quiesced_leaf_eval`] for a position that may be
    /// tracked; `node`, when given, must identify `state`.
    fn quiesced_leaf_eval_at(&mut self, node: Option<SearchNodeId>, state: &GameState) -> f64 {
        if state.stack.is_empty() || self.deadline.expired() {
            let value = self.evaluate_for_planner(state);
            return self.reduce_utility(state, &value);
        }
        let quiesced = self.quiesce_at(node, state);
        let value = self.evaluate_for_planner(quiesced.state());
        self.reduce_utility(quiesced.state(), &value)
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

    /// Policy priors and the planner value for `state`, as a rollout node sees
    /// them. Computed afresh on every call: nothing a policy scores is
    /// memoized.
    pub fn planner_evaluation(&mut self, state: &GameState) -> PlannerEvaluation {
        self.planner_evaluation_at(None, state)
    }

    /// [`PlannerServices::planner_evaluation`] for a position that may be
    /// tracked; `node`, when given, must identify `state`. Only the decision
    /// context can come from a memo; the priors and the value are recomputed
    /// on every call.
    fn planner_evaluation_at(
        &mut self,
        node: Option<SearchNodeId>,
        state: &GameState,
    ) -> PlannerEvaluation {
        // Rollout leaf skips upfront legality validation — illegal candidates are
        // dropped at apply time by `sample_backfilled_continuations`
        // (`apply_candidate` → None), mirroring the beam path. This removes one
        // clone-and-apply-per-candidate probe (`state_clone_for_legality`).
        let ctx = self.build_decision_context_at(node, state);
        let candidates = prepare_payment_candidates(state, ctx.candidates.clone());
        let scoring_player = state.waiting_for.acting_player().unwrap_or(self.ai_player);
        let mut priors = self.policy_priors(
            state,
            &ctx,
            &candidates
                .iter()
                .map(|prepared| prepared.candidate.clone())
                .collect::<Vec<_>>(),
            scoring_player,
            SearchDepth::Lookahead,
        );
        for (prior, prepared) in priors.iter_mut().zip(&candidates) {
            prior.payment_successor = prepared.payment_successor.clone();
        }
        PlannerEvaluation {
            // Rollout leaf: every node reached here is deep lookahead, never the
            // committed decision, so board-wide/affordability policies self-gate.
            priors,
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

    /// Depth-limited rollout value of `state`: sample the best-prior legal
    /// continuations, recurse, and fall back to the quiesced leaf evaluation at
    /// depth 0, at game over, or once the wall-clock deadline is blown.
    pub fn rollout_estimate(&mut self, state: &GameState, depth: u32) -> f64 {
        self.rollout_estimate_at(None, state, depth)
    }

    /// [`PlannerServices::rollout_estimate`] for a position that may be
    /// tracked; `node`, when given, must identify `state`. Scoring runs on
    /// every visit; only the pure successor and decision-context work is
    /// shared with earlier visits of the same position.
    pub(crate) fn rollout_estimate_at(
        &mut self,
        node: Option<SearchNodeId>,
        state: &GameState,
        depth: u32,
    ) -> f64 {
        // CR-agnostic: if the wall-clock budget is blown, short-circuit to the
        // cheap leaf evaluator rather than descending further. Without this
        // bail, rollout recursion ignores `time_budget_ms` entirely.
        if self.deadline.expired() {
            return self.quiesced_leaf_eval_at(node, state);
        }
        if depth == 0 || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return self.quiesced_leaf_eval_at(node, state);
        }

        let evaluation = self.planner_evaluation_at(node, state);
        if evaluation.priors.is_empty() {
            return self.quiesced_leaf_eval_at(node, state);
        }

        let rollout_player = state.waiting_for.acting_player().unwrap_or(self.ai_player);
        let sample_count = self.config.search.rollout_samples.max(1) as usize;
        let continuations =
            self.sample_backfilled_continuations(node, state, evaluation.priors, sample_count);
        if continuations.is_empty() {
            return self.quiesced_leaf_eval_at(node, state);
        }
        // CR-agnostic: if the budget was blown while sampling continuations,
        // short-circuit to the cheap leaf eval instead of descending the rollout
        // tree. Closes the post-sampling path the reporter flagged; the top-of-
        // function guard covers a deadline already expired on entry.
        if self.deadline.expired() {
            return self.quiesced_leaf_eval_at(node, state);
        }
        let is_maximizing = rollout_player == self.ai_player;
        continuations
            .into_iter()
            .map(|(prior, child, sim)| {
                self.rollout_estimate_at(child, &sim, depth - 1) + (prior * 0.05)
            })
            .reduce(|best, value| {
                if is_maximizing {
                    best.max(value)
                } else {
                    best.min(value)
                }
            })
            .unwrap_or_else(|| self.quiesced_leaf_eval_at(node, state))
    }
}

pub trait ContinuationPlanner {
    /// Value of `state`, the position reached after a root action, from the
    /// searching player's point of view.
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
    /// Alpha-beta beam value of `state` at `depth` plies remaining. Untracked
    /// wrapper over [`BeamContinuationPlanner::search_value_at`], kept for
    /// tests that drive the search directly.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn search_value(
        &self,
        state: &GameState,
        depth: u32,
        ply: usize,
        alpha: f64,
        beta: f64,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        self.search_value_at(None, state, depth, ply, alpha, beta, services, budget)
    }

    /// Alpha-beta beam value of `state` at `depth` plies remaining, for a
    /// position that may be tracked; `node`, when given, must identify `state`.
    /// Ranking, killer ordering, the transposition table, the budget and the
    /// deadline behave the same for tracked and untracked positions; a tracked
    /// position only reuses its decision context and the successors of the
    /// actions already applied there.
    #[allow(clippy::too_many_arguments)]
    fn search_value_at(
        &self,
        node: Option<SearchNodeId>,
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
            return services.rollout_estimate_at(node, state, self.rollout_depth);
        }
        if budget.exhausted() || matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            return services.evaluate_state_quiesced_at(node, state);
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

        let ctx = services.build_decision_context_at(node, state);
        // Skip upfront validation in beam search — invalid candidates are handled
        // by apply_candidate returning None in the loop below. This avoids cloning
        // the state once per candidate just to test validity. The rollout leaf
        // path (planner_evaluation → sample_backfilled_continuations) applies the
        // same skip: illegal candidates are dropped when apply_candidate returns
        // None during backfill sampling, not by an upfront clone-per-candidate probe.
        let candidates = prepare_payment_candidates(state, ctx.candidates.clone());
        if candidates.is_empty() {
            return services.evaluate_state_quiesced_at(node, state);
        }

        let node_player = state.waiting_for.acting_player();
        let is_maximizing = node_player.is_none_or(|player| player == services.ai_player);
        let scoring_player = node_player.unwrap_or(services.ai_player);
        let mut ranked = rank_prepared_candidates(
            candidates,
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
            let Some((child, sim)) = services.successor_edge(
                node,
                state,
                &ranked.candidate,
                ranked.payment_successor.clone(),
            ) else {
                continue;
            };
            let value = self.search_value_at(
                child,
                &sim,
                depth - 1,
                ply + 1,
                alpha,
                beta,
                services,
                budget,
            ) + (ranked.score * 0.05);

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
            services.evaluate_state_quiesced_at(node, state)
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

    /// [`ContinuationPlanner::evaluate_after_action`] for a position that may
    /// be tracked; `node`, when given, must identify `state`.
    pub(crate) fn evaluate_after_action_at(
        &self,
        node: Option<SearchNodeId>,
        state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        if self.depth == 0 {
            services.evaluate_state_quiesced_at(node, state)
        } else {
            // Ply-from-root starts at 0 for the rung root: a killer recorded at
            // ply 1 of one rung is reused at ply 1 of the next (chess convention).
            self.search_value_at(
                node,
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

impl ContinuationPlanner for BeamContinuationPlanner {
    /// Untracked form of [`BeamContinuationPlanner::evaluate_after_action_at`].
    fn evaluate_after_action(
        &mut self,
        state: &GameState,
        services: &mut PlannerServices<'_>,
        budget: &mut SearchBudget,
    ) -> f64 {
        self.evaluate_after_action_at(None, state, services, budget)
    }
}

#[cfg(test)]
pub fn rank_candidates<F>(
    candidates: impl IntoIterator<Item = CandidateAction>,
    mut scorer: F,
    limit: usize,
) -> Vec<RankedCandidate>
where
    F: FnMut(&CandidateAction) -> f64,
{
    rank_prepared_candidates(
        candidates
            .into_iter()
            .enumerate()
            .map(|(source_index, candidate)| PreparedCandidate {
                source_index,
                candidate,
                payment_successor: None,
            }),
        |candidate| scorer(candidate),
        limit,
    )
}

pub fn apply_candidate(state: &GameState, candidate: &CandidateAction) -> Option<GameState> {
    match classify_payment_continuation(state) {
        // Payment edges must consume the cached decision-wide witness produced
        // before ranking. Re-running it here would multiply its global bound.
        PaymentContinuationState::Affiliated(_) => return None,
        PaymentContinuationState::UnsupportedAffiliated(_) => return None,
        PaymentContinuationState::NotAffiliated => {}
    }
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
    /// plus the THREE serve-time carve-outs (`energy_offset`,
    /// `mana_development_offset`, `threat_adjustment`), must equal
    /// `evaluate_with_strategy` to 1e-9. Runs a reach-guard pair — one config
    /// where `threat_adjustment` is provably nonzero, one where it is zero — and
    /// asserts both fixed offsets are non-vacuous (`p0.energy > 0`, and p0
    /// controls a mana source).
    ///
    /// # What this test does and does NOT guarantee
    ///
    /// It previously claimed that "a future `evaluate_with_strategy` term without
    /// a matching `FeatureRow` field would break this identity loudly". **That was
    /// false as stated**, and the correction matters because the false version
    /// invited exactly the mistake it appeared to prevent. This test compares two
    /// numbers on ONE fixture: a new term that happens to be `0.0` here leaves the
    /// identity holding while the harvested vector silently omits it. The
    /// guarantee was fixture-dependent, not structural — which is why this unit had
    /// to add a tapped land to make its own term non-zero.
    ///
    /// The structural half now lives in the type system rather than here:
    ///
    /// - A term added to **`EvalFeatures`** is caught at compile time. Both
    ///   `FeatureRow::extract` and `EvaluationBreakdown::total` destructure
    ///   exhaustively with no `..`, so a new field is an **E0027** rather than a
    ///   silent drop. This is the shape every term in this unit takes.
    /// - A term added **directly in `evaluate_with_strategy`** — as
    ///   `threat_adjustment` is — has no such tie and remains **fixture-dependent**.
    ///   Nothing forces it onto `FeatureRow`, so if it is zero on this fixture the
    ///   omission still passes silently. Anyone adding one must extend this
    ///   fixture to make it provably non-zero, exactly as the `threat_adjustment`
    ///   reach-guard pair below already does.
    ///
    /// State the narrower true guarantee rather than the broad false one: this
    /// test pins the *arithmetic* of the identity; the compiler pins the
    /// *completeness* of the `EvalFeatures` half; nothing yet pins the
    /// completeness of the `evaluate_with_strategy` half.
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

        // Exactly one TAPPED land for p0. It does double duty: `available_mana`
        // filters `&& !obj.tapped`, so `available_mana(p0)` stays 0 and the
        // `ai_mana <= 1` counter-tapout reach-guard above is preserved exactly —
        // while `is_intrinsic_mana_source` deliberately ignores tapped state, so
        // `mana_development_offset` becomes non-vacuous. Bind the `CardId` before
        // the `&mut state` borrow (an explicit `&mut` in a free-function argument
        // list is not a two-phase borrow — inlining it is E0502).
        let land_card_id = CardId(state.next_object_id);
        let land_id = create_object(
            &mut state,
            land_card_id,
            PlayerId(0),
            "Tapped Land".to_string(),
            Zone::Battlefield,
        );
        let land = state.objects.get_mut(&land_id).unwrap();
        land.card_types.core_types.push(CoreType::Land);
        land.tapped = true;

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
        let reconstructed = row.weighted_total(weights)
            + row.energy_offset
            + row.mana_development_offset
            + threat_adj;
        let planner_eval = services.evaluate_with_strategy(&state);

        assert!(
            row.energy_offset > 0.0,
            "energy term must be non-vacuous (p0.energy > 0)"
        );
        assert!(
            row.mana_development_offset > 0.0,
            "mana-development term must be non-vacuous (p0 controls a tapped land)"
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
        let reconstructed_none = row_none.weighted_total(weights_none)
            + row_none.energy_offset
            + row_none.mana_development_offset;
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
            must_be_blocked_targets: Default::default(),
            block_capacities: Default::default(),
        };

        let mut right = make_state();
        right.waiting_for = WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![ObjectId(30)],
            valid_block_targets: right_targets,
            block_requirements: right_requirements,
            blocker_constraints: Default::default(),
            must_be_blocked_targets: Default::default(),
            block_capacities: Default::default(),
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

    /// A position with an empty stack is already quiet, so quiescence returns it unchanged.
    #[test]
    fn quiesce_is_noop_on_empty_stack() {
        let state = make_state();
        assert!(state.stack.is_empty());
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(PlayerId(0), &config, &policies);
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

    /// Quiescence resolves a creature spell on the stack before the position is evaluated.
    #[test]
    fn quiesce_resolves_creature_spell_on_stack() {
        // Both players have priority, only PassPriority is legal
        // (creature spell on stack, no instant-speed responses available)
        let state = creature_spell_on_stack_state();
        let battlefield_before = state.battlefield.len();

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(PlayerId(0), &config, &policies);
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
        let mut live = PlannerServices::with_deadline(
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
        let mut expired = PlannerServices::with_deadline(
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
    /// Sampling backfills past an illegal high-prior candidate without spending a sample slot.
    #[test]
    fn sample_backfilled_continuations_backfills_past_illegal_high_prior() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(PlayerId(0), &config, &policies);

        let illegal = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: ObjectId(99999),
                ability_index: 0,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
            payment_successor: None,
        };
        let legal_prior = PolicyPrior {
            candidate: legal.clone(),
            prior: 0.1,
            payment_successor: None,
        };

        // sample_count=1: the high-prior illegal candidate is backfilled past, and
        // the single surviving slot carries the legal candidate. A take-then-filter
        // implementation would take the illegal top, drop it, and return [].
        let sampled = services.sample_backfilled_continuations(
            None,
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
            payment_successor: None,
        };
        let legal_b = PolicyPrior {
            candidate: legal.clone(),
            prior: 0.15,
            payment_successor: None,
        };
        let two = services.sample_backfilled_continuations(
            None,
            &state,
            vec![illegal_prior.clone(), legal_a, legal_b],
            2,
        );
        assert_eq!(two.len(), 2, "both legal candidates survive backfill");
        assert!(
            two.iter().all(|(p, _, _)| *p <= 0.2),
            "only the legal priors (<= 0.2) survive; the illegal 0.9 top is dropped"
        );

        // Sibling: empty priors → empty result.
        assert!(services
            .sample_backfilled_continuations(None, &state, Vec::new(), 1)
            .is_empty());

        // Sibling: all-illegal → empty result (nothing to backfill from).
        assert!(
            services
                .sample_backfilled_continuations(
                    None,
                    &state,
                    vec![illegal_prior.clone(), illegal_prior.clone()],
                    2,
                )
                .is_empty(),
            "all-illegal priors yield no continuations"
        );
    }

    /// A retained payment successor is used directly instead of the inapplicable rollout fallback.
    #[test]
    fn retained_payment_successor_bypasses_inapplicable_rollout_fallback() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(PlayerId(0), &config, &policies);
        let fallback = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: ObjectId(99999),
                ability_index: 0,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Mana),
        };
        assert!(
            services.apply_candidate(&state, &fallback).is_none(),
            "reach-guard: the hostile fallback cannot be applied at this root"
        );
        let retained = services
            .apply_candidate(
                &state,
                &CandidateAction {
                    action: GameAction::PassPriority,
                    metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
                },
            )
            .expect("reach-guard: a concrete retained successor exists");
        let sampled = services.sample_backfilled_continuations(
            None,
            &state,
            vec![PolicyPrior {
                candidate: fallback,
                prior: 1.0,
                payment_successor: Some(retained.clone()),
            }],
            1,
        );
        assert_eq!(sampled.len(), 1);
        assert_eq!(
            quick_state_hash(&sampled[0].2),
            quick_state_hash(&retained),
            "rollout consumes the witnessed successor instead of retrying its hostile fallback"
        );
    }

    // T3′: rollout_estimate at its production entry point must not probe legality.
    // PINNED to a land-only empty-stack fixture: every sampled continuation leaves
    // an empty stack, so the depth-0 leaf never enters quiesce → build_decision_context
    // (mod.rs:584), which would otherwise fire the generation recorders.
    /// The rollout estimate does not probe legality at its production entry point.
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
        let continuations = services.sample_backfilled_continuations(
            None,
            &state,
            eval.priors.clone(),
            sample_count,
        );
        assert!(
            !continuations.is_empty(),
            "reach-guard B: at least one legal continuation must be sampled"
        );
        assert!(
            continuations.iter().all(|(_, _, sim)| sim.stack.is_empty()),
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
            live.sample_backfilled_continuations(None, &state, eval.priors.clone(), sample_count);
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
        let empty = expired.sample_backfilled_continuations(
            None,
            &state,
            eval.priors.clone(),
            sample_count,
        );
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
                ability: Box::new(ResolvedAbility::new(Effect::NoOp, vec![], src, ctrl)),
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
        RankedCandidate::new(
            CandidateAction {
                action,
                metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
            },
            score,
        )
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

    /// The AI cache keys hash stored containers, so a pile in the canonical seat's
    /// container is folded once under the shared-zone axis, not once per seat.
    #[test]
    fn position_hashes_fold_the_shared_pile_once() {
        let mut standard = make_state();
        for id in [1, 2, 3] {
            standard.players[0].library.push_back(ObjectId(id));
        }
        standard.players[0].graveyard.push_back(ObjectId(4));
        let mut dandan = standard.clone();
        dandan.format_config = engine::types::format::FormatConfig::dandan();

        assert_eq!(quick_state_hash(&standard), quick_state_hash(&dandan));
        assert_eq!(
            search_position_hash(&standard),
            search_position_hash(&dandan)
        );

        let mut reordered = dandan.clone();
        let top = reordered.players[0].library.pop_front().unwrap();
        reordered.players[0].library.push_back(top);
        assert_eq!(
            quick_state_hash(&dandan),
            quick_state_hash(&reordered),
            "reach: quick hash sees the library length only"
        );
        assert_ne!(
            search_position_hash(&dandan),
            search_position_hash(&reordered),
            "reach: the position hash reads the pile's order"
        );
        let mut buried = dandan.clone();
        buried.players[0].graveyard.push_back(ObjectId(5));
        assert_ne!(
            quick_state_hash(&dandan),
            quick_state_hash(&buried),
            "reach: the pile's graveyard contents are hashed"
        );
    }

    /// A `PassPriority` candidate for player 0, the legal edge of the memo tests.
    fn pass_candidate() -> CandidateAction {
        CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Pass),
        }
    }

    /// An activation of an object that does not exist: an edge whose apply fails.
    fn inapplicable_candidate() -> CandidateAction {
        CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: ObjectId(99999),
                ability_index: 0,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Mana),
        }
    }

    /// T1: a tracked `(parent, action)` pair is simulated once; the revisit is a
    /// memo hit that returns the very same successor, equal to a fresh apply.
    #[test]
    fn successor_memo_simulates_a_tracked_edge_once() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let pass = pass_candidate();
        let root = services.new_search_root();
        let runs = std::cell::Cell::new(0u32);
        let simulate = || {
            runs.set(runs.get() + 1);
            apply_candidate(&state, &pass)
        };

        let (first_child, first) = services
            .successor(Some(root), &pass.action, simulate)
            .expect("reach-guard: passing priority applies at this root");
        let (second_child, second) = services
            .successor(Some(root), &pass.action, simulate)
            .expect("the memoized edge is still a success");

        assert_eq!(runs.get(), 1, "the revisit must not simulate again");
        assert_eq!(services.memo_witness.successor_hits, 1);
        assert_eq!(services.memo_witness.successor_simulations, 1);
        assert_eq!(services.memo_witness.peak_successor_edges, 1);
        assert_eq!(services.memo_witness.peak_retained_states, 1);
        assert!(first_child.is_some(), "a stored success is tracked");
        assert_eq!(first_child, second_child);
        assert!(
            Arc::ptr_eq(&first, &second),
            "the hit shares the stored state"
        );
        let fresh = apply_candidate(&state, &pass).expect("fresh apply succeeds");
        assert!(
            *first == fresh,
            "the memoized successor equals a fresh apply"
        );
    }

    /// T2: an untracked parent never reads or writes the memo. Reach-guard:
    /// `successor_memo_simulates_a_tracked_edge_once` on the same fixture.
    #[test]
    fn successor_memo_is_bypassed_for_an_untracked_parent() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let pass = pass_candidate();
        let runs = std::cell::Cell::new(0u32);
        let simulate = || {
            runs.set(runs.get() + 1);
            apply_candidate(&state, &pass)
        };

        for _ in 0..2 {
            let (child, _) = services
                .successor(None, &pass.action, simulate)
                .expect("reach-guard: passing priority applies");
            assert!(
                child.is_none(),
                "an untracked parent yields an untracked child"
            );
        }

        assert_eq!(runs.get(), 2, "every untracked request simulates");
        assert_eq!(services.memo_witness.successor_hits, 0);
        assert_eq!(services.memo_witness.successor_simulations, 2);
        assert_eq!(services.memo_witness.peak_successor_edges, 0);
        assert_eq!(services.memo_witness.peak_retained_states, 0);
        assert!(services.successors.is_empty());
    }

    /// T3: a failed apply at a tracked parent is memoized as a failure, which
    /// is an edge that retains no state.
    #[test]
    fn successor_memo_remembers_a_failed_apply() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let illegal = inapplicable_candidate();
        let root = services.new_search_root();
        let runs = std::cell::Cell::new(0u32);
        let simulate = || {
            runs.set(runs.get() + 1);
            apply_candidate(&state, &illegal)
        };

        assert!(services
            .successor(Some(root), &illegal.action, simulate)
            .is_none());
        assert!(services
            .successor(Some(root), &illegal.action, simulate)
            .is_none());

        assert_eq!(runs.get(), 1, "the stored failure answers the revisit");
        assert_eq!(services.memo_witness.successor_hits, 1);
        assert_eq!(services.memo_witness.peak_successor_edges, 1);
        assert_eq!(services.memo_witness.peak_retained_states, 0);
    }

    /// T4 (hostile): two roots on different states never share an edge, even
    /// for an equal action. Reach-guard: the T1 test on the same action.
    #[test]
    fn successor_memo_keeps_distinct_roots_apart() {
        let with_land = make_state_with_land();
        let without_land = make_state();
        assert!(with_land != without_land, "fixture: the roots differ");
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let pass = pass_candidate();
        let first_root = services.new_search_root();
        let second_root = services.new_search_root();
        assert_ne!(first_root, second_root);

        let (_, from_first) = services
            .successor(Some(first_root), &pass.action, || {
                apply_candidate(&with_land, &pass)
            })
            .expect("reach-guard: pass applies at the first root");
        let (_, from_second) = services
            .successor(Some(second_root), &pass.action, || {
                apply_candidate(&without_land, &pass)
            })
            .expect("reach-guard: pass applies at the second root");

        assert_eq!(services.memo_witness.successor_simulations, 2);
        assert_eq!(services.memo_witness.successor_hits, 0);
        assert!(!Arc::ptr_eq(&from_first, &from_second));
        assert!(
            *from_first != *from_second,
            "each root keeps the successor of its own state"
        );
    }

    /// T5: once `successor_state_capacity` snapshots are retained an edge is
    /// simulated on every visit and its child is untracked, while the edges
    /// stored earlier keep hitting. The witness reports the bound binding.
    #[test]
    fn successor_memo_falls_open_past_capacity() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        services.successor_state_capacity = 1;
        let pass = pass_candidate();
        let other = build_decision_context(&state)
            .candidates
            .into_iter()
            .find(|candidate| !matches!(candidate.action, GameAction::PassPriority))
            .expect("fixture: the land in hand gives a second legal action");
        let root = services.new_search_root();

        let (stored_child, _) = services
            .successor(Some(root), &pass.action, || apply_candidate(&state, &pass))
            .expect("reach-guard: pass applies");
        assert!(stored_child.is_some(), "the first edge fits and is tracked");

        for _ in 0..2 {
            let (child, _) = services
                .successor(Some(root), &other.action, || {
                    apply_candidate(&state, &other)
                })
                .expect("reach-guard: the second action applies");
            assert!(child.is_none(), "an edge past capacity is untracked");
        }
        assert_eq!(
            services.memo_witness.successor_simulations, 3,
            "the edge past capacity simulates on every visit"
        );
        assert_eq!(services.memo_witness.successor_hits, 0);
        assert_eq!(services.memo_witness.peak_successor_edges, 1);
        assert_eq!(
            services.memo_witness.peak_retained_states, 1,
            "the retained snapshots stop at the capacity"
        );

        let (again, _) = services
            .successor(Some(root), &pass.action, || apply_candidate(&state, &pass))
            .expect("the stored edge is still there");
        assert_eq!(again, stored_child);
        assert_eq!(services.memo_witness.successor_hits, 1);
        assert_eq!(services.memo_witness.successor_simulations, 3);
    }

    /// T5b: a stored failure retains no snapshot, so it takes none of the
    /// retained-state budget: with room for one snapshot, a success requested
    /// after a failure is still tracked. Once that snapshot is retained the
    /// memo is closed to every new edge, a failing one included. Reach-guard:
    /// the failure and the success are both stored, as their revisits hit.
    #[test]
    fn failed_apply_takes_no_retained_state_budget() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        services.successor_state_capacity = 1;
        let pass = pass_candidate();
        let illegal = inapplicable_candidate();
        let root = services.new_search_root();

        assert!(services
            .successor(Some(root), &illegal.action, || {
                apply_candidate(&state, &illegal)
            })
            .is_none());
        assert_eq!(services.memo_witness.peak_successor_edges, 1);
        assert_eq!(services.memo_witness.peak_retained_states, 0);

        let (child, _) = services
            .successor(Some(root), &pass.action, || apply_candidate(&state, &pass))
            .expect("reach-guard: pass applies");
        let child = child.expect("the failure left the whole state budget free");
        assert_eq!(services.memo_witness.peak_successor_edges, 2);
        assert_eq!(services.memo_witness.peak_retained_states, 1);

        assert!(services
            .successor(Some(root), &illegal.action, || {
                apply_candidate(&state, &illegal)
            })
            .is_none());
        services
            .successor(Some(root), &pass.action, || apply_candidate(&state, &pass))
            .expect("the stored success is still there");
        assert_eq!(
            services.memo_witness.successor_hits, 2,
            "reach-guard: both edges were stored"
        );
        assert_eq!(services.memo_witness.successor_simulations, 2);

        // The budget is spent: a new failing edge below the tracked child is
        // simulated on every visit and never stored.
        for _ in 0..2 {
            assert!(services
                .successor(Some(child), &illegal.action, || {
                    apply_candidate(&state, &illegal)
                })
                .is_none());
        }
        assert_eq!(services.memo_witness.successor_simulations, 4);
        assert_eq!(services.memo_witness.successor_hits, 2);
        assert_eq!(services.memo_witness.peak_successor_edges, 2);
        assert_eq!(services.memo_witness.peak_retained_states, 1);
    }

    /// T5c: failed applies are bounded by `successor_edge_capacity` on their
    /// own, so a search that only ever fails cannot grow the edge lists
    /// without limit, and reaching that ceiling closes the memo to successes
    /// too. Reach-guard: the first failure is stored and answers its revisit.
    #[test]
    fn failed_applies_stop_at_the_edge_capacity() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        services.successor_edge_capacity = 1;
        let pass = pass_candidate();
        let illegal = inapplicable_candidate();
        let first_root = services.new_search_root();
        let second_root = services.new_search_root();

        for _ in 0..2 {
            assert!(services
                .successor(Some(first_root), &illegal.action, || {
                    apply_candidate(&state, &illegal)
                })
                .is_none());
        }
        assert_eq!(
            services.memo_witness.successor_hits, 1,
            "reach-guard: the first failure is stored"
        );

        for _ in 0..2 {
            assert!(services
                .successor(Some(second_root), &illegal.action, || {
                    apply_candidate(&state, &illegal)
                })
                .is_none());
        }
        let (child, _) = services
            .successor(Some(second_root), &pass.action, || {
                apply_candidate(&state, &pass)
            })
            .expect("reach-guard: pass applies");
        assert!(child.is_none(), "a full edge list tracks no new child");
        assert_eq!(services.memo_witness.successor_hits, 1);
        assert_eq!(services.memo_witness.successor_simulations, 4);
        assert_eq!(services.memo_witness.peak_successor_edges, 1);
        assert_eq!(services.memo_witness.peak_retained_states, 0);
    }

    /// T5d: the decision-context memo holds at most
    /// `successor_state_capacity + 1` contexts however many positions are
    /// tracked. One root and one retained child fill it at capacity 1; a
    /// second root, the only way to track a further position, is then built on
    /// every request. Reach-guard: the two stored contexts answer their
    /// revisits from the memo.
    #[test]
    fn context_memo_stays_within_the_retained_state_bound() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        services.successor_state_capacity = 1;
        let pass = pass_candidate();
        let root = services.new_search_root();
        let (child, child_state) = services
            .successor(Some(root), &pass.action, || apply_candidate(&state, &pass))
            .expect("reach-guard: pass applies");
        let child = child.expect("the first edge is tracked");
        let extra_root = services.new_search_root();

        for _ in 0..2 {
            services.exact_decision_context(Some(root), &state);
            services.exact_decision_context(Some(child), &child_state);
        }
        assert_eq!(services.memo_witness.context_builds, 2);
        assert_eq!(
            services.memo_witness.context_hits, 2,
            "reach-guard: the root and the retained child are memoized"
        );
        assert_eq!(services.node_contexts.len(), 2);

        let fresh = build_decision_context(&state);
        for _ in 0..2 {
            let ctx = services.exact_decision_context(Some(extra_root), &state);
            assert_eq!(
                ctx.candidates.len(),
                fresh.candidates.len(),
                "a context past the bound is still the exact one"
            );
        }
        assert_eq!(
            services.memo_witness.context_builds, 4,
            "a position past the bound is built on every request"
        );
        assert_eq!(services.memo_witness.context_hits, 2);
        assert_eq!(services.node_contexts.len(), 2);
        assert_eq!(services.memo_witness.peak_node_contexts, 2);
        assert_eq!(
            services.memo_witness.peak_node_contexts,
            services.successor_state_capacity + 1
        );
    }

    /// T6 (hostile): an edge carrying a payment successor returns that state
    /// with an untracked child and never touches the action-keyed memo, so an
    /// equal action with a different payment certificate cannot be confused
    /// with it. Reach-guard: the same node and action without a payment
    /// successor do go through the memo.
    #[test]
    fn payment_successor_edge_is_never_memoized() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let illegal = inapplicable_candidate();
        let retained = apply_candidate(&state, &pass_candidate())
            .expect("reach-guard: a concrete retained successor exists");
        let root = services.new_search_root();

        for _ in 0..2 {
            let (child, successor) = services
                .successor_edge(Some(root), &state, &illegal, Some(retained.clone()))
                .expect("a carried payment successor is the edge's result");
            assert!(child.is_none(), "a payment edge yields an untracked child");
            assert!(*successor == retained);
        }
        assert!(services.successors.is_empty());
        assert_eq!(services.memo_witness, SearchMemoWitness::default());

        assert!(
            services
                .successor_edge(Some(root), &state, &illegal, None)
                .is_none(),
            "reach-guard: without the payment successor the action itself is applied"
        );
        assert_eq!(services.memo_witness.successor_simulations, 1);
        assert_eq!(services.memo_witness.peak_successor_edges, 1);
        let (child, successor) = services
            .successor_edge(Some(root), &state, &illegal, Some(retained.clone()))
            .expect("a stored failure for the bare action must not shadow the payment edge");
        assert!(child.is_none());
        assert!(*successor == retained);
        assert_eq!(services.memo_witness.successor_hits, 0);
    }

    /// T11: quiescing a tracked position a second time simulates nothing and
    /// builds no context, and its value is bit-equal to an untracked run on a
    /// separate services instance (on one instance the evaluation cache would
    /// make the equality vacuous).
    #[test]
    fn tracked_quiescence_revisit_reuses_successors_and_contexts() {
        let state = creature_spell_on_stack_state();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();

        let mut tracked = hard_services(&config, &policies);
        let node = tracked.new_search_root();
        let first = tracked.evaluate_state_quiesced_at(Some(node), &state);
        let after_first = tracked.memo_witness;
        assert!(
            after_first.successor_simulations > 0,
            "reach-guard: the first visit resolves the stack through the memo"
        );
        assert!(after_first.context_builds > 0);
        let second = tracked.evaluate_state_quiesced_at(Some(node), &state);
        let after_second = tracked.memo_witness;
        assert_eq!(
            after_second.successor_simulations, after_first.successor_simulations,
            "the revisit must not simulate any step again"
        );
        assert_eq!(
            after_second.context_builds, after_first.context_builds,
            "the revisit must not rebuild any decision context"
        );
        assert!(after_second.successor_hits > after_first.successor_hits);
        assert!(after_second.context_hits > after_first.context_hits);
        assert_eq!(first.to_bits(), second.to_bits());

        // Fall-open arm: an untracked call never goes through the memo. Its
        // forced steps are applied in place on a private copy.
        let mut untracked = hard_services(&config, &policies);
        let value = untracked.evaluate_state_quiesced_at(None, &state);
        assert_eq!(value.to_bits(), first.to_bits());
        assert_eq!(untracked.memo_witness.successor_hits, 0);
        assert_eq!(untracked.memo_witness.successor_simulations, 0);
        assert_eq!(untracked.memo_witness.context_hits, 0);
        assert_eq!(untracked.memo_witness.peak_successor_edges, 0);
        assert_eq!(untracked.memo_witness.peak_retained_states, 0);
        assert_eq!(untracked.memo_witness.peak_node_contexts, 0);
        let builds = untracked.memo_witness.context_builds;
        assert!(matches!(
            untracked.quiesce_at(None, &state),
            QuiesceCursor::Owned(_)
        ));
        assert!(
            untracked.memo_witness.context_builds > builds,
            "an untracked revisit builds its contexts again"
        );
        assert_eq!(untracked.memo_witness.successor_simulations, 0);
    }

    /// T11 (seam): a forced step whose apply fails reports `Failed` and leaves
    /// the cursor holding the state exactly as an in-place apply leaves it,
    /// both on the first visit and when the failure is answered from the memo.
    #[test]
    fn quiesce_step_failed_apply_matches_an_in_place_apply() {
        let state = make_state_with_land();
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = hard_services(&config, &policies);
        let illegal = inapplicable_candidate().action;
        let mut in_place = state.clone();
        assert!(
            apply_as_current_for_simulation(&mut in_place, illegal.clone()).is_err(),
            "fixture: the action does not apply"
        );
        let node = services.new_search_root();

        for visit in 0..2u32 {
            let mut cursor = QuiesceCursor::Borrowed(&state);
            let step = services.quiesce_step(Some(node), &mut cursor, illegal.clone());
            assert!(matches!(step, QuiesceStep::Failed));
            assert!(matches!(cursor, QuiesceCursor::Owned(_)));
            assert!(*cursor.state() == in_place);
            assert_eq!(services.memo_witness.successor_simulations, 1);
            assert_eq!(services.memo_witness.successor_hits, visit);
        }

        // Reach-guard: a step that applies moves the cursor onto the shared
        // successor and reports the tracked child.
        let mut cursor = QuiesceCursor::Borrowed(&state);
        let step = services.quiesce_step(Some(node), &mut cursor, GameAction::PassPriority);
        assert!(matches!(step, QuiesceStep::Advanced(Some(_))));
        assert!(matches!(cursor, QuiesceCursor::Shared(_)));
    }

    /// T12: the hash-keyed `candidate_cache` is consulted before the exact
    /// per-position memo, exactly as before the memo existed. Reach-guard:
    /// without a hash entry the position's own context is returned and inserted.
    #[test]
    fn decision_context_lookup_prefers_the_hash_cache() {
        let state = make_state_with_land();
        let key = candidate_cache_key(&state);
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();

        let mut services = hard_services(&config, &policies);
        let node = services.new_search_root();
        let sentinel = Arc::new(build_decision_context(&state));
        let exact = Arc::new(build_decision_context(&state));
        services.candidate_cache.insert(key, Arc::clone(&sentinel));
        services.node_contexts.insert(node, Arc::clone(&exact));
        let got = services.build_decision_context_at(Some(node), &state);
        assert!(
            Arc::ptr_eq(&got, &sentinel),
            "the hash cache entry wins over the position's memo"
        );
        assert_eq!(services.memo_witness.context_hits, 0);
        assert_eq!(services.memo_witness.context_builds, 0);

        let mut guard = hard_services(&config, &policies);
        let node = guard.new_search_root();
        let exact = Arc::new(build_decision_context(&state));
        guard.node_contexts.insert(node, Arc::clone(&exact));
        let got = guard.build_decision_context_at(Some(node), &state);
        assert!(
            Arc::ptr_eq(&got, &exact),
            "reach-guard: the memo is consulted"
        );
        assert_eq!(guard.memo_witness.context_hits, 1);
        assert!(Arc::ptr_eq(
            guard.candidate_cache.get(&key).expect("inserted on miss"),
            &exact
        ));
    }

    /// T19: quiescence clones nothing when it takes no step, and a tracked
    /// revisit ends on the shared memoized successor without simulating.
    #[test]
    fn quiescence_cursor_clones_only_when_it_must() {
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();

        // A finished game is quiet before any step: quiescence takes none.
        let mut quiet = make_state_with_land();
        quiet.waiting_for = WaitingFor::GameOver { winner: None };
        let mut services = hard_services(&config, &policies);
        match services.quiesce_at(None, &quiet) {
            QuiesceCursor::Borrowed(borrowed) => assert!(std::ptr::eq(borrowed, &quiet)),
            QuiesceCursor::Shared(_) | QuiesceCursor::Owned(_) => {
                panic!("a quiet position must be returned by reference")
            }
        }
        let quiet_node = services.new_search_root();
        assert!(matches!(
            services.quiesce_at(Some(quiet_node), &quiet),
            QuiesceCursor::Borrowed(_)
        ));
        assert_eq!(services.memo_witness.successor_simulations, 0);

        let stacked = creature_spell_on_stack_state();
        let node = services.new_search_root();
        let first = services.quiesce_at(Some(node), &stacked).into_owned();
        let simulations = services.memo_witness.successor_simulations;
        assert!(simulations > 0, "reach-guard: the first visit takes a step");
        let revisit = services.quiesce_at(Some(node), &stacked);
        assert!(
            matches!(revisit, QuiesceCursor::Shared(_)),
            "a tracked revisit must end on the memoized successor"
        );
        assert!(*revisit.state() == first);
        assert_eq!(services.memo_witness.successor_simulations, simulations);
        assert!(
            first == services.quiesce(&stacked),
            "equals untracked quiescence"
        );
    }
}
