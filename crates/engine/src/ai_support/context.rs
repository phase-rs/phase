use crate::types::game_state::{GameState, WaitingFor};

use super::candidates::{candidate_actions, CandidateAction};

#[derive(Debug, Clone)]
pub struct AiDecisionContext {
    pub waiting_for: WaitingFor,
    pub candidates: Vec<CandidateAction>,
}

pub fn build_decision_context(state: &GameState) -> AiDecisionContext {
    AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates: candidate_actions(state),
    }
}
