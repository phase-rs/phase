mod candidates;
mod context;

use crate::types::actions::GameAction;
use crate::types::game_state::GameState;

pub use candidates::{candidate_actions, ActionMetadata, CandidateAction, TacticalClass};
pub use context::{build_decision_context, AiDecisionContext};

pub fn legal_actions(state: &GameState) -> Vec<GameAction> {
    candidate_actions(state)
        .into_iter()
        .map(|candidate| candidate.action)
        .collect()
}
