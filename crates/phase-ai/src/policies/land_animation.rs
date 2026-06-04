//! Land Animation Timing Policy
//!
//! Evaluates when to animate man-lands like Lumbering Falls. Prevents the AI from
//! animating lands every turn regardless of strategic value, considering mana needs,
//! color requirements, and combat value.

use engine::game::game_object;
use engine::types::ability::{
    AbilityDefinition, ContinuousModification, CostCategory, Effect, ManaProduction,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Penalty for animating a land when mana is needed for other spells.
const MANA_NEEDED_PENALTY: f64 = -2.0;

/// Penalty for animation abilities whose cost needs an untapped source.
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

        if !ability_animates_land(ability_def) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_not_animation"),
            };
        }

        let mut delta = 0.0;

        // CR 107.5: A permanent that's already tapped can't be tapped again to pay a cost.
        if obj.tapped && ability_taps_source(ability_def) {
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
    let mut colors = Vec::new();
    for ability in land.abilities.iter() {
        if let Effect::Mana { produced, .. } = &*ability.effect {
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

fn ability_animates_land(ability: &AbilityDefinition) -> bool {
    crate::cast_facts::collect_definition_effects(ability)
        .into_iter()
        .any(effect_animates_land)
}

fn effect_animates_land(effect: &Effect) -> bool {
    match effect {
        Effect::Animate { .. } => true,
        Effect::GenericEffect {
            static_abilities, ..
        } => static_abilities.iter().any(|static_ability| {
            static_ability
                .modifications
                .iter()
                .any(modification_adds_creature_type)
        }),
        _ => false,
    }
}

fn modification_adds_creature_type(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddType {
            core_type: CoreType::Creature
        }
    )
}

fn ability_taps_source(ability: &AbilityDefinition) -> bool {
    ability.cost.as_ref().is_some_and(|cost| {
        cost.categories()
            .into_iter()
            .any(|category| category == CostCategory::TapsSelf)
    })
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityKind, QuantityExpr, StaticDefinition, TargetFilter};
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaColor;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    const AI: PlayerId = PlayerId(0);

    fn mana_effect(colors: Vec<ManaColor>) -> Effect {
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors,
                contribution: Default::default(),
            },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }

    fn animate_effect() -> Effect {
        Effect::Animate {
            power: Some(2),
            toughness: Some(2),
            types: vec!["Creature".to_string()],
            remove_types: Vec::new(),
            target: TargetFilter::SelfRef,
            keywords: Vec::new(),
        }
    }

    fn generic_creature_type_effect() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::Continuous).modifications(
                vec![ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }],
            )],
            duration: None,
            target: Some(TargetFilter::SelfRef),
        }
    }

    fn land_with_ability(state: &mut GameState, ability: AbilityDefinition) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            AI,
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn policy_verdict(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        LandAnimationPolicy.verdict(&ctx)
    }

    fn assert_score(verdict: PolicyVerdict, expected_reason: &str) {
        let PolicyVerdict::Score { delta, reason } = verdict else {
            panic!("expected score verdict");
        };
        assert_eq!(delta, 0.0);
        assert_eq!(reason.kind, expected_reason);
    }

    #[test]
    fn mana_ability_on_land_is_not_animation() {
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(
            &mut state,
            AbilityDefinition::new(AbilityKind::Activated, mana_effect(vec![ManaColor::Green])),
        );

        assert_score(
            policy_verdict(&state, source_id),
            "land_animation_not_animation",
        );
    }

    #[test]
    fn ability_animates_land_walks_sub_ability_chain() {
        let mut ability =
            AbilityDefinition::new(AbilityKind::Activated, mana_effect(vec![ManaColor::Green]));
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            animate_effect(),
        )));

        assert!(ability_animates_land(&ability));
    }

    #[test]
    fn ability_animates_land_detects_generic_creature_type_grant() {
        let ability =
            AbilityDefinition::new(AbilityKind::Activated, generic_creature_type_effect());

        assert!(ability_animates_land(&ability));
    }

    #[test]
    fn colors_produced_by_land_handles_any_one_color() {
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(
            &mut state,
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
                        contribution: Default::default(),
                    },
                    restrictions: Vec::new(),
                    grants: Vec::new(),
                    expiry: None,
                    target: None,
                },
            ),
        );

        let colors = colors_produced_by_land(state.objects.get(&source_id).unwrap());
        assert_eq!(colors, vec![ManaColor::White, ManaColor::Blue]);
    }
}
