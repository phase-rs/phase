use engine::game::DeckEntry;

use crate::deck_profile::DeckProfile;
use crate::eval::EvalWeightSet;
use crate::synergy::SynergyGraph;

/// Pre-computed deck analysis, built once per game from the deck pool.
/// Threaded through `PlannerServices` into eval, policies, and search.
///
/// When no deck data is available (e.g., tests, non-deck games), use
/// `AiContext::empty()` which provides neutral defaults that produce
/// identical behavior to the pre-context-aware AI.
#[derive(Debug, Clone)]
pub struct AiContext {
    pub deck_profile: DeckProfile,
    pub synergy_graph: SynergyGraph,
    pub adjusted_weights: EvalWeightSet,
}

impl AiContext {
    /// Analyze a deck list to build the context.
    pub fn analyze(deck: &[DeckEntry], base_weights: &EvalWeightSet) -> Self {
        let deck_profile = DeckProfile::analyze(deck);
        let synergy_graph = SynergyGraph::build(deck);
        let adjusted_weights = EvalWeightSet {
            early: deck_profile.adjust_weights(&base_weights.early),
            mid: deck_profile.adjust_weights(&base_weights.mid),
            late: deck_profile.adjust_weights(&base_weights.late),
        };
        Self {
            deck_profile,
            synergy_graph,
            adjusted_weights,
        }
    }

    /// Neutral context for when no deck data is available.
    /// Strategic dimensions contribute 0.0, weights are unchanged from base.
    pub fn empty(base_weights: &EvalWeightSet) -> Self {
        Self {
            deck_profile: DeckProfile::default(),
            synergy_graph: SynergyGraph::empty(),
            adjusted_weights: base_weights.clone(),
        }
    }
}
