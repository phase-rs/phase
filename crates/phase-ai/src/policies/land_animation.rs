//! Land Animation Timing Policy
//!
//! Evaluates when to animate man-lands like Lumbering Falls. Prevents the AI from
//! animating lands every turn regardless of strategic value, considering mana needs,
//! color requirements, and combat value.

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;
use engine::game::game_object;

/// Penalty for animating a land when mana is needed for other spells.
const MANA_NEEDED_PENALTY: f64 = -2.0;

/// Penalty for animating a tapped land (can't animate tapped lands).
const TAPPED_LAND_PENALTY: f64 = -100.0;

/// Bonus for animating when sufficient alternative mana sources exist.
const SUFFICIENT_MANA_BONUS: f64 = 0.3;

pub struct LandAnimationPolicy;

impl TacticalPolicy for LandAnimationPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LandAnimation
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_na"),
            };
        };

        // Get the ability definition
        let Some(obj) = ctx.state.objects.get(source_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_na"),
            };
        };

        // Check if this is a land
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_not_land"),
            };
        }

        let Some(ability_def) = obj.abilities.get(*ability_index) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_na"),
            };
        };

        // Check if the ability adds Creature type (animation)
        let adds_creature_type = match &*ability_def.effect {
            engine::types::ability::Effect::Animate { .. } => true,
            engine::types::ability::Effect::GenericEffect {
                static_abilities, ..
            } => static_abilities.iter().any(|s| {
                s.modifications.iter().any(|m| {
                    matches!(
                        m,
                        engine::types::ability::ContinuousModification::AddType {
                            core_type: engine::types::card_type::CoreType::Creature
                        }
                    )
                })
            }),
            _ => false,
        };

        if !adds_creature_type {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_not_animation"),
            };
        }

        let mut delta = 0.0;

        // Penalize if the land is tapped (can't animate tapped lands)
        // CR 302.5: A permanent that’s tapped can’t activate abilities unless the ability
        // or another effect specifically allows it.
        if obj.tapped {
            return PolicyVerdict::Score {
                delta: TAPPED_LAND_PENALTY,
                reason: PolicyReason::new("land_animation_tapped"),
            };
        }

        // Check if this is the only source of a critical color
        let is_critical_color_source = is_only_source_of_color(ctx, *source_id);
        if is_critical_color_source {
            delta += MANA_NEEDED_PENALTY;
        }

        // Check if mana is needed for spells in hand
        let mana_needed = mana_needed_in_hand(ctx);
        if mana_needed {
            delta += MANA_NEEDED_PENALTY;
        }

        // Bonus if sufficient alternative mana sources exist
        let sufficient_mana = has_sufficient_mana_sources(ctx, *source_id);
        if sufficient_mana {
            delta += SUFFICIENT_MANA_BONUS;
        }

        PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new("land_animation_score"),
        }
    }
}

/// Check if this land is the only source of a critical color for the AI.
fn is_only_source_of_color(ctx: &PolicyContext<'_>, land_id: ObjectId) -> bool {
    let Some(land) = ctx.state.objects.get(&land_id) else {
        return false;
    };

    // Get colors this land can produce
    let land_colors = colors_produced_by_land(land);

    // For each color, check if this is the only source
    for color in land_colors {
        let other_sources = ctx
            .state
            .battlefield
            .iter()
            .filter(|&&id| {
                id != land_id && {
                    let Some(obj) = ctx.state.objects.get(&id) else {
                        return false;
                    };
                    obj.controller == ctx.ai_player
                        && obj.card_types.core_types.contains(&CoreType::Land)
                        && !obj.tapped
                        && colors_produced_by_land(obj).contains(&color)
                }
            })
            .count();

        if other_sources == 0 {
            return true;
        }
    }

    false
}

/// Get the colors a land can produce.
fn colors_produced_by_land(land: &game_object::GameObject) -> Vec<engine::types::mana::ManaColor> {
    use engine::types::ability::ManaProduction;
    let mut colors = Vec::new();
    for ability in land.abilities.iter() {
        if let engine::types::ability::Effect::Mana { produced, .. } = &*ability.effect {
            match produced {
                ManaProduction::Fixed {
                    colors: produced_colors,
                    ..
                } => {
                    colors.extend(produced_colors.clone());
                }
                ManaProduction::Mixed {
                    colors: produced_colors,
                    ..
                } => {
                    colors.extend(produced_colors.clone());
                }
                ManaProduction::AnyOneColor { color_options, .. } => {
                    colors.extend(color_options.clone());
                }
                ManaProduction::AnyCombination { color_options, .. } => {
                    colors.extend(color_options.clone());
                }
                ManaProduction::ChosenColor {
                    fixed_alternative, ..
                } => {
                    if let Some(c) = land.chosen_color() {
                        colors.push(c);
                    }
                    if let Some(c) = fixed_alternative {
                        colors.push(*c);
                    }
                }
                _ => {}
            }
        }
    }
    colors
}

/// Check if the AI needs mana for spells in hand.
fn mana_needed_in_hand(ctx: &PolicyContext<'_>) -> bool {
    // Check if AI has spells in hand that require mana
    let has_spells = ctx.state.players[ctx.ai_player.0 as usize]
        .hand
        .iter()
        .any(|&object_id| {
            let Some(obj) = ctx.state.objects.get(&object_id) else {
                return false;
            };
            // Simple heuristic: if object has a mana cost, AI needs mana
            obj.mana_cost.mana_value() > 0
        });

    // Check if AI has untapped mana sources
    let has_untapped_mana = ctx.state.battlefield.iter().any(|&id| {
        let Some(obj) = ctx.state.objects.get(&id) else {
            return false;
        };
        obj.controller == ctx.ai_player
            && obj.card_types.core_types.contains(&CoreType::Land)
            && !obj.tapped
    });

    has_spells && !has_untapped_mana
}

/// Check if the AI has sufficient alternative mana sources.
fn has_sufficient_mana_sources(ctx: &PolicyContext<'_>, exclude_land: ObjectId) -> bool {
    let land_count = ctx
        .state
        .battlefield
        .iter()
        .filter(|&&id| {
            id != exclude_land && {
                let Some(obj) = ctx.state.objects.get(&id) else {
                    return false;
                };
                obj.controller == ctx.ai_player
                    && obj.card_types.core_types.contains(&CoreType::Land)
            }
        })
        .count();

    land_count >= 3 // Heuristic: need at least 3 other lands
}

