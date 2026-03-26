mod candidates;
mod context;

use crate::game::engine::apply;
use crate::game::mana_abilities;
use crate::game::mana_sources;
use crate::types::ability::AbilityKind;
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, WaitingFor};

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

/// Returns the legal actions for the current game state.
///
/// `TapLandForMana`/`UntapLandForMana` actions are filtered out — the frontend
/// derives land tappability from game state. Non-land mana abilities (dorks,
/// artifacts) are included so the frontend auto-pass system knows meaningful
/// actions exist. The AI uses `candidate_actions()` which excludes mana abilities
/// from priority candidates to keep the search tree clean.
pub fn legal_actions(state: &GameState) -> Vec<GameAction> {
    let mut actions: Vec<GameAction> = validated_candidate_actions(state)
        .into_iter()
        .map(|candidate| candidate.action)
        .filter(|action| !action.is_mana_ability())
        .collect();

    // CR 605.3a: Append activatable mana abilities so the frontend knows the player
    // has meaningful actions beyond PassPriority. These are excluded from
    // candidate_actions() to keep the AI search tree clean (see candidates.rs
    // priority_actions), but the frontend needs them to avoid incorrect auto-pass.
    actions.extend(activatable_mana_ability_actions(state));

    actions
}

/// CR 605.1b: Enumerate activatable mana abilities for the priority player.
///
/// Mirrors the per-ability scan pattern in `mana_sources::scan_mana_abilities` rather
/// than using the single `mana_ability_index` derived field, since a permanent may have
/// multiple mana abilities. Per-ability tap/sickness guards match `scan_mana_abilities`:
/// only abilities with a tap cost component require the permanent to be untapped and
/// free of summoning sickness (CR 302.6). Mana abilities don't use the stack (CR 605.3a).
fn activatable_mana_ability_actions(state: &GameState) -> Vec<GameAction> {
    let player = match &state.waiting_for {
        WaitingFor::Priority { player } => *player,
        _ => return Vec::new(),
    };

    let mut actions = Vec::new();
    for &obj_id in &state.battlefield {
        let Some(obj) = state.objects.get(&obj_id) else {
            continue;
        };
        if obj.controller != player || !obj.has_mana_ability {
            continue;
        }
        for (idx, ability) in obj.abilities.iter().enumerate() {
            if ability.kind != AbilityKind::Activated || !mana_abilities::is_mana_ability(ability) {
                continue;
            }
            // CR 302.6: Only tap-cost abilities are gated by tapped state and summoning
            // sickness. Free or mana-cost-only mana abilities are always activatable.
            if mana_sources::has_tap_component(&ability.cost)
                && (obj.tapped || obj.has_summoning_sickness)
            {
                continue;
            }
            // CR 605.3b: Activation restrictions still apply to mana abilities.
            if mana_sources::activation_condition_satisfied(state, player, obj_id, idx, ability) {
                actions.push(GameAction::ActivateAbility {
                    source_id: obj_id,
                    ability_index: idx,
                });
            }
        }
    }
    actions
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
