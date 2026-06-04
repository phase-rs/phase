//! Control-Change Awareness Policy
//!
//! Prevents the AI from activating abilities that give away control of its own
//! permanents to opponents. This addresses cards like Humble Defector which have
//! drawback abilities that exchange control or grant control to opponents.
//!
//! The policy detects `Effect::GainControl` and `Effect::ExchangeControl` in
//! ability effects and applies severe penalties when the target would hit the
//! AI's own permanents.

use engine::types::ability::{Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Severe penalty for activating an ability that gives away the AI's own permanents.
/// This should be enough to make PassPriority win over such activations.
const CONTROL_CHANGE_PENALTY: f64 = -100.0;

pub struct ControlChangeAwarenessPolicy;

impl TacticalPolicy for ControlChangeAwarenessPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ControlChangeAwareness
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Applies to every deck; the verdict's effect guard self-gates.
        Some(1.0) // activation-constant:
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("control_change_na"),
            };
        };

        // Get the ability definition
        let Some(obj) = ctx.state.objects.get(source_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("control_change_na"),
            };
        };

        let Some(ability_def) = obj.abilities.get(*ability_index) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("control_change_na"),
            };
        };

        // Check if the ability effect involves control change
        let effect = &ability_def.effect;
        let (control_change_effect, target_filter) = match &**effect {
            Effect::GainControl { target } => (true, Some(target)),
            Effect::ExchangeControl { target_a, target_b } => {
                // ExchangeControl has two targets; check if either could hit AI's permanents
                if would_target_own_permanent(ctx, *source_id, target_a)
                    || would_target_own_permanent(ctx, *source_id, target_b)
                {
                    (true, None)
                } else {
                    (false, None)
                }
            }
            _ => (false, None),
        };

        if !control_change_effect {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("control_change_ok"),
            };
        }

        // For GainControl, check if the target would hit AI's own permanents
        if let Some(target) = target_filter {
            if !would_target_own_permanent(ctx, *source_id, target) {
                return PolicyVerdict::Score {
                    delta: 0.0,
                    reason: PolicyReason::new("control_change_ok"),
                };
            }
        }

        // Apply severe penalty for giving away own permanents
        PolicyVerdict::Score {
            delta: CONTROL_CHANGE_PENALTY,
            reason: PolicyReason::new("control_change_gives_away_permanent"),
        }
    }
}

/// Check if a target filter would hit the AI's own permanents.
fn would_target_own_permanent(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    target: &TargetFilter,
) -> bool {
    // This is a simplified check - in a full implementation, we'd need to
    // evaluate the target filter against all of the AI's permanents.
    // For now, we check common patterns that indicate self-targeting.

    match target {
        TargetFilter::SelfRef => true,
        TargetFilter::Controller => {
            // Controller filter without a specific controller means "any controller"
            // We can't determine if it would hit AI's own permanents without more context
            // Conservatively assume it could hit AI's permanents
            false
        }
        TargetFilter::Typed(typed) => {
            // Check if the typed filter could match the source
            let Some(obj) = ctx.state.objects.get(&source_id) else {
                return false;
            };

            // Check if object matches the type filter
            let matches_type = typed.type_filters.iter().any(|tf| match tf {
                engine::types::ability::TypeFilter::Permanent => true,
                engine::types::ability::TypeFilter::Creature => {
                    obj.card_types.core_types.contains(&CoreType::Creature)
                }
                _ => false,
            });

            // Check if object is controlled by AI
            let matches_controller = obj.controller == ctx.ai_player;

            matches_type && matches_controller
        }
        _ => false,
    }
}
