mod candidates;
mod context;

use std::collections::HashMap;

use crate::game::engine::apply;
use crate::game::mana_abilities;
use crate::game::mana_sources;
use crate::types::ability::AbilityKind;
use crate::types::actions::GameAction;
use crate::types::card_type::CoreType;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;

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
/// Determines whether the frontend should auto-pass the current priority window.
///
/// Returns `true` when auto-passing is recommended:
/// - Only `PassPriority` is available (no spells, abilities, or lands to play)
/// - Player's own spell/ability is on top of the stack (MTGA-style: let your
///   own spells resolve without pausing)
///
/// This centralizes the "meaningful action" classification in the engine so
/// frontends don't need to inspect game objects or card types.
pub fn auto_pass_recommended(state: &GameState, actions: &[GameAction]) -> bool {
    let player = match &state.waiting_for {
        WaitingFor::Priority { player } => *player,
        _ => return false,
    };

    // Meaningful = any action that directly affects the game beyond passing.
    // Land mana abilities (ActivateAbility on a Land) are NOT meaningful on their
    // own — they only matter if the mana enables casting a spell, in which case
    // CastSpell will also be present in `actions` (the engine's can_pay_cost_after_auto_tap
    // already simulates tapping those lands when checking spell castability).
    let has_meaningful = actions.iter().any(|a| {
        match a {
            GameAction::PassPriority => false,
            GameAction::ActivateAbility { source_id, .. } => {
                // Non-land activated abilities (creatures, artifacts) are meaningful.
                // Land activated abilities are only present as mana-producing fallbacks
                // and are not meaningful on their own.
                state
                    .objects
                    .get(source_id)
                    .is_some_and(|obj| !obj.card_types.core_types.contains(&CoreType::Land))
            }
            _ => true,
        }
    });
    if !has_meaningful {
        return true;
    }

    // MTGA-style: auto-pass when own spell/ability is on top of the stack.
    // The player almost never wants to respond to their own spell — let it resolve.
    // Full control mode (checked by the frontend) overrides this.
    if let Some(top) = state.stack.last() {
        if top.controller == player {
            return true;
        }
    }

    false
}

pub fn legal_actions(state: &GameState) -> Vec<GameAction> {
    legal_actions_with_costs(state).0
}

/// Returns legal actions plus effective mana costs for castable spells.
///
/// The spell costs map contains the post-reduction effective cost for each
/// CastSpell action's object_id, reflecting all modifiers (alt costs, commander
/// tax, battlefield reducers, affinity). Frontends use this to display dynamic
/// mana cost overlays on cards in hand.
pub fn legal_actions_with_costs(state: &GameState) -> (Vec<GameAction>, HashMap<ObjectId, ManaCost>) {
    let mut actions: Vec<GameAction> = validated_candidate_actions(state)
        .into_iter()
        .map(|candidate| candidate.action)
        .filter(|action| !action.is_mana_ability())
        .collect();

    // Build spell costs map from CastSpell actions.
    let mut spell_costs = HashMap::new();
    if let WaitingFor::Priority { player } = &state.waiting_for {
        for action in &actions {
            if let GameAction::CastSpell { object_id, .. } = action {
                if let Some(cost) = crate::game::casting::effective_spell_cost(state, *player, *object_id) {
                    spell_costs.insert(*object_id, cost);
                }
            }
        }
    }

    // CR 605.3a: Append activatable mana abilities so the frontend knows the player
    // has meaningful actions beyond PassPriority. These are excluded from
    // candidate_actions() to keep the AI search tree clean (see candidates.rs
    // priority_actions), but the frontend needs them to avoid incorrect auto-pass.
    actions.extend(activatable_mana_ability_actions(state));

    (actions, spell_costs)
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
            if mana_sources::activation_condition_satisfied(state, player, obj_id, idx, ability)
                && mana_abilities::can_activate_mana_ability_now(state, player, obj_id, ability)
            {
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
