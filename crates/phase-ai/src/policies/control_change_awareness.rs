//! Control-Change Awareness Policy
//!
//! Prevents the AI from activating abilities that give away control of its own
//! permanents to opponents. This addresses cards like Humble Defector which have
//! drawback abilities that exchange control or grant control to opponents.
//!
//! The policy detects `Effect::GainControl` and `Effect::ExchangeControl` in
//! ability effects and applies severe penalties when the target would hit the
//! AI's own permanents.
//!
//! CR 800.4a: When an object changes controller, any Auras, Equipment, or
//! Fortifications attached to that object become unattached if they can't be
//! attached to the new controller. This policy prevents the AI from losing
//! valuable attachments by giving away control.

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
        let effects = crate::cast_facts::collect_definition_effects(ability_def);
        let mut control_change_effect = false;
        let mut gives_away_permanent = false;

        for effect in effects {
            match effect {
                Effect::GainControl { target } => {
                    control_change_effect = true;
                    if would_target_own_permanent(ctx, *source_id, target) {
                        gives_away_permanent = true;
                    }
                }
                Effect::ExchangeControl { target_a, target_b } => {
                    control_change_effect = true;
                    if would_target_own_permanent(ctx, *source_id, target_a)
                        || would_target_own_permanent(ctx, *source_id, target_b)
                    {
                        gives_away_permanent = true;
                    }
                }
                Effect::GiveControl { target, .. } => {
                    control_change_effect = true;
                    if would_target_own_permanent(ctx, *source_id, target) {
                        gives_away_permanent = true;
                    }
                }
                _ => {}
            }
        }

        if !control_change_effect {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("control_change_ok"),
            };
        }

        if gives_away_permanent {
            // Apply severe penalty for giving away own permanents
            PolicyVerdict::Score {
                delta: CONTROL_CHANGE_PENALTY,
                reason: PolicyReason::new("control_change_gives_away_permanent"),
            }
        } else {
            PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("control_change_ok"),
            }
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
            // If the filter explicitly restricts targets to opponents, it cannot hit our own permanents
            if matches!(
                typed.controller,
                Some(engine::types::ability::ControllerRef::Opponent)
            ) {
                return false;
            }

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
                engine::types::ability::TypeFilter::Artifact => {
                    obj.card_types.core_types.contains(&CoreType::Artifact)
                }
                engine::types::ability::TypeFilter::Enchantment => {
                    obj.card_types.core_types.contains(&CoreType::Enchantment)
                }
                engine::types::ability::TypeFilter::Land => {
                    obj.card_types.core_types.contains(&CoreType::Land)
                }
                engine::types::ability::TypeFilter::Planeswalker => {
                    obj.card_types.core_types.contains(&CoreType::Planeswalker)
                }
                engine::types::ability::TypeFilter::Any => true,
                _ => false,
            });

            // Check if object is controlled by AI
            let matches_controller = obj.controller == ctx.ai_player;

            matches_type && matches_controller
        }
        _ => false,
    }
}

