mod candidates;
mod context;

use crate::game::engine::apply;
use crate::types::actions::GameAction;
use crate::types::game_state::GameState;

pub use candidates::{candidate_actions, ActionMetadata, CandidateAction, TacticalClass};
pub use context::{build_decision_context, AiDecisionContext};

pub fn validated_candidate_actions(state: &GameState) -> Vec<CandidateAction> {
    candidate_actions(state)
        .into_iter()
        .filter(|candidate| {
            let mut sim = state.clone();
            apply(&mut sim, candidate.action.clone()).is_ok()
        })
        .collect()
}

/// Returns the legal actions for the current game state, excluding mana abilities.
///
/// Mana abilities (CR 605.3a) are omitted because they do not represent meaningful
/// priority decisions — the frontend derives land tappability from game state.
/// The AI uses `candidate_actions()` which includes them.
pub fn legal_actions(state: &GameState) -> Vec<GameAction> {
    validated_candidate_actions(state)
        .into_iter()
        .map(|candidate| candidate.action)
        .filter(|action| !action.is_mana_ability())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{candidate_actions, legal_actions, validated_candidate_actions};
    use crate::types::actions::GameAction;
    use crate::types::game_state::{GameState, WaitingFor};
    use crate::types::player::PlayerId;

    #[test]
    fn legal_actions_filter_out_reducer_illegal_priority_candidates() {
        let mut state = GameState::new_two_player(42);
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let raw_candidates = candidate_actions(&state);
        assert!(raw_candidates
            .iter()
            .any(|candidate| { matches!(candidate.action, GameAction::PassPriority) }));

        let validated_candidates = validated_candidate_actions(&state);
        assert!(validated_candidates.is_empty());
        assert!(legal_actions(&state).is_empty());
    }

    #[test]
    fn legal_actions_preserve_reducer_legal_priority_candidates() {
        let state = GameState::new_two_player(42);

        let validated_candidates = validated_candidate_actions(&state);
        assert!(validated_candidates
            .iter()
            .any(|candidate| { matches!(candidate.action, GameAction::PassPriority) }));

        let actions = legal_actions(&state);
        assert!(actions
            .iter()
            .any(|action| matches!(action, GameAction::PassPriority)));
    }
}
