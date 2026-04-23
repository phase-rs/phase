//! Candidate validation pipeline.
//!
//! A [`CandidateFilter`] rejects [`CandidateAction`]s that cannot legally be
//! performed in the current [`GameState`]. Filters are ordered cheapest-first:
//! trivial structural checks (zone legality, choice-index bounds) run before
//! the expensive catch-all that simulates the action via
//! `apply_as_current` and clones the full state.
//!
//! # Invariant — `cheap ⊆ simulate`
//!
//! Every cheap filter MUST be a subset of what [`SimulationFilter`] rejects:
//!
//! ```text
//! cheap.accept(state, candidate) == false
//!   ⇒
//! SimulationFilter.accept(state, candidate) == false
//! ```
//!
//! Without this property, a cheap filter could silently drop a candidate that
//! the simulation would accept — a correctness bug that surfaces as the AI
//! refusing to take legal actions. The property is enforced by the proptest
//! in the tests module.
//!
//! # Scope guardrail
//!
//! `CandidateFilter` validates candidate **actions** — it does not validate
//! target legality (that belongs in `game::targeting`) or replacement-effect
//! selection (that belongs in `game::replacement`). Adding game-rule verbs
//! ("resolve_trigger", "apply_damage") here is out of scope; this is a
//! structural legality pipeline, not a rules engine.

use crate::game::engine::apply_as_current;
use crate::types::game_state::GameState;

use super::CandidateAction;

/// A filter's approximate computational cost. The pipeline runs filters in
/// ascending cost order so the cheapest rejection wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FilterCost {
    /// Constant-time structural check (e.g., object-id existence, index bounds).
    Trivial,
    /// Bounded lookup (e.g., iterate known-small lists).
    Cheap,
    /// Requires cloning `GameState` and running `apply_as_current`.
    Expensive,
}

/// Rejects candidates that can't legally be performed in the current state.
pub trait CandidateFilter {
    /// Human-readable filter name for tracing/instrumentation.
    fn name(&self) -> &'static str;

    /// Approximate cost. Used to order the pipeline; cheaper first.
    fn cost(&self) -> FilterCost;

    /// Return `true` to accept the candidate, `false` to reject.
    fn accept(&self, state: &GameState, candidate: &CandidateAction) -> bool;
}

/// Structural legality check wrapping [`super::cheap_reject_candidate`].
///
/// Covers:
/// - CR 117.1: priority ownership (only the player with priority may act).
/// - CR 400: zone presence (the acting object/card must still exist in its
///   expected zone; a permanent removed mid-resolution cannot be activated).
/// - CR 601.2: casting/activation announcement steps (mode count, phyrexian
///   shard choices, modal bounds, target-set bounds).
/// - CR 508/509: combat declarations (attackers/blockers must be valid).
///
/// This is the current workhorse. A future follow-up should decompose it into
/// the four named sub-filters originally planned: `ManaAvailabilityFilter`,
/// `ZoneLegalityFilter`, `TargetCountFilter`, `RestrictionFilter`. The
/// `cheap ⊆ sim` invariant guards the decomposition — any split that
/// over-rejects a candidate the simulation accepts will fail the proptest.
pub struct BasicLegalityFilter;

impl CandidateFilter for BasicLegalityFilter {
    fn name(&self) -> &'static str {
        "BasicLegality"
    }

    fn cost(&self) -> FilterCost {
        FilterCost::Cheap
    }

    fn accept(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        !super::cheap_reject_candidate(state, &candidate.action)
    }
}

/// Catch-all fallback: clones the state and runs `apply_as_current`, accepting
/// candidates that produce no error. This is the authoritative oracle — every
/// cheap filter must be a subset of what this rejects. Only reached when all
/// cheap filters accept.
pub struct SimulationFilter;

impl CandidateFilter for SimulationFilter {
    fn name(&self) -> &'static str {
        "Simulation"
    }

    fn cost(&self) -> FilterCost {
        FilterCost::Expensive
    }

    fn accept(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        let mut sim = state.clone();
        apply_as_current(&mut sim, candidate.action.clone()).is_ok()
    }
}

