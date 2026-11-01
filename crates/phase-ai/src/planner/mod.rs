use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use engine::ai_support::{
    activates_mana_ability, build_decision_context, classify_payment_continuation,
    witness_payment_continuations, AiDecisionContext, CandidateAction,
    PaymentContinuationBatchStatus, PaymentContinuationState, PaymentContinuationStep,
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
    /// Whether the witnessed edge finalizes the payment root in its own step.
    pub payment_step: Option<PaymentContinuationStep>,
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
                payment_step: None,
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
                            payment_step: Some(accepted.step),
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
                                    payment_step: Some(accepted.step),
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
                                        payment_step: None,
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

/// Whether a leaf `state` is not yet quiet: something is on the stack, or a
/// spell or ability is being paid for. CR 602.2a creates an activated ability
/// on the stack as its activation begins, but the engine adds it to
/// `state.stack` only once its payment finalizes, so an activation paid at
/// `ManaPayment` has an empty stack and still has to be finished
/// (`finalizing_payment_action`; CR 602.2b applies CR 601.2b–i to it).
fn needs_quiescence(state: &GameState) -> bool {
    !state.stack.is_empty()
        || matches!(
            classify_payment_continuation(state),
            PaymentContinuationState::Affiliated(_)
        )
}

/// The action that finishes the payment in progress at `state` as it stands:
/// an action that activates no mana ability (at `ManaPayment`, the pass that
/// pays the total cost, CR 601.2h) and that the payment witness proves
/// finalizes the root in its own step. `None` when `state` is not a payment
/// the witness owns, or no such action is among `actions`.
///
/// CR 118.3c: activating mana abilities is never mandatory, so finishing the
/// payment now is always one way to continue it. Quiescence and rollouts use it
/// to value a position in the middle of a payment as that payment completed:
/// nobody gets priority while a spell or ability is being paid for (CR 601.2,
/// CR 602.2b), so such a position is not a quiet leaf. Further activations stay
/// selectable as search edges (see `search_value_at` for their depth).
fn finalizing_payment_action(state: &GameState, actions: &[GameAction]) -> Option<GameAction> {
    if !matches!(
        classify_payment_continuation(state),
        PaymentContinuationState::Affiliated(_)
    ) {
        return None;
    }
    let finishers: Vec<_> = actions
        .iter()
        .filter(|action| {
            !matches!(action, GameAction::CancelCast) && !activates_mana_ability(state, action)
        })
        .cloned()
        .collect();
    if finishers.is_empty() {
        return None;
    }
    witness_payment_continuations(state, &finishers)
        .successors
        .into_iter()
        .flatten()
        .find(|accepted| match accepted.step {
            PaymentContinuationStep::Finalizes => true,
            PaymentContinuationStep::Continues => false,
        })
        .map(|accepted| accepted.action)
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
    /// How `candidate_is_valid` validates a prompt answer for this decision.
    /// `EachCandidate` unless the caller takes on what `AtSubmission` leaves
    /// to it; the root scorer sets it from its own caller.
    pub(crate) prompt_answers: PromptAnswerValidation,
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
            prompt_answers: PromptAnswerValidation::EachCandidate,
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
            .filter(|candidate| Self::candidate_is_valid(state, candidate, self.prompt_answers))
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
                    || Self::candidate_is_valid(state, &candidate.candidate, self.prompt_answers)
            })
            .collect()
    }

    fn candidate_is_valid(
        state: &GameState,
        candidate: &CandidateAction,
        prompt_answers: PromptAnswerValidation,
    ) -> bool {
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
            // CR 608.2d + CR 614.12a + CR 701.23a: a named-choice answer from the prompt's
            // enumerated domain, or a library search selection, is admitted by
            // its submission guard without mutating the game; the engine decides that
            // through the same authority `SimulationFilter` consults before its
            // probe. Under `PromptAnswerValidation::AtSubmission` no state copy is
            // made here: the root search simulates the answers in its beam until one fails, and
            // the caller the one answer it submits. A shape
            // the authority declines (a scoped search, a card name, a free-entry
            // number, an answer given while a cast or activation is in progress),
            // and every answer under `EachCandidate`, is simulated below.
            engine::types::actions::GameAction::ChooseOption { .. }
            | engine::types::actions::GameAction::SelectCards { .. }
                if prompt_answers.admits_without_simulation(state, &candidate.action) =>
            {
                true
            }
            _ => Self::simulation_accepts(state, &candidate.action),
        }
    }

    /// The legality gate's simulation of `action` at `state`: one state copy,
    /// counted by `perf_counters::record_state_clone_for_legality`, then
    /// `apply_as_current_for_simulation`. It is also the check
    /// [`PromptAnswerValidation::AtSubmission`] leaves to the caller for the
    /// answer it submits.
    pub(crate) fn simulation_accepts(state: &GameState, action: &GameAction) -> bool {
        engine::game::perf_counters::record_state_clone_for_legality();
        let mut sim = state.clone();
        apply_as_current_for_simulation(&mut sim, action.clone()).is_ok()
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

            // Case 4: a payment in progress is not a quiet position; finish it
            // as it stands (see `finalizing_payment_action`).
            if let Some(action) = finalizing_payment_action(cursor.state(), &actions) {
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
        if !needs_quiescence(state) || self.deadline.expired() {
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
        if !needs_quiescence(state) || self.deadline.expired() {
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

        // CR 601.2: a payment in progress and the cast it completes are one
        // move; the rollout finishes the payment as it stands rather than
        // spending its depth on mana activations (see `finalizing_payment_action`).
        if matches!(
            classify_payment_continuation(state),
            PaymentContinuationState::Affiliated(_)
        ) {
            let actions: Vec<_> = self
                .build_decision_context_at(node, state)
                .candidates
                .iter()
                .map(|candidate| candidate.action.clone())
                .collect();
            if let Some(action) = finalizing_payment_action(state, &actions) {
                let mut finished = state.clone();
                if apply_as_current_for_simulation(&mut finished, action).is_ok() {
                    return self.rollout_estimate_at(None, &finished, depth);
                }
            }
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

/// How the planner legality gate (`PlannerServices::candidate_is_valid`)
/// validates a prompt answer, a named-choice answer or a library search
/// selection, that `engine::ai_support::structurally_valid_prompt_answer`
/// admits. Every other candidate is validated the same way under both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptAnswerValidation {
    /// Simulate each answer on a state copy, as the gate's catch-all does. The
    /// gate's verdict is final.
    EachCandidate,
    /// Admit the answer without a state copy. The root search simulates such
    /// answers in its beam on its first rung, in beam order, and returns the
    /// first that fails (`search::FailedAdmittedAnswer`) instead of scores,
    /// simulating no answer after it, and the root scorer scores the decision
    /// again under `EachCandidate`. The caller must simulate
    /// the one answer it submits ([`PlannerServices::simulation_accepts`]) and,
    /// if that simulation fails, decide again under `EachCandidate`. Only the
    /// canonical chooser (`search::choose_action_with_session_inner`) does
    /// both.
    AtSubmission,
}

impl PromptAnswerValidation {
    /// Whether the gate admits `action` at `state` without a state copy.
    pub(crate) fn admits_without_simulation(self, state: &GameState, action: &GameAction) -> bool {
        match self {
            Self::EachCandidate => false,
            Self::AtSubmission => {
                engine::ai_support::structurally_valid_prompt_answer(state, action)
            }
        }
    }

    /// The validation the canonical chooser decides under first:
    /// `AtSubmission`, unless `config` scores the decision in determinized
    /// samples (`determinization_samples > 0`). Each sample simulates every
    /// answer itself (`search::score_candidates_core`) on a world that differs
    /// from the decision state only in opponents' hidden information, and the
    /// chooser keeps deciding from those simulations, as it did before
    /// `AtSubmission` existed.
    pub(crate) fn for_chooser(config: &AiConfig) -> Self {
        if config.search.determinization_samples > 0 || chooser_simulates_each_prompt_answer() {
            Self::EachCandidate
        } else {
            Self::AtSubmission
        }
    }
}

/// Whether this thread's chooser is switched to `EachCandidate` for a test's
/// reference decision.
#[cfg(test)]
#[inline]
fn chooser_simulates_each_prompt_answer() -> bool {
    CHOOSER_SIMULATES_EACH_PROMPT_ANSWER.get()
}

/// Production variant: the switch does not exist.
#[cfg(not(test))]
#[inline]
fn chooser_simulates_each_prompt_answer() -> bool {
    false
}

// Test-only switch for the chooser's first validation, so a production-path
// test can run, as its reference, the decision the gate made before
// `PromptAnswerValidation::AtSubmission` existed: every prompt answer simulated,
// nothing checked after selection. The switch does not exist outside
// `cfg(test)`. Thread-local, as `layers.rs`'s `REBUILD_STATIC_INDEX_AT_TOP`: the
// decision runs on the test's own thread, and every other test reads `false`.
#[cfg(test)]
thread_local! {
    static CHOOSER_SIMULATES_EACH_PROMPT_ANSWER: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

/// While alive, the chooser on this thread decides under
/// `PromptAnswerValidation::EachCandidate` from the start. Dropping it restores
/// the previous setting, on unwind too, as `speed.rs`'s `SpeedCapGuard` does.
#[cfg(test)]
struct ChooserSimulatesEachPromptAnswer {
    previous: bool,
}

#[cfg(test)]
impl ChooserSimulatesEachPromptAnswer {
    fn enter() -> Self {
        Self {
            previous: CHOOSER_SIMULATES_EACH_PROMPT_ANSWER.replace(true),
        }
    }
}

#[cfg(test)]
impl Drop for ChooserSimulatesEachPromptAnswer {
    fn drop(&mut self) {
        CHOOSER_SIMULATES_EACH_PROMPT_ANSWER.set(self.previous);
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
        // CR 601.2: nobody receives priority while a spell or ability is being
        // paid for, so a payment step and the cast it completes are one move.
        // Charging a ply for continuing a payment that the pass could finish
        // let another mana activation (and its life, card or permanent) push
        // the opponent's reply to the cast past the horizon (#10488). So a step
        // inside a payment costs no depth, with one exception: at the payment
        // window itself, while no edge that activates no mana ability can
        // finish it yet, every option continues the payment and is charged
        // alike. Prompts nested in a mana ability's activation (its cost, its
        // color) belong to that one activation (CR 605.3a). The node budget
        // bounds the payment chain.
        let payment_can_finish_now = candidates.iter().any(|prepared| {
            prepared.payment_step == Some(PaymentContinuationStep::Finalizes)
                && !activates_mana_ability(state, &prepared.candidate.action)
        });
        let free_payment_step = match classify_payment_continuation(state) {
            PaymentContinuationState::Affiliated(_) => {
                payment_can_finish_now
                    || !matches!(state.waiting_for, WaitingFor::ManaPayment { .. })
            }
            PaymentContinuationState::NotAffiliated
            | PaymentContinuationState::UnsupportedAffiliated(_) => false,
        };
        let child_depth = if free_payment_step { depth } else { depth - 1 };
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
                child_depth,
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
                payment_step: None,
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
pub(crate) mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::combat::BlockRequirement;
    use engine::game::engine::EngineError;
    use engine::game::perf_counters;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::turn_control;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ChoiceType, Comparator, Effect,
        ManaContribution, ManaProduction, NumberDistinctness, QuantityExpr, ResolvedAbility,
        SearchSelectionConstraint, TargetFilter, TargetRef,
    };
    use engine::types::actions::{GameAction, MulliganChoice};
    use engine::types::card_type::{CoreType, Supertype};
    use engine::types::counter::CounterType;
    use engine::types::game_state::{
        ActiveSearchDecisionAuthority, CastPaymentMode, CommanderDamageEntry,
        NamedChoiceSourceBinding, PendingContinuation, PendingScopedLibrarySearch,
        ScopedLibrarySearchPhase, StackEntry, StackEntryKind, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::WardCost;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::zones::Zone;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use std::collections::{HashMap, HashSet};
    use std::ops::ControlFlow;

    use crate::auto_play::run_ai_action;
    use crate::config::{create_config, create_config_for_players, AiDifficulty, Platform};
    use crate::policies::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
    use crate::session::AiSession;

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

    /// Bonesplitter's equip, paid by Treasonous Ogre's "Pay 3 life: Add {R}."
    /// at its `ManaPayment` window (a Manual activation reaches that window, as
    /// in `activation_manual_mana_payment.rs`). Oracle text verbatim from
    /// Scryfall `cards/named`.
    fn equip_paid_by_ogre_mid_payment() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        let ogre = scenario
            .add_creature(P0, "Treasonous Ogre", 2, 3)
            .from_oracle_text_with_keywords(
                &["Dethrone"],
                "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\nPay 3 life: Add {R}.",
            )
            .id();
        let bonesplitter = scenario
            .add_artifact_from_oracle(
                P0,
                "Bonesplitter",
                "Equipped creature gets +2/+0.\nEquip {1}",
            )
            .with_subtypes(vec!["Equipment"])
            .id();
        let mut runner = scenario.build();
        let equip_index = runner.state().objects[&bonesplitter]
            .abilities
            .iter()
            .position(|ability| ability.kind == AbilityKind::Activated)
            .expect("Bonesplitter has its equip ability");
        runner
            .act(GameAction::ActivateAbility {
                source_id: bonesplitter,
                ability_index: equip_index,
                payment_mode: CastPaymentMode::Manual,
            })
            .expect("the equip activation begins");
        if matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ) {
            runner
                .act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(bears)),
                })
                .expect("Grizzly Bears is a legal equip target");
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ManaPayment { .. }),
            "reach: the Manual equip waits for its payment, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::ActivateAbility {
                source_id: ogre,
                ability_index: 0,
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("the Ogre pays 3 life for {R} while the equip is paid for");
        runner.state().clone()
    }

    /// CR 602.2b + CR 601.2h: an activation paid at `ManaPayment` has nothing on
    /// the stack yet, and a leaf there is valued as its payment finished, not
    /// with the life paid and the ability missing (#10488).
    #[test]
    fn activation_payment_leaf_is_valued_as_its_finished_payment() {
        let mid = equip_paid_by_ogre_mid_payment();
        assert!(mid.stack.is_empty(), "reach: nothing is on the stack yet");
        assert_eq!(mid.players[0].life, 17, "reach: the Ogre paid 3 life");
        assert!(matches!(
            classify_payment_continuation(&mid),
            PaymentContinuationState::Affiliated(
                engine::ai_support::PaymentContinuationRoot::Activation { .. }
            )
        ));
        let mut finished = mid.clone();
        apply_as_current_for_simulation(&mut finished, GameAction::PassPriority)
            .expect("the floating {R} pays Equip {1}");
        assert_eq!(
            finished.stack.len(),
            1,
            "reach: the equip ability is on the stack"
        );

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::with_deadline(
            PlayerId(0),
            &config,
            &policies,
            crate::context::AiContext::empty(&config.weights),
            Some(engine::util::Deadline::none()),
        );
        let finished_value = services.evaluate_state_quiesced(&finished);
        assert_ne!(
            services.evaluate_state_cached(&mid),
            finished_value,
            "reach: valuing the payment unfinished differs from finishing it"
        );
        assert_eq!(
            services.evaluate_state_quiesced(&mid),
            finished_value,
            "the search leaf finishes the activation's payment"
        );
        assert_eq!(
            services.quiesced_leaf_eval(&mid),
            services.quiesced_leaf_eval(&finished),
            "the rollout leaf finishes the activation's payment"
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

    // ------------------------------------------------------------------
    // Prompt-answer legality gate: `candidate_is_valid` admits a named-choice
    // answer or a library search selection through
    // `engine::ai_support::structurally_valid_prompt_answer` instead of a state
    // copy + simulation, and only at the `NamedChoice` / `SearchChoice`
    // windows. Every base board is reached through the real cast/play pipeline
    // from the card's verbatim Oracle text.
    // ------------------------------------------------------------------

    const DISTANT_MELODY_ORACLE: &str =
        "Choose a creature type. Draw a card for each permanent you control of that type.";
    const BRAVE_THE_ELEMENTS_ORACLE: &str = "Choose a color. White creatures you control gain protection from the chosen color until end of turn.";
    const THRIVING_GROVE_ORACLE: &str = "This land enters tapped. As it enters, choose a color other than green.\n{T}: Add {G} or one mana of the chosen color.";
    const DEMONIC_TUTOR_ORACLE: &str =
        "Search your library for a card, put that card into your hand, then shuffle.";
    const CELESTIAL_REUNION_ORACLE: &str = "As an additional cost to cast this spell, you may choose a creature type and behold two creatures of that type.\nSearch your library for a creature card with mana value X or less, reveal it, put it into your hand, then shuffle. If this spell's additional cost was paid and the revealed card is the chosen type, put that card onto the battlefield instead of putting it into your hand.";
    const CULTIVATE_ORACLE: &str = "Search your library for up to two basic land cards, reveal those cards, put one onto the battlefield tapped and the other into your hand, then shuffle.";

    /// Cast `spell` for P0 (the active player, holding priority) through the
    /// full cast pipeline and return the state parked at its first prompt; the
    /// runner stays parked there too.
    fn resolve_to_prompt(runner: &mut GameRunner, spell: ObjectId) -> GameState {
        runner.state_mut().active_player = P0;
        runner.state_mut().priority_player = P0;
        runner.cast(spell).resolve().state().clone()
    }

    /// Distant Melody cast and resolved up to its CR 608.2d creature-type
    /// prompt. The answer persists on the resolving spell, so the prompt is
    /// `ExactObjectAndResolution`-bound.
    fn distant_melody_prompt(mut scenario: GameScenario) -> GameState {
        scenario.at_phase(Phase::PreCombatMain);
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Distant Melody",
            false,
            DISTANT_MELODY_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Blue],
        });
        let melody = builder.id();
        let mut pool: Vec<ManaUnit> = (0..3)
            .map(|_| ManaUnit::new(ManaType::Colorless, melody, false, vec![]))
            .collect();
        pool.push(ManaUnit::new(ManaType::Blue, melody, false, vec![]));
        scenario.with_mana_pool(P0, pool);
        let state = resolve_to_prompt(&mut scenario.build(), melody);
        match &state.waiting_for {
            WaitingFor::NamedChoice {
                player,
                choice_type: ChoiceType::CreatureType { .. },
                options,
                source: Some(source),
                ..
            } => {
                assert_eq!(*player, P0, "P0 resolves Distant Melody");
                assert!(
                    options.len() > 1,
                    "the creature-type prompt must enumerate several answers"
                );
                assert_eq!(
                    source.binding,
                    NamedChoiceSourceBinding::ExactObjectAndResolution
                );
            }
            other => panic!("Distant Melody must halt on its creature-type prompt, got {other:?}"),
        }
        assert!(
            !state.withholds_priority(),
            "the prompt is answered while the spell resolves, not mid-cast"
        );
        state
    }

    /// Brave the Elements cast and resolved up to its CR 608.2d color prompt.
    /// Its choice does not persist, so the prompt carries no source.
    fn brave_the_elements_prompt() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Brave the Elements",
            true,
            BRAVE_THE_ELEMENTS_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            generic: 0,
            shards: vec![ManaCostShard::White],
        });
        let brave = builder.id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::White, brave, false, vec![])],
        );
        let state = resolve_to_prompt(&mut scenario.build(), brave);
        assert!(
            matches!(
                &state.waiting_for,
                WaitingFor::NamedChoice {
                    player,
                    choice_type: ChoiceType::Color { .. },
                    source: None,
                    ..
                } if *player == P0
            ),
            "Brave the Elements must halt on its source-less color prompt, got {:?}",
            state.waiting_for
        );
        assert!(!state.withholds_priority());
        state
    }

    /// Thriving Grove played up to its CR 614.12a pre-entry color prompt: a
    /// `PendingEntry`-bound source whose parked entry is live.
    fn thriving_grove_prompt() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let grove = scenario
            .add_land_to_hand(P0, "Thriving Grove")
            .from_oracle_text(THRIVING_GROVE_ORACLE)
            .id();
        let mut runner = scenario.build();
        runner.state_mut().active_player = P0;
        runner.state_mut().priority_player = P0;
        let card_id = runner.state().objects[&grove].card_id;
        runner
            .act(GameAction::PlayLand {
                object_id: grove,
                card_id,
            })
            .expect("playing the land must start its entry");
        let state = runner.state().clone();
        assert!(
            matches!(
                &state.waiting_for,
                WaitingFor::NamedChoice {
                    player,
                    choice_type: ChoiceType::Color { .. },
                    source: Some(source),
                    ..
                } if *player == P0
                    && source.binding == NamedChoiceSourceBinding::PendingEntry
                    && source.has_matching_context()
            ),
            "Thriving Grove must halt on its pre-entry color prompt, got {:?}",
            state.waiting_for
        );
        assert!(
            state.pending_replacement.is_some(),
            "the entry is parked behind the choice"
        );
        assert!(!state.withholds_priority());
        state
    }

    /// Demonic Tutor cast and resolved up to its CR 701.23a/d quantity search:
    /// an ordinary (non-scoped) search over a four-card library that must find
    /// exactly one card.
    fn demonic_tutor_prompt(mut scenario: GameScenario) -> GameState {
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Demonic Tutor",
            false,
            DEMONIC_TUTOR_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        });
        let tutor = builder.id();
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Colorless, tutor, false, vec![]),
                ManaUnit::new(ManaType::Black, tutor, false, vec![]),
            ],
        );
        let state = resolve_to_prompt(&mut scenario.build(), tutor);
        let WaitingFor::SearchChoice {
            player,
            cards,
            count,
            up_to,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "Demonic Tutor must halt on its library search, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*player, P0, "P0 searches");
        assert_eq!(*count, 1, "\"a card\" is a quantity search for one card");
        assert!(!*up_to, "the search must find exactly one card");
        let mut pool = cards.clone();
        pool.sort();
        let mut library: Vec<ObjectId> = state
            .players
            .iter()
            .find(|seat| seat.id == P0)
            .expect("P0 exists")
            .library
            .iter()
            .copied()
            .collect();
        library.sort();
        assert_eq!(pool.len(), 4, "the pool is the four-card library");
        assert_eq!(pool, library, "the pool is exactly P0's library");
        assert!(
            state.pending_scoped_library_search.is_none(),
            "Demonic Tutor's search is an ordinary (non-scoped) search"
        );
        state
    }

    /// Celestial Reunion cast with its optional additional cost paid, up to the
    /// CR 601.2b cost-time creature-type prompt (`CostTypeChoice`, not a
    /// `NamedChoice`): P0 holds two Elf creature cards, so only "Elf" can be
    /// beheld twice (CR 701.4a).
    fn celestial_reunion_cost_type_prompt() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        for name in ["Elf A", "Elf B"] {
            scenario
                .add_creature_to_hand(P0, name, 1, 1)
                .with_subtypes(vec!["Elf"]);
        }
        let mut builder = scenario.add_spell_to_hand_from_oracle(
            P0,
            "Celestial Reunion",
            false,
            CELESTIAL_REUNION_ORACLE,
        );
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Green],
            generic: 0,
        });
        let reunion = builder.id();
        scenario.with_mana_pool(
            P0,
            (0..4)
                .map(|_| ManaUnit::new(ManaType::Green, reunion, false, vec![]))
                .collect(),
        );
        let mut runner = scenario.build();
        runner.state_mut().all_creature_types = vec!["Elf".to_string(), "Goblin".to_string()];
        runner.state_mut().active_player = P0;
        runner.state_mut().priority_player = P0;
        let card_id = runner.state().objects[&reunion].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: reunion,
                card_id,
                targets: vec![],
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("casting Celestial Reunion must be accepted");
        assert!(
            matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalCostChoice { .. }
            ),
            "the caster must be asked whether to pay the optional additional cost, got {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::DecideOptionalCost { pay: true })
            .expect("paying the behold cost must be accepted");
        let state = runner.state().clone();
        match &state.waiting_for {
            WaitingFor::CostTypeChoice {
                player,
                choice_type,
                options,
                ..
            } => {
                assert_eq!(*player, P0, "P0 announces the cost");
                assert!(matches!(choice_type, ChoiceType::CreatureType { .. }));
                assert_eq!(*options, ["Elf"], "only Elf can be beheld");
            }
            other => panic!("Celestial Reunion must halt on its cost-type prompt, got {other:?}"),
        }
        assert!(
            state.withholds_priority(),
            "the cost-type prompt is answered mid-cast"
        );
        state
    }

    /// Cultivate cast and resolved through its library search (both basics
    /// found) up to the split-destination partition prompt
    /// (`SearchPartitionChoice`, answered with `SelectCards` but not a
    /// `SearchChoice`): CR 701.23a + CR 608.2d.
    fn cultivate_partition_prompt() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let forest = scenario
            .add_land_to_library_top(P0, "Forest")
            .with_subtypes(vec!["Forest"])
            .id();
        let mountain = scenario
            .add_land_to_library_top(P0, "Mountain")
            .with_subtypes(vec!["Mountain"])
            .id();
        let mut builder =
            scenario.add_spell_to_hand_from_oracle(P0, "Cultivate", false, CULTIVATE_ORACLE);
        builder.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green],
            generic: 2,
        });
        let cultivate = builder.id();
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(ManaType::Green, cultivate, false, vec![]),
                ManaUnit::new(ManaType::Colorless, cultivate, false, vec![]),
                ManaUnit::new(ManaType::Colorless, cultivate, false, vec![]),
            ],
        );
        let mut runner = scenario.build();
        // Both library lands are basic (the scenario builder offers no
        // supertype setter, and its state is private outside the engine).
        for land in [forest, mountain] {
            let obj = runner
                .state_mut()
                .objects
                .get_mut(&land)
                .expect("the land was staged");
            obj.card_types.supertypes.push(Supertype::Basic);
            obj.base_card_types = obj.card_types.clone();
        }
        let searching = resolve_to_prompt(&mut runner, cultivate);
        match &searching.waiting_for {
            WaitingFor::SearchChoice {
                player,
                cards,
                count,
                up_to,
                ..
            } => {
                assert_eq!(*player, P0, "P0 searches");
                assert_eq!(*count, 2);
                assert!(*up_to, "\"up to two\"");
                assert!(cards.contains(&forest) && cards.contains(&mountain));
            }
            other => panic!("Cultivate must halt on its library search, got {other:?}"),
        }
        runner
            .act(GameAction::SelectCards {
                cards: vec![forest, mountain],
            })
            .expect("finding both basics must park the partition choice");
        let state = runner.state().clone();
        match &state.waiting_for {
            WaitingFor::SearchPartitionChoice {
                player,
                cards,
                primary_destination,
                primary_count,
                rest_destination,
                ..
            } => {
                assert_eq!(*player, P0, "P0 partitions the found cards");
                assert_eq!(*primary_count, 1);
                assert_eq!(*primary_destination, Zone::Battlefield);
                assert_eq!(*rest_destination, Zone::Hand);
                let mut found = cards.clone();
                found.sort();
                let mut expected = [forest, mountain];
                expected.sort();
                assert_eq!(found, expected, "the found set is both basics");
            }
            other => panic!("Cultivate must halt on its partition prompt, got {other:?}"),
        }
        assert!(state.pending_scoped_library_search.is_none());
        state
    }

    fn choose(choice: &str) -> GameAction {
        GameAction::ChooseOption {
            choice: choice.to_string(),
        }
    }

    fn select(cards: &[ObjectId]) -> GameAction {
        GameAction::SelectCards {
            cards: cards.to_vec(),
        }
    }

    fn named_options(state: &GameState) -> Vec<String> {
        match &state.waiting_for {
            WaitingFor::NamedChoice { options, .. } => options.clone(),
            other => panic!("expected a NamedChoice prompt, got {other:?}"),
        }
    }

    fn search_pool(state: &GameState) -> Vec<ObjectId> {
        match &state.waiting_for {
            WaitingFor::SearchChoice { cards, .. } => cards.clone(),
            other => panic!("expected a SearchChoice prompt, got {other:?}"),
        }
    }

    fn object_named(state: &GameState, name: &str) -> ObjectId {
        state
            .objects
            .values()
            .find(|object| object.name == name)
            .map(|object| object.id)
            .unwrap_or_else(|| panic!("no object named {name}"))
    }

    /// The same board with a scoped ("each player searches") library search
    /// pending, in the acceptance phase — the shape the structural authority
    /// defers to the simulation.
    fn with_scoped_search_flag(state: &GameState) -> GameState {
        let mut scoped = state.clone();
        let tutor = object_named(state, "Demonic Tutor");
        scoped.pending_scoped_library_search = Some(PendingScopedLibrarySearch {
            ability: Box::new(ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                tutor,
                P0,
            )),
            phase: ScopedLibrarySearchPhase::CollectAcceptance {
                remaining_players: Vec::new(),
                accepted_players: Vec::new(),
                acceptance_authorities: Vec::new(),
                current_player: None,
            },
            after_scope: None,
        });
        scoped
    }

    /// The legality gate's simulation arm verbatim: one state copy,
    /// `apply_as_current_for_simulation`.
    fn planner_simulation_accepts(state: &GameState, action: &GameAction) -> bool {
        let mut sim = state.clone();
        apply_as_current_for_simulation(&mut sim, action.clone()).is_ok()
    }

    /// `PlannerServices::candidate_is_valid` on `action`: its verdict and the
    /// legality state copies it recorded.
    fn gate_verdict(state: &GameState, action: &GameAction) -> (bool, u64) {
        let candidate = CandidateAction {
            action: action.clone(),
            metadata: ActionMetadata::for_actor(
                state.waiting_for.acting_player(),
                TacticalClass::Selection,
            ),
        };
        perf_counters::reset();
        let verdict = PlannerServices::candidate_is_valid(
            state,
            &candidate,
            PromptAnswerValidation::AtSubmission,
        );
        (verdict, perf_counters::snapshot().state_clone_for_legality)
    }

    /// How the legality gate must decide an answer.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Route {
        /// Admitted by `structurally_valid_prompt_answer`: no state copy.
        Structural,
        /// Declined by the authority and simulated: at least one state copy.
        Simulated,
    }

    /// One agreement row: an answer on a board, the route the gate must take,
    /// and the verdict derived from the card text and the rules.
    struct GateRow {
        label: String,
        state: GameState,
        action: GameAction,
        route: Route,
        derived: bool,
        /// CR 723.5: the submitter a multi-authority board must derive.
        submitter: Option<PlayerId>,
    }

    fn gate_row(
        label: impl Into<String>,
        state: &GameState,
        action: GameAction,
        route: Route,
        derived: bool,
    ) -> GateRow {
        GateRow {
            label: label.into(),
            state: state.clone(),
            action,
            route,
            derived,
            submitter: None,
        }
    }

    /// T1. Every creature-type answer at Distant Melody's resolution prompt
    /// passes the planner legality gate, composed as production composes it
    /// (`score_candidates_core_for_root`: decision context →
    /// `prepare_payment_candidates` → `validate_prepared_candidates`), without
    /// a state copy, and every answer is retained.
    ///
    /// Fails on revert of the `candidate_is_valid` prompt-answer arm: each
    /// answer is then simulated (one legality clone per answer).
    #[test]
    fn named_choice_answers_pass_the_legality_gate_without_a_state_copy() {
        let state = distant_melody_prompt(GameScenario::new());
        let options = named_options(&state);
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(P0, &config, &policies);
        // The validation the chooser's root scoring decides under.
        services.prompt_answers = PromptAnswerValidation::AtSubmission;
        let ctx = services.build_decision_context(&state);
        let prepared = prepare_payment_candidates(&state, ctx.candidates.clone());

        // Reach guard: one `ChooseOption` per option, and none carries a
        // payment successor (which would bypass `candidate_is_valid`).
        assert_eq!(
            prepared.len(),
            options.len(),
            "one prepared candidate per option"
        );
        for entry in &prepared {
            assert!(
                matches!(entry.candidate.action, GameAction::ChooseOption { .. }),
                "unexpected candidate {:?}",
                entry.candidate.action
            );
            assert!(entry.payment_successor.is_none());
        }

        perf_counters::reset();
        let validated = services.validate_prepared_candidates(&state, prepared.clone());
        let clones = perf_counters::snapshot().state_clone_for_legality;
        assert_eq!(
            clones, 0,
            "named-choice answers must pass the legality gate without a state copy"
        );

        let issued: Vec<GameAction> = prepared
            .iter()
            .map(|entry| entry.candidate.action.clone())
            .collect();
        let retained: Vec<GameAction> = validated
            .iter()
            .map(|entry| entry.candidate.action.clone())
            .collect();
        assert_eq!(
            retained, issued,
            "the gate must retain every answer, in order"
        );
        for action in &issued {
            assert!(
                planner_simulation_accepts(&state, action),
                "the planner's simulation accepts {action:?}"
            );
        }

        // Paired reach guard: on the same board an answer outside the domain is
        // declined by the authority and simulated, so the counter is live.
        let (verdict, clones) = gate_verdict(&state, &choose("Not A Creature Type"));
        assert!(!verdict, "a non-creature-type answer is illegal");
        assert!(
            clones >= 1,
            "an answer the authority declines must be simulated"
        );
    }

    /// T2. Every selection at Demonic Tutor's library search passes the
    /// planner legality gate, composed as production composes it, without a
    /// state copy, and every selection is retained.
    ///
    /// Fails on revert of the `candidate_is_valid` prompt-answer arm: each
    /// selection is then simulated (one legality clone per selection).
    #[test]
    fn search_selections_pass_the_legality_gate_without_a_state_copy() {
        let state = demonic_tutor_prompt(GameScenario::new());
        let pool = search_pool(&state);
        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(P0, &config, &policies);
        // The validation the chooser's root scoring decides under.
        services.prompt_answers = PromptAnswerValidation::AtSubmission;
        let ctx = services.build_decision_context(&state);
        let prepared = prepare_payment_candidates(&state, ctx.candidates.clone());

        // Reach guard: one `SelectCards` per library card, and none carries a
        // payment successor (which would bypass `candidate_is_valid`).
        assert_eq!(prepared.len(), pool.len(), "one selection per library card");
        for entry in &prepared {
            assert!(
                matches!(entry.candidate.action, GameAction::SelectCards { .. }),
                "unexpected candidate {:?}",
                entry.candidate.action
            );
            assert!(entry.payment_successor.is_none());
        }

        perf_counters::reset();
        let validated = services.validate_prepared_candidates(&state, prepared.clone());
        let clones = perf_counters::snapshot().state_clone_for_legality;
        assert_eq!(
            clones, 0,
            "search selections must pass the legality gate without a state copy"
        );

        let issued: Vec<GameAction> = prepared
            .iter()
            .map(|entry| entry.candidate.action.clone())
            .collect();
        let retained: Vec<GameAction> = validated
            .iter()
            .map(|entry| entry.candidate.action.clone())
            .collect();
        assert_eq!(
            retained, issued,
            "the gate must retain every selection, in order"
        );
        for action in &issued {
            assert!(
                planner_simulation_accepts(&state, action),
                "the planner's simulation accepts {action:?}"
            );
        }

        // Paired reach guard: the same pick on a scoped search is deferred by
        // the authority and simulated, and keeps the simulation's verdict.
        let scoped = with_scoped_search_flag(&state);
        let pick = select(&pool[..1]);
        let (verdict, clones) = gate_verdict(&scoped, &pick);
        assert!(clones >= 1, "a scoped search selection must be simulated");
        assert_eq!(verdict, planner_simulation_accepts(&scoped, &pick));
        assert!(verdict, "the ordinary guard accepts a legal single pick");
    }

    /// The score-only route (`search::score_candidates`; engine-wasm's pool
    /// workers reach the same scorer through
    /// `score_candidates_for_parallel_worker`) hands its scores to a selection
    /// made elsewhere, so nothing on it simulates the answer that is finally
    /// submitted: its legality gate simulates every offered answer to Distant
    /// Melody's creature-type prompt, one state copy each.
    ///
    /// Fails if that route admits prompt answers without a state copy, as the
    /// chooser's root scoring does.
    #[test]
    fn score_only_route_simulates_every_prompt_answer() {
        let state = distant_melody_prompt(GameScenario::new());
        let offered = named_options(&state).len() as u64;
        let config = create_config(AiDifficulty::Medium, Platform::Native).into_measurement(0);
        perf_counters::reset();
        let scored = crate::search::score_candidates(&state, P0, &config);
        let clones = perf_counters::snapshot().state_clone_for_legality;
        assert!(!scored.is_empty(), "reach guard: the prompt is scored");
        assert!(
            scored
                .iter()
                .all(|(action, _)| matches!(action, GameAction::ChooseOption { .. })),
            "reach guard: the prompt's answers are scored, got {scored:?}"
        );
        assert_eq!(
            clones, offered,
            "the score-only route must simulate each offered answer once"
        );
    }

    /// T3. The prompt-answer arm agrees with the planner's own simulation on
    /// every row: answers the engine authority admits (including CR 723.5
    /// controlled-player and latched-search-controller boards, where someone
    /// other than the prompted player submits) and hostile shapes it must
    /// decline. Each row's simulation verdict also equals the reading derived
    /// from the card text and the rules, or, on a board a row mutates after the
    /// pipeline, from that mutated prompt.
    ///
    /// The scope rows answer `ChooseOption` at Celestial Reunion's
    /// `CostTypeChoice` and `SelectCards` at Cultivate's
    /// `SearchPartitionChoice` — windows other than `NamedChoice` /
    /// `SearchChoice`. They fail if `structurally_valid_prompt_answer`'s window
    /// set widens, because the planner would then admit them without a state
    /// copy.
    #[test]
    fn prompt_answer_gate_agrees_with_the_planner_simulation() {
        use Route::{Simulated, Structural};

        let mut rows: Vec<GateRow> = Vec::new();

        // CR 608.2d + CR 205.3m: every offered creature type is a legal answer.
        let melody = distant_melody_prompt(GameScenario::new());
        let melody_options = named_options(&melody);
        for option in &melody_options {
            rows.push(gate_row(
                format!("melody/each {option}"),
                &melody,
                choose(option),
                Structural,
                true,
            ));
        }

        let brave = brave_the_elements_prompt();
        for option in named_options(&brave) {
            rows.push(gate_row(
                format!("brave/each {option}"),
                &brave,
                choose(&option),
                Structural,
                true,
            ));
        }

        // CR 614.12a: every offered color answers the parked entry.
        let grove = thriving_grove_prompt();
        let grove_options = named_options(&grove);
        for option in &grove_options {
            rows.push(gate_row(
                format!("grove/each {option}"),
                &grove,
                choose(option),
                Structural,
                true,
            ));
        }

        // CR 701.23a + CR 701.23d: each single library card is a legal find.
        let tutor = demonic_tutor_prompt(GameScenario::new());
        let pool = search_pool(&tutor);
        for &card in &pool {
            rows.push(gate_row(
                format!("tutor/each {card:?}"),
                &tutor,
                select(&[card]),
                Structural,
                true,
            ));
        }

        // Synthetic board (not card-derived): Demonic Tutor's quantity search
        // must find one card (CR 701.23d); with the prompt's up_to set, as an
        // "up to N" search's is, the guard accepts finding none.
        let mut up_to_board = tutor.clone();
        let WaitingFor::SearchChoice { up_to, .. } = &mut up_to_board.waiting_for else {
            unreachable!("the fixture asserts the search prompt");
        };
        *up_to = true;
        rows.push(gate_row(
            "tutor/up-to-empty",
            &up_to_board,
            select(&[]),
            Structural,
            true,
        ));

        // CR 701.23a: two distinct cards found by a two-card search.
        let mut count_two = tutor.clone();
        let WaitingFor::SearchChoice { count, .. } = &mut count_two.waiting_for else {
            unreachable!("the fixture asserts the search prompt");
        };
        *count = 2;
        rows.push(gate_row(
            "tutor/count2-distinct",
            &count_two,
            select(&pool[..2]),
            Structural,
            true,
        ));

        // CR 723.5: under player control the controller submits; legality is
        // unchanged.
        let mut controlled_2p = melody.clone();
        controlled_2p.turn_decision_controller = Some(P1);
        for option in melody_options.iter().take(3) {
            rows.push(GateRow {
                submitter: Some(P1),
                ..gate_row(
                    format!("melody/controlled-2p {option}"),
                    &controlled_2p,
                    choose(option),
                    Structural,
                    true,
                )
            });
        }

        let mut controlled_3p = distant_melody_prompt(GameScenario::new_n_player(3, 42));
        controlled_3p.turn_decision_controller = Some(PlayerId(2));
        for option in named_options(&controlled_3p).iter().take(3) {
            rows.push(GateRow {
                submitter: Some(PlayerId(2)),
                ..gate_row(
                    format!("melody/controlled-3p {option}"),
                    &controlled_3p,
                    choose(option),
                    Structural,
                    true,
                )
            });
        }

        // CR 723.5: the controller makes the controlled player's choices; here
        // the search's latched controller (P1), not live turn control (P2),
        // submits. Legality is unchanged.
        let mut latched_3p = demonic_tutor_prompt(GameScenario::new_n_player(3, 42));
        latched_3p
            .active_search_decision_controls
            .get_mut(&P0)
            .expect("the search latches its decision control when it starts")
            .authority = ActiveSearchDecisionAuthority::LatchedController { controller: P1 };
        latched_3p.turn_decision_controller = Some(PlayerId(2));
        for &card in search_pool(&latched_3p).iter().take(2) {
            rows.push(GateRow {
                submitter: Some(P1),
                ..gate_row(
                    format!("tutor/latched-3p {card:?}"),
                    &latched_3p,
                    select(&[card]),
                    Structural,
                    true,
                )
            });
        }

        // Hostile named-choice rows: the authority declines, the simulation
        // decides.
        // CR 205.3m: not a creature type.
        rows.push(gate_row(
            "melody/outside-domain",
            &melody,
            choose("Not A Creature Type"),
            Simulated,
            false,
        ));
        // CR 201.4 + CR 107.1a/b: card names and free-entry numbers are
        // validated by rule; a creature-type word is neither.
        for (label, by_rule) in [
            ("melody/card-name", ChoiceType::CardName),
            (
                "melody/free-entry",
                ChoiceType::NumberRange {
                    min: 0,
                    max: None,
                    distinctness: NumberDistinctness::Repeatable,
                },
            ),
        ] {
            let mut board = melody.clone();
            let WaitingFor::NamedChoice { choice_type, .. } = &mut board.waiting_for else {
                unreachable!("the fixture asserts the named-choice prompt");
            };
            *choice_type = by_rule;
            rows.push(gate_row(
                label,
                &board,
                choose(&melody_options[0]),
                Simulated,
                false,
            ));
        }
        // CR 614.12a: with no parked entry there is nothing to answer.
        let mut no_live_entry = grove.clone();
        no_live_entry.pending_replacement = None;
        rows.push(gate_row(
            "grove/no-live-entry",
            &no_live_entry,
            choose(&grove_options[0]),
            Simulated,
            false,
        ));
        // A source-bound prompt without its matching context (the guard's
        // incoherent-source refusal; not card-derived).
        let mut no_context = grove.clone();
        let WaitingFor::NamedChoice {
            source: Some(source),
            ..
        } = &mut no_context.waiting_for
        else {
            unreachable!("the fixture asserts the source-bound prompt");
        };
        source.context = None;
        rows.push(gate_row(
            "grove/no-context",
            &no_context,
            choose(&grove_options[0]),
            Simulated,
            false,
        ));

        // Hostile search rows.
        // CR 701.23d: "a card" with a non-empty library finds exactly one.
        rows.push(gate_row(
            "tutor/exact-empty",
            &tutor,
            select(&[]),
            Simulated,
            false,
        ));
        rows.push(gate_row(
            "tutor/exact-two",
            &tutor,
            select(&pool[..2]),
            Simulated,
            false,
        ));
        // CR 701.23a: the found card must be in the searched library.
        rows.push(gate_row(
            "tutor/outside-pool",
            &tutor,
            select(&[object_named(&tutor, "Demonic Tutor")]),
            Simulated,
            false,
        ));
        // CR 701.23a: one card cannot be found twice.
        rows.push(gate_row(
            "tutor/duplicate",
            &count_two,
            select(&[pool[0], pool[0]]),
            Simulated,
            false,
        ));
        // CR 202.3 + CR 701.23a: a mana value counts the mana in a cost, so no selection's total is below zero; a found card must match the search's description, which the guard enforces at submission.
        let mut constrained = tutor.clone();
        let WaitingFor::SearchChoice { constraint, .. } = &mut constrained.waiting_for else {
            unreachable!("the fixture asserts the search prompt");
        };
        *constraint = SearchSelectionConstraint::TotalManaValue {
            comparator: Comparator::LT,
            value: 0,
        };
        rows.push(gate_row(
            "tutor/constraint",
            &constrained,
            select(&pool[..1]),
            Simulated,
            false,
        ));
        // Routing row (not card-derived): a scoped search is deferred, and the
        // ordinary guard then accepts a legal single pick.
        rows.push(gate_row(
            "tutor/scoped",
            &with_scoped_search_flag(&tutor),
            select(&pool[..1]),
            Simulated,
            true,
        ));

        // Scope rows: the same two actions at other windows keep the
        // simulation.
        // CR 601.2b + CR 701.4a: P0 can behold two Elves, so "Elf" is legal.
        rows.push(gate_row(
            "reunion/cost-type-choice",
            &celestial_reunion_cost_type_prompt(),
            choose("Elf"),
            Simulated,
            true,
        ));
        // CR 701.23a + CR 608.2d: either found basic is a legal "one".
        let partition = cultivate_partition_prompt();
        let WaitingFor::SearchPartitionChoice { cards: found, .. } = &partition.waiting_for else {
            unreachable!("the fixture asserts the partition prompt");
        };
        for &card in found {
            rows.push(gate_row(
                format!("cultivate/partition-each {card:?}"),
                &partition,
                select(&[card]),
                Simulated,
                true,
            ));
        }

        assert!(
            rows.iter().any(|row| row.route == Structural)
                && rows.iter().any(|row| row.route == Simulated),
            "the table must pair structural rows with simulated controls"
        );

        let config = create_config(AiDifficulty::VeryHard, Platform::Native);
        let policies = PolicyRegistry::default();
        let mut services = PlannerServices::new_default(P0, &config, &policies);
        let mut scope_rows = 0;
        for row in &rows {
            let label = &row.label;
            let reference = planner_simulation_accepts(&row.state, &row.action);
            assert_eq!(
                reference, row.derived,
                "{label}: the planner's simulation must match the derived reading"
            );
            let (verdict, clones) = gate_verdict(&row.state, &row.action);
            assert_eq!(
                verdict, reference,
                "{label}: the gate must agree with the planner's simulation"
            );
            match row.route {
                Structural => assert_eq!(
                    clones, 0,
                    "{label}: an answer the authority admits needs no state copy"
                ),
                Simulated => assert!(
                    clones >= 1,
                    "{label}: an answer the authority declines must be simulated"
                ),
            }
            if let Some(submitter) = row.submitter {
                assert_eq!(
                    turn_control::authorized_submitter(&row.state),
                    Some(submitter),
                    "{label}: the board must derive the controlling submitter"
                );
                assert_ne!(
                    row.state.waiting_for.acting_player(),
                    Some(submitter),
                    "{label}: the submitter must differ from the prompted player"
                );
            }
            if !matches!(
                row.state.waiting_for,
                WaitingFor::NamedChoice { .. } | WaitingFor::SearchChoice { .. }
            ) {
                // Production reach for a scope row: the engine issues this
                // answer and it carries no payment successor, so the planner
                // decides it in `candidate_is_valid`.
                let ctx = services.build_decision_context(&row.state);
                let prepared = prepare_payment_candidates(&row.state, ctx.candidates.clone());
                let entry = prepared
                    .iter()
                    .find(|entry| entry.candidate.action == row.action)
                    .unwrap_or_else(|| panic!("{label}: the engine must issue {:?}", row.action));
                assert!(
                    entry.payment_successor.is_none(),
                    "{label}: the answer must reach `candidate_is_valid`"
                );
                scope_rows += 1;
            }
        }
        assert_eq!(
            scope_rows,
            1 + found.len(),
            "both scope boards must be exercised"
        );
    }

    // ------------------------------------------------------------------
    // The prompt-answer arm through the AI's per-decision driver: the chooser
    // decides under `PromptAnswerValidation::AtSubmission`, simulates only the
    // answer it submits, and when that answer fails decides again as the
    // simulate-every-answer gate does. Each reference decision runs under
    // `ChooserSimulatesEachPromptAnswer`, which is that gate.
    // ------------------------------------------------------------------

    /// CR 205.3m: every creature type, verbatim: Time Lord, the one two-word
    /// type, then the one-word types in the rule's order.
    const CR_205_3M_CREATURE_TYPES: &[&str] = &[
        "Time Lord",
        "Advisor",
        "Aetherborn",
        "Alien",
        "Ally",
        "Angel",
        "Antelope",
        "Ape",
        "Archer",
        "Archon",
        "Armadillo",
        "Army",
        "Artificer",
        "Assassin",
        "Assembly-Worker",
        "Astartes",
        "Atog",
        "Aurochs",
        "Avatar",
        "Azra",
        "Badger",
        "Balloon",
        "Barbarian",
        "Bard",
        "Basilisk",
        "Bat",
        "Bear",
        "Beast",
        "Beaver",
        "Beeble",
        "Beholder",
        "Berserker",
        "Bird",
        "Bison",
        "Blinkmoth",
        "Boar",
        "Bringer",
        "Brushwagg",
        "Camarid",
        "Camel",
        "Capybara",
        "Caribou",
        "Carrier",
        "Cat",
        "Centaur",
        "Child",
        "Chimera",
        "Citizen",
        "Cleric",
        "Clown",
        "Cockatrice",
        "Construct",
        "Coward",
        "Coyote",
        "Crab",
        "Crocodile",
        "C’tan",
        "Custodes",
        "Cyberman",
        "Cyclops",
        "Dalek",
        "Dauthi",
        "Demigod",
        "Demon",
        "Deserter",
        "Detective",
        "Devil",
        "Dinosaur",
        "Djinn",
        "Doctor",
        "Dog",
        "Dragon",
        "Drake",
        "Dreadnought",
        "Drix",
        "Drone",
        "Druid",
        "Dryad",
        "Dwarf",
        "Echidna",
        "Efreet",
        "Egg",
        "Elder",
        "Eldrazi",
        "Elemental",
        "Elephant",
        "Elf",
        "Elk",
        "Employee",
        "Eternal",
        "Eye",
        "Faerie",
        "Ferret",
        "Fish",
        "Flagbearer",
        "Fox",
        "Fractal",
        "Frog",
        "Fungus",
        "Gamer",
        "Gamma",
        "Gargoyle",
        "Germ",
        "Giant",
        "Giraffe",
        "Gith",
        "Glimmer",
        "Gnoll",
        "Gnome",
        "Goat",
        "Goblin",
        "God",
        "Golem",
        "Gorgon",
        "Graveborn",
        "Gremlin",
        "Griffin",
        "Guest",
        "Hag",
        "Halfling",
        "Hamster",
        "Harpy",
        "Hedgehog",
        "Hellion",
        "Hero",
        "Hippo",
        "Hippogriff",
        "Homarid",
        "Homunculus",
        "Horror",
        "Horse",
        "Human",
        "Hydra",
        "Hyena",
        "Illusion",
        "Imp",
        "Incarnation",
        "Inhuman",
        "Inkling",
        "Inquisitor",
        "Insect",
        "Jackal",
        "Jellyfish",
        "Juggernaut",
        "Kangaroo",
        "Kavu",
        "Kirin",
        "Kithkin",
        "Knight",
        "Kobold",
        "Kor",
        "Kraken",
        "Kree",
        "Llama",
        "Lamia",
        "Lammasu",
        "Leech",
        "Lemur",
        "Leviathan",
        "Lhurgoyf",
        "Licid",
        "Lizard",
        "Lobster",
        "Manticore",
        "Masticore",
        "Mercenary",
        "Merfolk",
        "Metathran",
        "Minion",
        "Minotaur",
        "Mite",
        "Mole",
        "Monger",
        "Mongoose",
        "Monk",
        "Monkey",
        "Moogle",
        "Moonfolk",
        "Mount",
        "Mouse",
        "Mutant",
        "Myr",
        "Mystic",
        "Nautilus",
        "Necron",
        "Nephilim",
        "Nightmare",
        "Nightstalker",
        "Ninja",
        "Noble",
        "Noggle",
        "Nomad",
        "Nymph",
        "Octopus",
        "Ogre",
        "Ooze",
        "Orb",
        "Orc",
        "Orgg",
        "Otter",
        "Ouphe",
        "Ox",
        "Oyster",
        "Pangolin",
        "Peasant",
        "Pegasus",
        "Pentavite",
        "Performer",
        "Pest",
        "Phelddagrif",
        "Phoenix",
        "Phyrexian",
        "Pilot",
        "Pincher",
        "Pirate",
        "Plant",
        "Platypus",
        "Porcupine",
        "Possum",
        "Praetor",
        "Primarch",
        "Prism",
        "Processor",
        "Qu",
        "Rabbit",
        "Raccoon",
        "Ranger",
        "Rat",
        "Rebel",
        "Reflection",
        "Rhino",
        "Rigger",
        "Robot",
        "Rogue",
        "Sable",
        "Salamander",
        "Samurai",
        "Sand",
        "Saproling",
        "Satyr",
        "Scarecrow",
        "Scientist",
        "Scion",
        "Scorpion",
        "Scout",
        "Sculpture",
        "Seal",
        "Serf",
        "Serpent",
        "Servo",
        "Shade",
        "Shaman",
        "Shapeshifter",
        "Shark",
        "Sheep",
        "Shi’ar",
        "Siren",
        "Skeleton",
        "Skrull",
        "Skunk",
        "Slith",
        "Sliver",
        "Sloth",
        "Slug",
        "Snail",
        "Snake",
        "Soldier",
        "Soltari",
        "Sorcerer",
        "Spawn",
        "Specter",
        "Spellshaper",
        "Sphinx",
        "Spider",
        "Spike",
        "Spirit",
        "Splinter",
        "Sponge",
        "Spy",
        "Squid",
        "Squirrel",
        "Starfish",
        "Surrakar",
        "Survivor",
        "Symbiote",
        "Synth",
        "Tentacle",
        "Tetravite",
        "Thalakos",
        "Thopter",
        "Thrull",
        "Tiefling",
        "Toy",
        "Treefolk",
        "Trilobite",
        "Triskelavite",
        "Troll",
        "Turtle",
        "Tyranid",
        "Unicorn",
        "Utrom",
        "Vampire",
        "Varmint",
        "Vedalken",
        "Villain",
        "Volver",
        "Wall",
        "Walrus",
        "Warlock",
        "Warrior",
        "Weasel",
        "Weird",
        "Werewolf",
        "Whale",
        "Wizard",
        "Wolf",
        "Wolverine",
        "Wombat",
        "Worm",
        "Wraith",
        "Wurm",
        "Yeti",
        "Zombie",
        "Zubera",
    ];

    const LUMINESCENT_RAIN_ORACLE: &str =
        "Choose a creature type. You gain 2 life for each permanent you control of that type.";

    /// Put `name`, a P0 spell with `oracle` and mana cost `cost`, in P0's hand,
    /// and float one mana of each type in `mana` to pay for it.
    fn add_creature_type_spell(
        scenario: &mut GameScenario,
        name: &str,
        is_instant: bool,
        oracle: &str,
        cost: ManaCost,
        mana: &[ManaType],
    ) -> ObjectId {
        let mut builder = scenario.add_spell_to_hand_from_oracle(P0, name, is_instant, oracle);
        builder.with_mana_cost(cost);
        let spell = builder.id();
        scenario.with_mana_pool(
            P0,
            mana.iter()
                .map(|&mana_type| ManaUnit::new(mana_type, spell, false, vec![]))
                .collect(),
        );
        spell
    }

    /// Cast `spell` for P0 on `runner` and resolve it up to its CR 608.2d
    /// creature-type prompt, with `GameState::all_creature_types` holding every
    /// type CR 205.3m lists, so the prompt offers that full list (sorted, as the
    /// engine offers it) rather than the engine's short fallback list.
    fn prompt_offering_every_creature_type(mut runner: GameRunner, spell: ObjectId) -> GameState {
        runner.state_mut().all_creature_types = CR_205_3M_CREATURE_TYPES
            .iter()
            .map(|creature_type| creature_type.to_string())
            .collect();
        let state = resolve_to_prompt(&mut runner, spell);
        let mut every_type = runner.state().all_creature_types.clone();
        every_type.sort();
        match &state.waiting_for {
            WaitingFor::NamedChoice {
                player,
                choice_type: ChoiceType::CreatureType { .. },
                options,
                ..
            } => {
                assert_eq!(*player, P0, "P0 resolves the spell");
                assert_eq!(
                    options, &every_type,
                    "the prompt must offer every CR 205.3m creature type"
                );
            }
            other => panic!("the spell must halt on its creature-type prompt, got {other:?}"),
        }
        assert!(
            !state.withholds_priority(),
            "the prompt is answered while the spell resolves, not mid-cast"
        );
        state
    }

    /// What one AI decision did.
    struct AiDecision {
        /// The action the AI submitted.
        action: GameAction,
        /// The state after the engine applied it.
        after: GameState,
        /// The legality state copies the decision made.
        clones: u64,
        /// The AI's next RNG draw after the decision: equal values mean the
        /// two decisions left the AI's stream at the same position.
        next_draw: u64,
    }

    /// One AI decision for P0 on `prompt` through `auto_play::run_ai_action`,
    /// the per-decision step of the native AI game loops (ai-duel and
    /// ai-commander reach it through `run_ai_actions_with_limit`, the
    /// multiplayer server directly): issue the decision contract, choose
    /// through `choose_action_with_issued_contract`, and apply the choice at
    /// the engine's interaction boundary. The seat is configured as ai-duel
    /// configures one (`Medium`, native, two players, measurement mode, so no
    /// wall-clock deadline), with the AI's RNG seeded from the same run seed
    /// and a fresh session. Panics if the engine rejects the action.
    fn run_ai_decision(prompt: &GameState) -> AiDecision {
        run_ai_decision_with(
            prompt,
            create_config_for_players(AiDifficulty::Medium, Platform::Native, 2)
                .into_measurement(AI_DECISION_SEED),
        )
    }

    /// The run seed of every AI decision these tests take: the seat's RNG is
    /// seeded from it, as is a measurement-mode seat configuration.
    const AI_DECISION_SEED: u64 = 0;

    /// A wall-clock budget no decision in these tests comes near (ten minutes):
    /// a deadline started from it is still live when the decision ends.
    const LIVE_TIME_BUDGET_MS: u32 = 600_000;

    /// [`run_ai_decision`] for a seat configured as `config`.
    fn run_ai_decision_with(prompt: &GameState, config: AiConfig) -> AiDecision {
        run_ai_decision_in(prompt, config, AiSession::arc_from_game(prompt))
    }

    /// [`run_ai_decision_with`] in `session`, a session fresh for `prompt`.
    fn run_ai_decision_in(
        prompt: &GameState,
        config: AiConfig,
        session: Arc<AiSession>,
    ) -> AiDecision {
        let ai_configs = HashMap::from([(P0, config)]);
        let ai_players = HashSet::from([P0]);
        let mut rng = StdRng::seed_from_u64(AI_DECISION_SEED);
        let mut state = prompt.clone();
        perf_counters::reset();
        let step = run_ai_action(&mut state, &ai_players, &ai_configs, &mut rng, &session);
        let clones = perf_counters::snapshot().state_clone_for_legality;
        match step {
            ControlFlow::Continue(result) => AiDecision {
                action: result.action,
                after: state,
                clones,
                next_draw: rng.random(),
            },
            ControlFlow::Break(stop) => {
                panic!("the engine must accept the AI's action, got {stop:?}")
            }
        }
    }

    /// [`run_ai_decision`] with the chooser on `PromptAnswerValidation::EachCandidate`
    /// from the start: the legality gate that simulates every prompt answer and
    /// checks nothing after selection.
    fn run_reference_ai_decision(prompt: &GameState) -> AiDecision {
        let _each_candidate = ChooserSimulatesEachPromptAnswer::enter();
        run_ai_decision(prompt)
    }

    /// The first `count` values the AI's RNG yields, seeded as each decision here
    /// seeds it. A selection's draw (`rng.random::<f64>()`) advances the stream by
    /// one `u64`, so a decision that took `n` draws reports `seat_draws(n + 1)[n]`
    /// as its `next_draw`.
    fn seat_draws(count: usize) -> Vec<u64> {
        let mut rng = StdRng::seed_from_u64(AI_DECISION_SEED);
        (0..count).map(|_| rng.random()).collect()
    }

    /// A tactical policy that rejects every named-choice answer (`ChooseOption`)
    /// except those naming one of `except`, and is neutral on everything else.
    /// Installed as a session's whole registry (`AiSession::policy_registry_override`),
    /// it makes each rejected answer's tactical score −∞ (`PolicyRegistry::score`):
    /// the only way to put a policy-rejected prompt answer in front of the scorer.
    #[derive(Clone, Copy)]
    struct RejectsNamedChoices {
        except: &'static [&'static str],
    }

    impl TacticalPolicy for RejectsNamedChoices {
        fn id(&self) -> PolicyId {
            PolicyId::CreatureTypeChoice
        }

        fn decision_kinds(&self) -> &'static [DecisionKind] {
            // `decision_kind::classify` routes a `NamedChoice` answer here.
            &[DecisionKind::ActivateAbility]
        }

        fn activation(
            &self,
            _: &crate::features::DeckFeatures,
            _: &GameState,
            _: PlayerId,
        ) -> Option<f32> {
            Some(1.0)
        }

        fn verdict(&self, context: &PolicyContext<'_>) -> PolicyVerdict {
            let reason = PolicyReason::new("rejects_named_choices_test");
            match &context.candidate.action {
                GameAction::ChooseOption { choice } if !self.except.contains(&choice.as_str()) => {
                    PolicyVerdict::reject(reason)
                }
                _ => PolicyVerdict::neutral(reason),
            }
        }
    }

    /// A fresh session for `prompt` whose scorer consults `policy` alone.
    fn session_scoring_with(prompt: &GameState, policy: RejectsNamedChoices) -> Arc<AiSession> {
        let mut session = AiSession::from_game(prompt);
        session.policy_registry_override =
            Some(Arc::new(PolicyRegistry::for_tests(vec![Box::new(policy)])));
        Arc::new(session)
    }

    fn seat(state: &GameState, player: PlayerId) -> &engine::types::player::Player {
        state
            .players
            .iter()
            .find(|seat| seat.id == player)
            .expect("the player exists")
    }

    /// Production path on the full creature-type list: through the AI's
    /// per-decision driver, Distant Melody's CR 608.2d creature-type prompt
    /// offering every CR 205.3m type costs one legality state copy, for the
    /// type the AI submits, and the AI submits the type, and leaves its RNG
    /// where, the simulate-every-answer gate does. The engine accepts the
    /// answer and the spell finishes resolving with it.
    ///
    /// Fails if the chooser's prompt-answer arm is removed (one state copy per
    /// offered type) or loses its submission check (no copy).
    #[test]
    fn ai_driver_simulates_only_the_creature_type_it_submits() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        // A library, so the cards "Elf" draws do not come from an empty one.
        scenario.with_library_top(P0, &["Card A", "Card B", "Card C", "Card D"]);
        for name in ["Elf A", "Elf B"] {
            scenario
                .add_creature(P0, name, 1, 1)
                .with_subtypes(vec!["Elf"]);
        }
        let melody = add_creature_type_spell(
            &mut scenario,
            "Distant Melody",
            false,
            DISTANT_MELODY_ORACLE,
            ManaCost::Cost {
                generic: 3,
                shards: vec![ManaCostShard::Blue],
            },
            &[
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Colorless,
                ManaType::Blue,
            ],
        );
        let prompt = prompt_offering_every_creature_type(scenario.build(), melody);
        let offered = named_options(&prompt).len() as u64;

        let decision = run_ai_decision(&prompt);
        assert_eq!(
            decision.clones, 1,
            "the AI driver must simulate the creature type it submits, and no other"
        );

        // The engine accepted the answer (`run_ai_decision` panics otherwise)
        // and Distant Melody resolved with it: P0 drew a card for each
        // permanent it controls of the chosen type, and as the active player
        // receives priority again (CR 117.3b).
        let GameAction::ChooseOption { choice } = &decision.action else {
            panic!(
                "the AI must answer the creature-type prompt, got {:?}",
                decision.action
            );
        };
        let of_chosen_type = prompt
            .objects
            .values()
            .filter(|object| {
                object.zone == Zone::Battlefield
                    && object.controller == P0
                    && object
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype == choice)
            })
            .count();
        assert_eq!(
            seat(&decision.after, P0).hand.len(),
            seat(&prompt, P0).hand.len() + of_chosen_type,
            "P0 draws a card for each permanent it controls of the chosen type ({choice})"
        );
        assert!(
            decision.after.stack.is_empty(),
            "Distant Melody must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Distant Melody resolves, got {:?}",
            decision.after.waiting_for
        );

        let reference = run_reference_ai_decision(&prompt);
        assert_eq!(
            reference.clones, offered,
            "reach guard: the simulate-every-answer gate copies the state once per offered type"
        );
        assert_eq!(
            decision.action, reference.action,
            "the AI must submit the creature type the simulate-every-answer gate picks"
        );
        assert_eq!(
            decision.next_draw, reference.next_draw,
            "the decision must draw from the AI's RNG exactly as that gate's decision does"
        );
    }

    /// Production path where the answer the AI prefers passes the structural
    /// check but the engine rejects it: Luminescent Rain ("Choose a creature
    /// type. You gain 2 life for each permanent you control of that type.") for
    /// a P0 one life below the largest life total the engine represents
    /// (`i32::MAX`; CR 119.3 adjusts a life total by what is gained, and the
    /// rules set no maximum), controlling one creature of each of five types.
    /// Each of those types would gain 2 life, a total outside that range, so
    /// its apply returns `EngineError::LifeChangeUnrepresentable`; every other
    /// type gains no life and applies. The five fill the Medium root beam, and
    /// the root search's simulation of the first of them fails, so the decision
    /// is scored again with every type simulated before anything is selected. The AI
    /// submits the type, and leaves its RNG where, the simulate-every-answer
    /// gate does, and the driver simulates that type once more before
    /// submitting it. Its assertions do not tell that re-score apart from the
    /// chooser's retake of a selected type that fails; rung 0's detection is
    /// pinned by
    /// `rung_zero_stops_at_the_first_admitted_answer_that_fails` (`search.rs`),
    /// `ai_driver_scores_again_when_its_root_search_rejects_an_admitted_type`
    /// and `ai_driver_scores_again_when_a_policy_rejected_beam_type_fails`.
    ///
    /// Fails if nothing simulates an admitted answer before it is submitted:
    /// the AI then submits a rejected type and `run_ai_action` stops with
    /// `ApplyFailed`.
    #[test]
    fn ai_driver_replaces_a_submitted_creature_type_the_engine_rejects() {
        const CONTROLLED: [&str; 5] = ["Elf", "Goblin", "Human", "Merfolk", "Zombie"];
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, i32::MAX - 1);
        for subtype in CONTROLLED {
            scenario
                .add_creature(P0, &format!("{subtype} A"), 1, 1)
                .with_subtypes(vec![subtype]);
        }
        let rain = add_creature_type_spell(
            &mut scenario,
            "Luminescent Rain",
            true,
            LUMINESCENT_RAIN_ORACLE,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            },
            &[ManaType::Colorless, ManaType::Colorless, ManaType::Green],
        );
        let prompt = prompt_offering_every_creature_type(scenario.build(), rain);
        let offered = named_options(&prompt).len() as u64;

        // Fixture: each controlled type passes the structural check and is
        // rejected on apply; a type P0 controls no permanent of applies.
        for subtype in CONTROLLED {
            let answer = choose(subtype);
            assert!(
                engine::ai_support::structurally_valid_prompt_answer(&prompt, &answer),
                "{subtype} is an offered creature type"
            );
            let mut sim = prompt.clone();
            let applied = apply_as_current_for_simulation(&mut sim, answer);
            assert!(
                matches!(
                    applied,
                    Err(EngineError::LifeChangeUnrepresentable { player }) if player == P0
                ),
                "{subtype}: 2 more life is outside the engine's range, got {applied:?}"
            );
        }
        assert!(
            planner_simulation_accepts(&prompt, &choose("Advisor")),
            "a type P0 controls no permanent of gains no life and applies"
        );

        let decision = run_ai_decision(&prompt);
        let reference = run_reference_ai_decision(&prompt);
        assert_eq!(
            reference.clones, offered,
            "reach guard: the simulate-every-answer gate copies the state once per offered type"
        );
        assert_eq!(
            decision.clones,
            offered + 1,
            "the root search rejects the first type in its beam, every offered type is then \
             simulated, and the submitted one once more"
        );
        assert_eq!(
            decision.action, reference.action,
            "the AI must submit the creature type the simulate-every-answer gate picks"
        );
        assert_eq!(
            decision.next_draw, reference.next_draw,
            "the decision must draw from the AI's RNG exactly as that gate's decision does"
        );

        // Luminescent Rain resolved with an accepted type: none of P0's, so no
        // life is gained, and P0 as the active player receives priority again
        // (CR 117.3b).
        let GameAction::ChooseOption { choice } = &decision.action else {
            panic!(
                "the AI must answer the creature-type prompt, got {:?}",
                decision.action
            );
        };
        assert!(
            !CONTROLLED.contains(&choice.as_str()),
            "the AI must not submit a rejected type, got {choice}"
        );
        assert_eq!(seat(&decision.after, P0).life, i32::MAX - 1);
        assert!(
            decision.after.stack.is_empty(),
            "Luminescent Rain must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Luminescent Rain resolves, got {:?}",
            decision.after.waiting_for
        );
    }

    /// Luminescent Rain ("Choose a creature type. You gain 2 life for each
    /// permanent you control of that type.") cast by a P0 one life below the
    /// largest life total the engine represents (`i32::MAX`), controlling one
    /// creature of each type in `controlled`, resolved up to its creature-type
    /// prompt offering every CR 205.3m type. Asserts the fixture: each
    /// controlled type passes the structural check and its apply returns
    /// `EngineError::LifeChangeUnrepresentable` (2 more life is outside that
    /// range; CR 119.3 adjusts a life total by what is gained, and the rules set
    /// no maximum), and a type P0 controls no permanent of gains no life and
    /// applies.
    /// `pub(crate)` so the root-search tests in `search.rs` build the same board.
    pub(crate) fn luminescent_rain_prompt_rejecting(controlled: &[&str]) -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, i32::MAX - 1);
        for subtype in controlled {
            scenario
                .add_creature(P0, &format!("{subtype} A"), 1, 1)
                .with_subtypes(vec![*subtype]);
        }
        let rain = add_creature_type_spell(
            &mut scenario,
            "Luminescent Rain",
            true,
            LUMINESCENT_RAIN_ORACLE,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::Green],
            },
            &[ManaType::Colorless, ManaType::Colorless, ManaType::Green],
        );
        let prompt = prompt_offering_every_creature_type(scenario.build(), rain);
        for subtype in controlled {
            let answer = choose(subtype);
            assert!(
                engine::ai_support::structurally_valid_prompt_answer(&prompt, &answer),
                "{subtype} is an offered creature type"
            );
            let mut sim = prompt.clone();
            let applied = apply_as_current_for_simulation(&mut sim, answer);
            assert!(
                matches!(
                    applied,
                    Err(EngineError::LifeChangeUnrepresentable { player }) if player == P0
                ),
                "{subtype}: 2 more life is outside the engine's range, got {applied:?}"
            );
        }
        assert!(
            planner_simulation_accepts(&prompt, &choose("Advisor")),
            "a type P0 controls no permanent of gains no life and applies"
        );
        prompt
    }

    /// Production path where the root beam holds answers the engine rejects
    /// beside one it accepts: Luminescent Rain for a P0 at `i32::MAX - 1` life
    /// controlling one creature of each of four types. Those four types would
    /// each gain life past the engine's range, so their applies fail; they
    /// outrank every other type and take four of the Medium beam's five places,
    /// and the fifth is a type P0 controls no permanent of, which applies. The
    /// root search simulates the beam in order and stops at the first of the
    /// four, which it finds rejected, so the decision is scored again with
    /// every type simulated before anything is selected: the AI submits the
    /// type, and leaves its RNG where, the
    /// simulate-every-answer gate does, whose beam holds five accepted types.
    ///
    /// Fails if only a selected answer that fails is replaced: the AI then
    /// selects the one accepted type of the crowded beam.
    #[test]
    fn ai_driver_scores_again_when_its_root_search_rejects_an_admitted_type() {
        const CONTROLLED: [&str; 4] = ["Elf", "Goblin", "Human", "Merfolk"];
        let prompt = luminescent_rain_prompt_rejecting(&CONTROLLED);
        let offered = named_options(&prompt).len() as u64;

        let decision = run_ai_decision(&prompt);
        let reference = run_reference_ai_decision(&prompt);
        assert_eq!(
            reference.clones, offered,
            "reach guard: the simulate-every-answer gate copies the state once per offered type"
        );
        assert_eq!(
            decision.action, reference.action,
            "the AI must submit the creature type the simulate-every-answer gate picks"
        );
        assert_eq!(
            decision.next_draw, reference.next_draw,
            "the decision must draw from the AI's RNG exactly as that gate's decision does"
        );
        assert_eq!(
            decision.clones,
            offered + 1,
            "the root search rejects the first admitted type in its beam, every offered type is then \
             simulated, and the submitted one once more"
        );

        // Luminescent Rain resolved with an accepted type: none of P0's, so no
        // life is gained, and P0 as the active player receives priority again
        // (CR 117.3b).
        let GameAction::ChooseOption { choice } = &decision.action else {
            panic!(
                "the AI must answer the creature-type prompt, got {:?}",
                decision.action
            );
        };
        assert!(
            !CONTROLLED.contains(&choice.as_str()),
            "the AI must not submit a rejected type, got {choice}"
        );
        assert_eq!(seat(&decision.after, P0).life, i32::MAX - 1);
        assert!(
            decision.after.stack.is_empty(),
            "Luminescent Rain must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Luminescent Rain resolves, got {:?}",
            decision.after.waiting_for
        );
    }

    /// Production path where nothing simulates the beam before selection: the
    /// Luminescent Rain board whose five rejected types fill the Medium root
    /// beam, decided by a seat whose root search has no time
    /// (`time_budget_ms: Some(0)`), so its deadline has expired before the
    /// search's first rung and the beam is scored by the tactical floor alone.
    /// The AI selects a rejected type, the driver's simulation of it fails, and
    /// the decision is taken again with every type simulated, from the same
    /// selection draw and under the same expired deadline: the AI submits the
    /// type, and leaves its RNG where, the simulate-every-answer gate does.
    ///
    /// Fails without the chooser's submission check: the AI then submits the
    /// rejected type and `run_ai_action` stops with `ApplyFailed`.
    #[test]
    fn ai_driver_replaces_a_rejected_type_it_selects_before_any_search() {
        const CONTROLLED: [&str; 5] = ["Elf", "Goblin", "Human", "Merfolk", "Zombie"];
        let prompt = luminescent_rain_prompt_rejecting(&CONTROLLED);
        let offered = named_options(&prompt).len() as u64;
        let mut config = create_config_for_players(AiDifficulty::Medium, Platform::Native, 2);
        config.search.time_budget_ms = Some(0);
        assert!(
            !config.execution_mode.is_measurement() && config.search.enabled,
            "fixture: a searching seat whose wall-clock budget is spent before the search"
        );

        let decision = run_ai_decision_with(&prompt, config.clone());
        let reference = {
            let _each_candidate = ChooserSimulatesEachPromptAnswer::enter();
            run_ai_decision_with(&prompt, config)
        };
        assert_eq!(
            reference.clones, offered,
            "reach guard: the simulate-every-answer gate copies the state once per offered type"
        );
        assert_eq!(
            decision.clones,
            offered + 1,
            "the selected type's simulation fails, then every offered type is simulated"
        );
        assert_eq!(
            decision.action, reference.action,
            "the AI must submit the creature type the simulate-every-answer gate picks"
        );
        assert_eq!(
            decision.next_draw, reference.next_draw,
            "the decision must draw from the AI's RNG exactly as that gate's decision does"
        );
        let GameAction::ChooseOption { choice } = &decision.action else {
            panic!(
                "the AI must answer the creature-type prompt, got {:?}",
                decision.action
            );
        };
        assert!(
            !CONTROLLED.contains(&choice.as_str()),
            "the AI must not submit a rejected type, got {choice}"
        );
        assert_eq!(seat(&decision.after, P0).life, i32::MAX - 1);
        assert!(
            decision.after.stack.is_empty(),
            "Luminescent Rain must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Luminescent Rain resolves, got {:?}",
            decision.after.waiting_for
        );
    }

    /// Production path where the root beam holds an answer both a policy and the
    /// engine reject: Luminescent Rain ("Choose a creature type. You gain 2 life
    /// for each permanent you control of that type.") for a P0 at `i32::MAX - 1`
    /// life controlling one Ally, decided by a Medium seat whose only policy
    /// rejects every creature type. Every type then scores −∞, so the root beam is
    /// the first five types in the stable action order (Advisor, Aetherborn, Alien,
    /// Ally, Angel). Ally's apply fails: 2 more life is outside the engine's range
    /// (CR 119.3 adjusts a life total by what is gained, and the rules set no
    /// maximum). The root search's first rung simulates the admitted answers in its
    /// beam in order although a policy rejects them, stops at Ally, which it finds
    /// rejected, and the decision is scored again with every type simulated before
    /// anything is selected. That
    /// gate's beam (Advisor, Aetherborn, Alien, Angel, Antelope) is all −∞, so its
    /// selection takes no draw and falls back to the last row in the stable order,
    /// Antelope, which the driver simulates once more before submitting it.
    ///
    /// Fails if the first rung skips a rejected admitted answer: the AI then
    /// selects Angel from the beam that holds Ally, after one state copy.
    #[test]
    fn ai_driver_scores_again_when_a_policy_rejected_beam_type_fails() {
        const CONTROLLED: [&str; 1] = ["Ally"];
        let rejects_every_type = RejectsNamedChoices { except: &[] };
        let prompt = luminescent_rain_prompt_rejecting(&CONTROLLED);
        let offered = named_options(&prompt).len() as u64;
        let config = create_config_for_players(AiDifficulty::Medium, Platform::Native, 2)
            .into_measurement(AI_DECISION_SEED);

        let decision = run_ai_decision_in(
            &prompt,
            config.clone(),
            session_scoring_with(&prompt, rejects_every_type),
        );
        let reference = {
            let _each_candidate = ChooserSimulatesEachPromptAnswer::enter();
            run_ai_decision_in(
                &prompt,
                config,
                session_scoring_with(&prompt, rejects_every_type),
            )
        };
        assert_eq!(
            reference.clones, offered,
            "reach guard: the simulate-every-answer gate copies the state once per offered type"
        );
        assert_eq!(
            reference.next_draw,
            seat_draws(1)[0],
            "reach guard: the policy rejects every type, so no row has a finite weight and the selection takes no draw"
        );
        assert_eq!(
            reference.action,
            choose("Antelope"),
            "reach guard: that gate's beam is the first five accepted types in the stable order, and its selection falls back to the last"
        );
        assert_eq!(
            decision.clones,
            offered + 1,
            "the first rung simulates the policy-rejected Ally in the beam and finds it rejected, every offered type is then simulated, and the submitted one once more"
        );
        assert_eq!(
            decision.action, reference.action,
            "the AI must submit the creature type the simulate-every-answer gate picks"
        );
        assert_eq!(
            decision.next_draw, reference.next_draw,
            "the decision must draw from the AI's RNG exactly as that gate's decision does"
        );
        assert_eq!(seat(&decision.after, P0).life, i32::MAX - 1);
        assert!(
            decision.after.stack.is_empty(),
            "Luminescent Rain must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Luminescent Rain resolves (CR 117.3b), got {:?}",
            decision.after.waiting_for
        );
    }

    /// Production path on a seat with search disabled (Easy;
    /// `create_config_for_players` also disables it at Medium or below for five or
    /// more players), so no root search simulates the scored answers and the
    /// chooser's check of the answer it selects is the only simulation of an
    /// admitted answer before submission. Luminescent Rain ("Choose a creature
    /// type. You gain 2 life for each permanent you control of that type.") for a
    /// P0 at `i32::MAX - 1` life controlling one creature of each of five types,
    /// scored by a policy that rejects every type except those five. Each of the
    /// five would gain life past the engine's range (CR 119.3 sets no maximum), so
    /// its apply fails. They are the only rows with a finite score, so the
    /// only rows with a nonzero selection weight, and the selection draws once
    /// and selects one of them. The driver's simulation of it
    /// fails, and the decision is taken again with every type simulated: the five
    /// are gone, every remaining row is −∞, and the selection falls back to the
    /// last type in the stable order, Zubera, without another draw. The
    /// simulate-every-answer gate selects Zubera too, without any draw, so the
    /// decision leaves the AI's RNG one draw ahead of that gate's, as the chooser
    /// states for a seat whose root search does not simulate the beam.
    ///
    /// Fails if the chooser checks the answer it selects only on a searching seat:
    /// the AI then submits a rejected type and `run_ai_action` stops with
    /// `ApplyFailed`.
    #[test]
    fn ai_driver_without_search_replaces_a_rejected_type_it_selects() {
        const CONTROLLED: [&str; 5] = ["Elf", "Goblin", "Human", "Merfolk", "Zombie"];
        let rejects_all_but_controlled = RejectsNamedChoices {
            except: &CONTROLLED,
        };
        let prompt = luminescent_rain_prompt_rejecting(&CONTROLLED);
        let offered = named_options(&prompt).len() as u64;
        let config = create_config_for_players(AiDifficulty::Easy, Platform::Native, 2)
            .into_measurement(AI_DECISION_SEED);
        assert!(
            !config.search.enabled
                && PromptAnswerValidation::for_chooser(&config)
                    == PromptAnswerValidation::AtSubmission,
            "fixture: a seat that scores without search and decides under `AtSubmission`"
        );

        let decision = run_ai_decision_in(
            &prompt,
            config.clone(),
            session_scoring_with(&prompt, rejects_all_but_controlled),
        );
        let reference = {
            let _each_candidate = ChooserSimulatesEachPromptAnswer::enter();
            run_ai_decision_in(
                &prompt,
                config,
                session_scoring_with(&prompt, rejects_all_but_controlled),
            )
        };
        let draws = seat_draws(2);
        assert_eq!(
            reference.clones, offered,
            "reach guard: the simulate-every-answer gate copies the state once per offered type"
        );
        assert_eq!(
            reference.next_draw, draws[0],
            "reach guard: without the five rejected types every row is rejected, so that gate's selection takes no draw"
        );
        assert_eq!(
            reference.action,
            choose("Zubera"),
            "reach guard: that gate's selection falls back to the last type in the stable order"
        );
        assert_eq!(
            decision.clones,
            offered + 1,
            "the selected type's simulation fails, then every offered type is simulated"
        );
        assert_eq!(
            decision.action, reference.action,
            "the AI must submit the creature type the simulate-every-answer gate picks"
        );
        assert_eq!(
            decision.next_draw, draws[1],
            "the decision's first selection draws once, where that gate's selection draws none"
        );
        assert_eq!(seat(&decision.after, P0).life, i32::MAX - 1);
        assert!(
            decision.after.stack.is_empty(),
            "Luminescent Rain must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Luminescent Rain resolves (CR 117.3b), got {:?}",
            decision.after.waiting_for
        );
    }

    /// Production path for the chooser's retake under a live wall-clock
    /// deadline: the board and policy of
    /// `ai_driver_without_search_replaces_a_rejected_type_it_selects`, decided
    /// by an Easy seat, whose search is disabled, outside measurement mode and
    /// under a budget no decision comes near. The type the selection takes is
    /// one P0 controls, so its simulation fails and the chooser takes the
    /// decision again under `EachCandidate`, a second scoring pass. That second
    /// pass must run under the root-search deadline the first pass started.
    ///
    /// Fails if the chooser hands a pass a fresh `RootSearchDeadline`: the
    /// second pass then starts a deadline of its own, at a later instant.
    #[test]
    fn ai_driver_retakes_a_decision_under_the_deadline_its_first_pass_started() {
        const CONTROLLED: [&str; 5] = ["Elf", "Goblin", "Human", "Merfolk", "Zombie"];
        let prompt = luminescent_rain_prompt_rejecting(&CONTROLLED);
        let offered = named_options(&prompt).len() as u64;
        let mut config = create_config_for_players(AiDifficulty::Easy, Platform::Native, 2);
        config.search.time_budget_ms = Some(LIVE_TIME_BUDGET_MS);
        assert!(
            !config.execution_mode.is_measurement()
                && !config.search.enabled
                && PromptAnswerValidation::for_chooser(&config)
                    == PromptAnswerValidation::AtSubmission,
            "fixture: a seat under a wall-clock budget that scores without search and decides under `AtSubmission`"
        );

        crate::search::ROOT_SEARCH_DEADLINES.with_borrow_mut(Vec::clear);
        let decision = run_ai_decision_in(
            &prompt,
            config,
            session_scoring_with(
                &prompt,
                RejectsNamedChoices {
                    except: &CONTROLLED,
                },
            ),
        );
        let deadlines = crate::search::ROOT_SEARCH_DEADLINES.take();

        assert_eq!(
            decision.clones,
            offered + 1,
            "reach guard: the selected type's simulation fails, then every offered type is simulated"
        );
        assert_eq!(
            deadlines.len(),
            2,
            "reach guard: the decision is scored twice"
        );
        assert!(
            deadlines[0].remaining().is_some(),
            "reach guard: the first pass runs under a wall-clock deadline"
        );
        assert_eq!(
            deadlines[1], deadlines[0],
            "the retaken decision must run under the deadline its first pass started"
        );
        let GameAction::ChooseOption { choice } = &decision.action else {
            panic!(
                "the AI must answer the creature-type prompt, got {:?}",
                decision.action
            );
        };
        assert!(
            !CONTROLLED.contains(&choice.as_str()),
            "the AI must not submit a rejected type, got {choice}"
        );
        assert_eq!(seat(&decision.after, P0).life, i32::MAX - 1);
        assert!(
            decision.after.stack.is_empty(),
            "Luminescent Rain must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Luminescent Rain resolves (CR 117.3b), got {:?}",
            decision.after.waiting_for
        );
    }

    /// Production path for the scorer's re-score under a live wall-clock
    /// deadline: the board of
    /// `ai_driver_replaces_a_submitted_creature_type_the_engine_rejects`,
    /// decided by a Medium seat outside measurement mode and under a budget no
    /// decision comes near. The root search's first rung finds a type of its
    /// beam rejected, so the scorer scores the decision again under
    /// `EachCandidate`, a second scoring pass. That second pass must run under
    /// the root-search deadline the first pass started.
    ///
    /// The assertions do not tell that re-score apart from the chooser's retake
    /// of a selected answer that fails: the chooser simulates a rejected type
    /// it selects and takes the decision again under `EachCandidate`, a second
    /// scoring pass that also runs under the deadline the first pass started,
    /// with as many state copies. Rung 0's detection of a rejected admitted
    /// answer is pinned by
    /// `rung_zero_stops_at_the_first_admitted_answer_that_fails` (`search.rs`),
    /// `ai_driver_scores_again_when_its_root_search_rejects_an_admitted_type`
    /// and `ai_driver_scores_again_when_a_policy_rejected_beam_type_fails`.
    ///
    /// Fails if the scorer hands its second pass a fresh `RootSearchDeadline`:
    /// that pass then starts a deadline of its own, at a later instant.
    #[test]
    fn ai_driver_scores_again_under_the_deadline_its_root_search_started() {
        const CONTROLLED: [&str; 5] = ["Elf", "Goblin", "Human", "Merfolk", "Zombie"];
        let prompt = luminescent_rain_prompt_rejecting(&CONTROLLED);
        let offered = named_options(&prompt).len() as u64;
        let mut config = create_config_for_players(AiDifficulty::Medium, Platform::Native, 2);
        config.search.time_budget_ms = Some(LIVE_TIME_BUDGET_MS);
        assert!(
            !config.execution_mode.is_measurement() && config.search.enabled,
            "fixture: a searching seat under a wall-clock budget"
        );

        crate::search::ROOT_SEARCH_DEADLINES.with_borrow_mut(Vec::clear);
        let decision = run_ai_decision_with(&prompt, config);
        let deadlines = crate::search::ROOT_SEARCH_DEADLINES.take();

        assert_eq!(
            decision.clones,
            offered + 1,
            "reach guard: the decision is scored again with every offered type simulated, and the submitted one once more"
        );
        assert_eq!(
            deadlines.len(),
            2,
            "reach guard: the decision is scored twice"
        );
        assert!(
            deadlines[0].remaining().is_some(),
            "reach guard: the first pass runs under a wall-clock deadline"
        );
        assert!(
            !deadlines[0].expired(),
            "reach guard: the first pass's deadline has not expired when the decision ends, so it did not stop that pass's root search before its first rung"
        );
        assert_eq!(
            deadlines[1], deadlines[0],
            "the decision scored again must run under the deadline its first pass started"
        );
        let GameAction::ChooseOption { choice } = &decision.action else {
            panic!(
                "the AI must answer the creature-type prompt, got {:?}",
                decision.action
            );
        };
        assert!(
            !CONTROLLED.contains(&choice.as_str()),
            "the AI must not submit a rejected type, got {choice}"
        );
        assert_eq!(seat(&decision.after, P0).life, i32::MAX - 1);
        assert!(
            decision.after.stack.is_empty(),
            "Luminescent Rain must finish resolving"
        );
        assert!(
            matches!(decision.after.waiting_for, WaitingFor::Priority { player } if player == P0),
            "P0 must receive priority after Luminescent Rain resolves (CR 117.3b), got {:?}",
            decision.after.waiting_for
        );
    }

    /// Production path with determinized sampling (`determinization_samples >
    /// 0`): every sampled world simulates each answer it scores, so the chooser
    /// decides under `PromptAnswerValidation::EachCandidate` from the start and
    /// simulates nothing more for the answer it submits. Distant Melody's
    /// creature-type prompt then costs one state copy per offered type per
    /// sampled world, and the AI submits the type, and leaves its RNG where,
    /// the simulate-every-answer gate does.
    ///
    /// Fails if the chooser still simulates the answer it submits (one copy
    /// more).
    #[test]
    fn chooser_scoring_sampled_worlds_does_not_simulate_its_answer_again() {
        const SAMPLES: u32 = 2;
        let prompt = distant_melody_prompt(GameScenario::new());
        let offered = named_options(&prompt).len() as u64;
        let mut config = create_config_for_players(AiDifficulty::Medium, Platform::Native, 2)
            .into_measurement(AI_DECISION_SEED);
        config.search.determinization_samples = SAMPLES;

        let decision = run_ai_decision_with(&prompt, config.clone());
        let reference = {
            let _each_candidate = ChooserSimulatesEachPromptAnswer::enter();
            run_ai_decision_with(&prompt, config)
        };
        assert_eq!(
            reference.clones,
            offered * u64::from(SAMPLES),
            "reach guard: each sampled world copies the state once per offered type"
        );
        assert_eq!(
            decision.clones,
            offered * u64::from(SAMPLES),
            "the chooser must not simulate the answer the sampled worlds already simulated"
        );
        assert!(
            matches!(decision.action, GameAction::ChooseOption { .. }),
            "the AI must answer the creature-type prompt, got {:?}",
            decision.action
        );
        assert_eq!(decision.action, reference.action);
        assert_eq!(decision.next_draw, reference.next_draw);
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
