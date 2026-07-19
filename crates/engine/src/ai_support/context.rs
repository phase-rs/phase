use crate::types::game_state::{GameState, WaitingFor};

use super::candidates::{candidate_actions, CandidateAction};

#[derive(Debug, Clone)]
pub struct AiDecisionContext {
    pub waiting_for: WaitingFor,
    pub candidates: Vec<CandidateAction>,
}

pub fn build_decision_context(state: &GameState) -> AiDecisionContext {
    // Issue #4878: sort via the same `GameAction::cmp_stable` total order
    // `validated_candidate_actions_with_probe` uses (ai_support/mod.rs), so
    // this context's candidates don't carry raw enumeration-order variance
    // into phase-ai's decision loop. Intentionally does NOT switch to
    // `validated_candidate_actions` — that also runs the FilterPipeline,
    // which would change candidate semantics, not just their order.
    let mut candidates = candidate_actions(state);
    candidates.sort_by(|a, b| a.action.cmp_stable(&b.action));
    AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates,
    }
}
