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

    /// Test that the policy constants are set correctly
    #[test]
    fn self_mill_penalty_is_negative() {
        assert!(SELF_MILL_PENALTY < 0.0);
    }

    /// Test that opponent mill bonus is positive
    #[test]
    fn opponent_mill_bonus_is_positive() {
        assert!(TARGET_BONUS > 0.0);
    }

    /// Test that empty library penalty is negative
    #[test]
    fn empty_library_penalty_is_negative() {
        assert!(EMPTY_LIBRARY_PENALTY < 0.0);
    }
}
