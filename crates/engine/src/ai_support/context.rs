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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::{
        actions::GameAction, card_type::CoreType, identifiers::CardId, player::PlayerId,
        zones::Zone, Phase,
    };

    /// Issue #4878: the decision context is consumed directly by phase-ai, so
    /// it must canonicalize candidate enumeration order before trajectories
    /// score tied actions. The hand deliberately enumerates the two land
    /// actions in descending object-id order; removing the context sort makes
    /// this assertion fail while tests for other candidate consumers still pass.
    #[test]
    fn build_decision_context_canonicalizes_candidate_action_order() {
        let player = PlayerId(0);
        let mut state = GameState::new_two_player(42);
        state.phase = Phase::PreCombatMain;
        state.active_player = player;
        state.priority_player = player;
        state.waiting_for = WaitingFor::Priority { player };

        let first_land = create_object(
            &mut state,
            CardId(1),
            player,
            "First Land".to_string(),
            Zone::Hand,
        );
        let second_land = create_object(
            &mut state,
            CardId(2),
            player,
            "Second Land".to_string(),
            Zone::Hand,
        );
        for object_id in [first_land, second_land] {
            state
                .objects
                .get_mut(&object_id)
                .expect("created land must exist")
                .card_types
                .core_types
                .push(CoreType::Land);
        }
        state.players[0].hand = [second_land, first_land].into_iter().collect();

        let context = build_decision_context(&state);
        let land_actions: Vec<_> = context
            .candidates
            .iter()
            .filter_map(|candidate| match &candidate.action {
                GameAction::PlayLand { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect();

        assert_eq!(land_actions, vec![first_land, second_land]);
    }
}
