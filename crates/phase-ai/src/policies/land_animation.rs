//! Land Animation Timing Policy
//!
//! Evaluates when to animate man-lands like Lumbering Falls. Prevents the AI from
//! animating lands every turn regardless of strategic value, considering mana needs,
//! color requirements, and combat value.

use std::collections::HashSet;

use engine::game::casting::can_pay_ability_mana_cost_after_auto_tap_excluding;
use engine::game::game_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, CostCategory, Effect, ManaProduction,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Penalty for animating a land when mana is needed for other spells.
const MANA_NEEDED_PENALTY: f64 = -2.0;

/// Penalty for animating a land that ends up tapped (no combat value).
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

        // CR 508.1a / CR 509.1a: an animated land that ends up tapped can
        // neither attack nor block this turn, so animating it has no combat
        // value. This is the Shambling Vent failure mode — the AI taps the
        // manland for mana to help pay its own {1}{W}{B} animation cost and
        // turns it into a useless tapped creature. Strongly disprefer any
        // activation that leaves the source tapped.
        if animation_leaves_source_tapped(ctx, *source_id, obj, ability_def) {
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
                // CR 202.2c: Omnath, Locus of All — colors come from a target
                // object resolved at trigger time, not statically predictable
                // for land-animation color preview. Contribute nothing.
                ManaProduction::AnyCombinationOfObjectColors { .. } => {}
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

/// CR 508.1a / CR 509.1a: returns true when activating `ability` would leave
/// the source land tapped — making the animated creature unable to attack or
/// block this turn. Three ways this happens:
///   1. the land is already tapped (e.g. used for mana earlier this turn),
///   2. the activation cost itself taps the source, or
///   3. paying the mana cost would force tapping the source for mana. The
///      engine's auto-tap (CR 605.3b) deprioritizes the source but still taps
///      it as a last resort, so the source is forced exactly when the cost
///      can't be paid *without* it. We answer that by asking the engine's own
///      payment solver to pay the cost with the source excluded — keeping color
///      and multi-mana yield exact instead of re-deriving them here.
fn animation_leaves_source_tapped(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    source: &game_object::GameObject,
    ability: &AbilityDefinition,
) -> bool {
    if source.tapped || ability_taps_source(ability) {
        return true;
    }

    // Only plain mana costs are assessable here; non-mana or dynamically-priced
    // animation costs (none exist among current man-lands) fall through to the
    // additive scoring below rather than being penalized blind.
    let Some(AbilityCost::Mana { cost }) = ability.cost.as_ref() else {
        return false;
    };
    if cost.mana_value() == 0 {
        return false;
    }

    !can_pay_cost_excluding_source(ctx, source_id, cost)
}

/// True iff the AI can pay `cost` for `source_id`'s ability without tapping the
/// source itself. Delegates to the engine's payment solver (color- and
/// yield-accurate) with the source added to the excluded set.
fn can_pay_cost_excluding_source(
    ctx: &PolicyContext<'_>,
    source_id: ObjectId,
    cost: &ManaCost,
) -> bool {
    let excluded = HashSet::from([source_id]);
    can_pay_ability_mana_cost_after_auto_tap_excluding(
        ctx.state,
        ctx.ai_player,
        source_id,
        cost,
        &excluded,
    )
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
    use engine::types::ability::{
        AbilityKind, PtValue, QuantityExpr, StaticDefinition, TargetFilter,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::CardId;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
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
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
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

    fn reason_kind(verdict: &PolicyVerdict) -> &str {
        let PolicyVerdict::Score { reason, .. } = verdict else {
            panic!("expected score verdict");
        };
        reason.kind
    }

    /// {1}{W}{B} animation ability (mana value 3), mirroring Shambling Vent.
    fn animate_ability_with_mana_cost() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, animate_effect()).cost(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::Black],
                generic: 1,
            },
        })
    }

    /// {1}{W}{B} animation via the `GenericEffect` + `AddType Creature` shape
    /// that real man-lands (Shambling Vent) actually use, not the synthetic
    /// `Effect::Animate`. Locks the production code path through the guard.
    fn generic_animate_ability_with_mana_cost() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, generic_creature_type_effect()).cost(
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::White, ManaCostShard::Black],
                    generic: 1,
                },
            },
        )
    }

    /// An untapped basic-style land: `{T}: Add {color}`.
    fn mana_land(state: &mut GameState, color: ManaColor) -> ObjectId {
        land_with_ability(
            state,
            AbilityDefinition::new(AbilityKind::Activated, mana_effect(vec![color]))
                .cost(AbilityCost::Tap),
        )
    }

    #[test]
    fn animation_forcing_self_tap_for_mana_is_penalized() {
        // Manland is untapped but the AI has no other mana: paying {1}{W}{B}
        // would tap the manland itself, animating it into a tapped creature.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
        let PolicyVerdict::Score { delta, .. } = verdict else {
            panic!("expected score verdict");
        };
        assert_eq!(delta, TAPPED_LAND_PENALTY);
    }

    #[test]
    fn animation_with_sufficient_other_mana_is_not_tapped_penalized() {
        // W + B + a third source cover {1}{W}{B} without tapping the manland,
        // so it can animate and still attack/block this turn.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::Black);
        mana_land(&mut state, ManaColor::White);

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_score");
    }

    #[test]
    fn animation_color_starved_off_source_is_penalized() {
        // Three other untapped sources cover the *total* of {1}{W}{B}, but none
        // produces black — only the manland could, so the engine would tap it
        // for {B} and leave a tapped creature. A count-only check would miss
        // this; the color-aware engine solver catches it.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::Green);

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
    }

    #[test]
    fn generic_effect_manland_self_tap_is_penalized() {
        // Real Shambling Vent shape (GenericEffect AddType Creature). With no
        // other mana, paying {1}{W}{B} forces tapping the manland → penalized.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, generic_animate_ability_with_mana_cost());

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
    }

    #[test]
    fn already_tapped_manland_animation_is_penalized() {
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::Black);
        mana_land(&mut state, ManaColor::White);
        state.objects.get_mut(&source_id).unwrap().tapped = true;

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
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