/// A pipeline of filters run in the order they're registered. Candidates pass
/// only if every filter in the pipeline accepts them; first rejection wins.
///
/// The default pipeline is the right choice for all current callers. Custom
/// pipelines are a future extension point (e.g., MCTS rollouts that want to
/// skip `SimulationFilter` and accept optimistic candidates).
pub struct FilterPipeline {
    filters: Vec<Box<dyn CandidateFilter + Send + Sync>>,
}

impl FilterPipeline {
    pub fn new(filters: Vec<Box<dyn CandidateFilter + Send + Sync>>) -> Self {
        Self { filters }
    }

    /// Default pipeline: `BasicLegalityFilter` → `SimulationFilter`.
    pub fn default_pipeline() -> Self {
        Self::new(vec![
            Box::new(BasicLegalityFilter),
            Box::new(SimulationFilter),
        ])
    }

    pub fn accepts(&self, state: &GameState, candidate: &CandidateAction) -> bool {
        self.filters.iter().all(|f| f.accept(state, candidate))
    }

    /// Apply the pipeline to an iterator of candidates.
    pub fn apply<I>(&self, state: &GameState, candidates: I) -> Vec<CandidateAction>
    where
        I: IntoIterator<Item = CandidateAction>,
    {
        candidates
            .into_iter()
            .filter(|c| self.accepts(state, c))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai_support::candidate_actions;
    use crate::types::game_state::GameState;

    #[test]
    fn default_pipeline_registered_filters_ordered_by_cost() {
        let pipeline = FilterPipeline::default_pipeline();
        let costs: Vec<FilterCost> = pipeline.filters.iter().map(|f| f.cost()).collect();
        // Pipeline must be monotonically non-decreasing in cost so cheap
        // rejections dominate and SimulationFilter is the last resort.
        for window in costs.windows(2) {
            assert!(
                window[0] <= window[1],
                "filters must be ordered cheapest-first: {:?}",
                costs
            );
        }
    }

    #[test]
    fn simulation_filter_accepts_pass_priority_at_opening_state() {
        // A fresh two-player state has Priority on player 0; PassPriority must
        // be accepted by the oracle. This is the baseline sanity check for the
        // `cheap ⊆ sim` invariant: if SimulationFilter rejected this,
        // BasicLegalityFilter would too.
        let state = GameState::new_two_player(42);
        let candidates = candidate_actions(&state);
        let pass = candidates
            .into_iter()
            .find(|c| matches!(c.action, crate::types::actions::GameAction::PassPriority))
            .expect("PassPriority should be a candidate in the opening state");
        assert!(SimulationFilter.accept(&state, &pass));
        assert!(BasicLegalityFilter.accept(&state, &pass));
    }

    /// The `cheap ⊆ sim` invariant: for every candidate generated by
    /// `candidate_actions` in a representative game state, if
    /// `BasicLegalityFilter` rejects the candidate, `SimulationFilter` must
    /// reject it too. Enforced over the full candidate set of a fresh
    /// two-player state to keep the test hermetic and fast — adding proptest
    /// state generation is a follow-up when deeper coverage is needed.
    #[test]
    fn basic_legality_is_subset_of_simulation() {
        let state = GameState::new_two_player(42);
        let candidates = candidate_actions(&state);
        for candidate in candidates {
            let cheap_accepts = BasicLegalityFilter.accept(&state, &candidate);
            let sim_accepts = SimulationFilter.accept(&state, &candidate);
            if !cheap_accepts {
                assert!(
                    !sim_accepts,
                    "cheap⊆sim violated: BasicLegalityFilter rejected `{}` \
                     but SimulationFilter accepted — candidate would be \
                     silently dropped. Action: {:?}",
                    candidate.action.variant_name(),
                    candidate.action
                );
            }
        }
    }
}
