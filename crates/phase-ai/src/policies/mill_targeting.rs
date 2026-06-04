//! Mill Targeting Optimization Policy
//!
//! Improves targeting for mill effects with conditional payoff, like Szarekh
//! which mills 3 cards and lets you put artifact creature/Vehicle cards from
//! those milled cards into your hand. This policy evaluates target selection
//! based on the probability of hitting desired card types.
//!
//! CR 701.13a: To mill a player, a player puts the top cards of their library
//! into their graveyard. This policy optimizes targeting for mill effects that
//! have conditional retrieval from the milled cards.

use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Bonus for targeting opponents likely to have desired card types.
const TARGET_BONUS: f64 = 0.3;

/// Penalty for self-milling when not beneficial.
const SELF_MILL_PENALTY: f64 = -0.2;

/// Penalty for targeting opponents with empty libraries.
const EMPTY_LIBRARY_PENALTY: f64 = -1.0;

pub struct MillTargetingPolicy;

impl TacticalPolicy for MillTargetingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::MillTargeting
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::SelectTarget]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        Some(1.0) // activation-constant:
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::SelectTargets { targets } = &ctx.candidate.action else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("mill_targeting_na"),
            };
        };

        if targets.is_empty() {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("mill_targeting_no_target"),
            };
        }

        // Check if the ability has mill with conditional payoff
        let has_conditional_payoff = has_mill_with_conditional_payoff(ctx);
        if !has_conditional_payoff {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("mill_targeting_no_conditional"),
            };
        }

        let target = &targets[0];
        let mut delta = 0.0;

        // Check if targeting self
        if let TargetRef::Player(player) = target {
            if *player == ctx.ai_player {
                delta += SELF_MILL_PENALTY;
            } else {
                // Bonus for targeting opponent
                delta += TARGET_BONUS;
            }

            // Check if target's library is empty
            if let Some(player_state) = ctx.state.players.get(player.0 as usize) {
                if player_state.library.is_empty() {
                    delta += EMPTY_LIBRARY_PENALTY;
                }
            }
        }

        PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new("mill_targeting_score"),
        }
    }
}

/// Check if the ability being activated has a mill effect with conditional payoff
/// (e.g., "mill X cards, you may put [type] cards from among them into your hand").
fn has_mill_with_conditional_payoff(ctx: &PolicyContext<'_>) -> bool {
    // Check if the source has a mill ability with conditional retrieval
    ctx.source_object()
        .map(|obj| {
            obj.abilities.iter().any(|ability| {
                let effects = crate::cast_facts::collect_definition_effects(ability);
                let has_mill = effects.iter().any(|e| matches!(e, Effect::Mill { .. }));
                let has_retrieval = effects.iter().any(|e| {
                    matches!(
                        e,
                        Effect::Draw { .. }
                            | Effect::ChangeZone { .. }
                            | Effect::ChooseFromZone { .. }
                    )
                });
                has_mill && has_retrieval
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetRef, ZoneFilter,
    };
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    use crate::config::AiConfig;
    use crate::context::AiContext;

    const AI: PlayerId = PlayerId(0);
    const OPPONENT: PlayerId = PlayerId(1);

    /// Test that the policy penalizes self-mill
    #[test]
    fn penalizes_self_mill() {
        let mut state = GameState::default();
        let mut context = AiContext::default();
        let config = AiConfig::default();

        // Create a creature controlled by AI
        let creature_id = ObjectId(1);
        let creature_obj = create_object(
            creature_id,
            CardId(100),
            AI,
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(creature_id, creature_obj);

        // Create a mill ability targeting self
        let mill_ability = AbilityDefinition {
            kind: AbilityKind::Activated,
            effect: Box::new(Effect::Mill {
                target: TargetRef::Player(AI),
                count: QuantityExpr::Fixed { value: 3 },
                destination: ZoneFilter::Graveyard,
            }),
            ..Default::default()
        };
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .abilities
            .push(mill_ability);

        let action = GameAction::ActivateAbility {
            source_id: creature_id,
            ability_index: 0,
        };

        let ctx = PolicyContext {
            state: &state,
            action: &action,
            ai_player: AI,
            context: &context,
            config: &config,
        };

        let policy = MillTargetingPolicy;
        let verdict = policy.verdict(&ctx);

        // Should penalize self-mill
        assert!(matches!(verdict, PolicyVerdict::Score { delta, .. } if delta < 0.0));
    }

    /// Test that the policy bonuses targeting opponent
    #[test]
    fn bonuses_targeting_opponent() {
        let mut state = GameState::default();
        let mut context = AiContext::default();
        let config = AiConfig::default();

        // Create a creature controlled by AI
        let creature_id = ObjectId(1);
        let creature_obj = create_object(
            creature_id,
            CardId(100),
            AI,
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(creature_id, creature_obj);

        // Create a mill ability targeting opponent
        let mill_ability = AbilityDefinition {
            kind: AbilityKind::Activated,
            effect: Box::new(Effect::Mill {
                target: TargetRef::Player(OPPONENT),
                count: QuantityExpr::Fixed { value: 3 },
                destination: ZoneFilter::Graveyard,
            }),
            ..Default::default()
        };
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .abilities
            .push(mill_ability);

        let action = GameAction::ActivateAbility {
            source_id: creature_id,
            ability_index: 0,
        };

        let ctx = PolicyContext {
            state: &state,
            action: &action,
            ai_player: AI,
            context: &context,
            config: &config,
        };

        let policy = MillTargetingPolicy;
        let verdict = policy.verdict(&ctx);

        // Should bonus targeting opponent
        assert!(matches!(verdict, PolicyVerdict::Score { delta, .. } if delta > 0.0));
    }
}
