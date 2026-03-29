use engine::game::filter::matches_target_filter;
use engine::types::ability::{Effect, QuantityExpr, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, WardCost};
use engine::types::zones::Zone;

use crate::eval::{evaluate_creature, threat_level};

use super::context::PolicyContext;
use super::effect_classify::{
    aggregate_player_impact, aura_polarity, effect_polarity, extract_target_filter,
    is_spell_beneficial, targeted_player_impact, targets_creatures, targets_creatures_only,
    EffectPolarity,
};
use super::registry::TacticalPolicy;

pub struct AntiSelfHarmPolicy;

impl TacticalPolicy for AntiSelfHarmPolicy {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::CastSpell { .. } | GameAction::ActivateAbility { .. } => {
                score_pre_cast(ctx)
            }
            GameAction::ChooseTarget { target } => target
                .as_ref()
                .map_or(-0.25, |target| score_target_ref(ctx, target)),
            GameAction::SelectTargets { targets } => targets
                .iter()
                .map(|target| score_target_ref(ctx, target))
                .sum(),
            _ => 0.0,
        }
    }
}

/// Penalise casting a targeted spell when the only legal creature targets
/// would hurt the AI.  Two cases:
/// - Beneficial spell (pump/aura buff) but AI has no creatures → would buff opponents.
/// - Harmful spell (destroy) but opponents have no creatures → would kill own.
fn score_pre_cast(ctx: &PolicyContext<'_>) -> f64 {
    let effects = ctx.effects();

    let mut has_beneficial_creature_target = effects.iter().any(|effect| {
        matches!(effect_polarity(effect), EffectPolarity::Beneficial) && targets_creatures(effect)
    });
    // For harmful spells, only penalise when targeting is creature-exclusive.
    // Burn spells with TargetFilter::Any can still go face — don't block those.
    let mut has_harmful_creature_only_target = effects.iter().any(|effect| {
        !matches!(effect, Effect::Bounce { .. })
            && matches!(effect_polarity(effect), EffectPolarity::Harmful)
            && targets_creatures_only(effect)
    });
    let has_harmful_bounce = effects.iter().any(is_hostile_or_neutral_bounce);

    // Auras have no active effects — detect polarity via static definitions.
    if effects.is_empty() {
        if let Some(source) = ctx.source_object() {
            if source.card_types.subtypes.iter().any(|s| s == "Aura") {
                match aura_polarity(source) {
                    EffectPolarity::Beneficial => has_beneficial_creature_target = true,
                    EffectPolarity::Harmful => has_harmful_creature_only_target = true,
                    EffectPolarity::Contextual => {}
                }
            }
        }
    }

    if !has_beneficial_creature_target && !has_harmful_creature_only_target && !has_harmful_bounce {
        return 0.0;
    }

    let has_own_creature = ctx.state.battlefield.iter().any(|&id| {
        ctx.state.objects.get(&id).is_some_and(|o| {
            o.controller == ctx.ai_player && o.card_types.core_types.contains(&CoreType::Creature)
        })
    });
    let has_opponent_creature = ctx.state.battlefield.iter().any(|&id| {
        ctx.state.objects.get(&id).is_some_and(|o| {
            o.controller != ctx.ai_player && o.card_types.core_types.contains(&CoreType::Creature)
        })
    });

    let mut penalty = 0.0;

    // Beneficial creature-targeting spell but no own creatures to buff.
    if has_beneficial_creature_target && !has_own_creature {
        penalty -= 8.0;
    }

    // Harmful creature-only spell (e.g. Murder) but no opponent creatures to hit.
    if has_harmful_creature_only_target && !has_opponent_creature {
        penalty -= 8.0;
    }

    // Harmful bounce with no opposing legal targets will force a self-bounce line.
    if has_harmful_bounce && !has_opponent_bounce_target(ctx, &effects) {
        penalty -= 8.0;
    }

    penalty
}

fn has_opponent_bounce_target(ctx: &PolicyContext<'_>, effects: &[&Effect]) -> bool {
    let Some(source) = ctx.source_object() else {
        return false;
    };

    effects
        .iter()
        .filter(|effect| is_hostile_or_neutral_bounce(effect))
        .filter_map(|effect| match effect {
            Effect::Bounce { target, .. } => Some(target),
            _ => None,
        })
        .any(|target| {
            ctx.state.battlefield.iter().any(|&object_id| {
                ctx.state.objects.get(&object_id).is_some_and(|object| {
                    object.controller != ctx.ai_player
                        && matches_target_filter(ctx.state, object_id, target, source.id)
                })
            })
        })
}

fn is_hostile_or_neutral_bounce(effect: &&Effect) -> bool {
    let Effect::Bounce { .. } = effect else {
        return false;
    };
    !matches!(
        extract_target_filter(effect),
        Some(TargetFilter::Typed(typed))
            if matches!(typed.controller, Some(engine::types::ability::ControllerRef::You))
    )
}

fn score_target_ref(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    let beneficial = is_spell_beneficial(ctx);
    match target {
        TargetRef::Player(player_id) => {
            let is_self = *player_id == ctx.ai_player;
            let player_impact = targeted_player_impact(ctx, *player_id)
                .unwrap_or_else(|| aggregate_player_impact(ctx));
            let prefers_self = if player_impact > 0.25 {
                true
            } else if player_impact < -0.25 {
                false
            } else {
                beneficial
            };
            // Beneficial spells → target self; harmful → target opponent
            if prefers_self == is_self {
                4.0 + threat_level(ctx.state, ctx.ai_player, *player_id) * 8.0
            } else {
                -100.0
            }
        }
        TargetRef::Object(object_id) => score_target_object(ctx, *object_id, beneficial),
    }
}

fn score_target_object(ctx: &PolicyContext<'_>, object_id: ObjectId, beneficial: bool) -> f64 {
    let Some(object) = ctx.state.objects.get(&object_id) else {
        return -10.0;
    };

    let controller_delta = if object.controller == ctx.ai_player {
        if beneficial {
            1.0
        } else {
            -1.0
        }
    } else if beneficial {
        -1.0
    } else {
        1.0
    };
    let mut score = controller_delta * 2.0;

    if object.card_types.core_types.contains(&CoreType::Creature) {
        score += controller_delta * evaluate_creature(ctx.state, object_id);

        // Cache effects once — used by damage check, indestructible check, and bounce check
        let effects = ctx.effects();

        if !beneficial {
            if let Some(damage) = extract_damage_amount(&effects) {
                if let Some(toughness) = object.toughness {
                    let remaining = toughness - object.damage_marked as i32;
                    // Penalize targeting creatures that won't die to this damage.
                    // Wasting burn on a creature that survives is worse than going face.
                    if damage < remaining {
                        score -= 4.0;
                    }
                    // Penalize massive overkill (wasting damage capacity)
                    if remaining > 0 && damage >= remaining && damage > remaining * 2 {
                        let wasted = damage - remaining;
                        let waste_ratio = wasted as f64 / damage as f64;
                        score += ctx.penalties().overkill_base_penalty * waste_ratio.sqrt();
                    }
                }
            }

            // Penalize casting Destroy at indestructible creatures (does nothing)
            let is_destroy = effects.iter().any(|e| matches!(e, Effect::Destroy { .. }));
            if is_destroy && object.has_keyword(&Keyword::Indestructible) {
                score += ctx.penalties().indestructible_destroy_penalty;
            }

            // Penalize targeting creatures with ward (must pay additional cost)
            for keyword in &object.keywords {
                if let Keyword::Ward(ward_cost) = keyword {
                    let severity = match ward_cost {
                        WardCost::Mana(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                        WardCost::PayLife(amount) => (*amount as f64 / 3.0).min(2.0),
                        WardCost::DiscardCard => 1.5,
                        WardCost::SacrificeAPermanent => 2.0,
                        WardCost::Waterbend(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                        // CR 702.21a: Compound costs sum severity of components.
                        WardCost::Compound(costs) => costs
                            .iter()
                            .map(|c| match c {
                                WardCost::Mana(cost) => (cost.mana_value() as f64 / 2.0).min(2.0),
                                WardCost::PayLife(amount) => (*amount as f64 / 3.0).min(2.0),
                                WardCost::DiscardCard => 1.5,
                                WardCost::SacrificeAPermanent => 2.0,
                                WardCost::Waterbend(cost) => {
                                    (cost.mana_value() as f64 / 2.0).min(2.0)
                                }
                                WardCost::Compound(_) => 2.0,
                            })
                            .sum::<f64>()
                            .min(4.0),
                    };
                    score += ctx.penalties().ward_cost_penalty_base * severity;
                    break;
                }
            }

            // Removal quality mismatch: penalize premium removal on cheap targets
            if let Some(source) = ctx.source_object() {
                let spell_mv = source.mana_cost.mana_value();
                let target_value = evaluate_creature(ctx.state, object_id);
                if spell_mv >= 4 && target_value < 4.0 {
                    score += ctx.penalties().removal_quality_mismatch
                        * (1.0 - target_value / 4.0).max(0.0);
                }
            }
        }

        // Bounce-specific valuation: tokens are great targets, cheap permanents are bad
        let bounce_destination = effects.iter().find_map(|e| match e {
            Effect::Bounce { destination, .. } => Some(*destination),
            _ => None,
        });
        if let Some(destination) = bounce_destination {
            if !beneficial {
                let is_tuck = matches!(destination, Some(Zone::Library));
                if object.is_token || is_tuck {
                    // Tokens cease to exist when bounced; tuck is permanent removal
                    score += ctx.penalties().bounce_token_bonus;
                } else {
                    let mv = object.mana_cost.mana_value();
                    if mv <= 2 {
                        score += ctx.penalties().bounce_cheap_discount;
                    } else {
                        score += mv as f64 * ctx.penalties().bounce_expensive_bonus_per_mv;
                    }
                }
            }
        }
    }

    score
}

/// Extract the fixed damage amount from the pending spell's DealDamage effect.
/// Returns None for variable damage or non-damage spells.
fn extract_damage_amount(effects: &[&Effect]) -> Option<i32> {
    effects.iter().find_map(|effect| match effect {
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value },
            ..
        } => Some(*value),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        ContinuousModification, FilterProp, PtValue, ResolvedAbility, StaticDefinition,
        TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    fn make_target_selection_ctx(
        _state: &GameState,
        effect: Effect,
        legal_targets: Vec<TargetRef>,
        candidate_target: Option<TargetRef>,
    ) -> (AiDecisionContext, CandidateAction) {
        let ability = ResolvedAbility::new(effect, Vec::new(), ObjectId(100), PlayerId(0));
        let pending_cast = PendingCast::new(ObjectId(100), CardId(100), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets,
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: candidate_target,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        (decision, candidate)
    }

    #[test]
    fn beneficial_pump_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::Pump {
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            target: TargetFilter::Any,
        };

        // Score targeting own creature
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        // Score targeting opponent's creature
        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_own > score_opp,
            "Pump +3/+3 should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
        assert!(
            score_opp < 0.0,
            "Opponent creature score should be negative"
        );
    }

    #[test]
    fn negative_pump_prefers_opponent_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::Pump {
            power: PtValue::Fixed(-3),
            toughness: PtValue::Fixed(-3),
            target: TargetFilter::Any,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_opp > score_own,
            "Pump -3/-3 should prefer opponent creature: own={score_own}, opp={score_opp}"
        );
    }

    #[test]
    fn harmful_destroy_prefers_opponent_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let config = AiConfig::default();

        let effect = Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(own_id)),
        );
        let ctx_own = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_own = AntiSelfHarmPolicy.score(&ctx_own);

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(opp_id)),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_opp > score_own,
            "Destroy should prefer opponent creature: own={score_own}, opp={score_opp}"
        );
    }

    #[test]
    fn beneficial_player_target_prefers_self() {
        let state = make_state();
        let config = AiConfig::default();

        let effect = Effect::Pump {
            power: PtValue::Fixed(3),
            toughness: PtValue::Fixed(3),
            target: TargetFilter::Any,
        };

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect.clone(),
            vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            Some(TargetRef::Player(PlayerId(0))),
        );
        let ctx_self = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_self = AntiSelfHarmPolicy.score(&ctx_self);

        let (decision, candidate) = make_target_selection_ctx(
            &state,
            effect,
            vec![
                TargetRef::Player(PlayerId(0)),
                TargetRef::Player(PlayerId(1)),
            ],
            Some(TargetRef::Player(PlayerId(1))),
        );
        let ctx_opp = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let score_opp = AntiSelfHarmPolicy.score(&ctx_opp);

        assert!(
            score_self > score_opp,
            "Beneficial spell targeting player should prefer self: self={score_self}, opp={score_opp}"
        );
    }

    #[test]
    fn discard_then_draw_player_target_prefers_self() {
        let state = make_state();
        let config = AiConfig::default();
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let legal_targets = vec![
            TargetRef::Player(PlayerId(0)),
            TargetRef::Player(PlayerId(1)),
        ];
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(100),
                    CardId(100),
                    ability.clone(),
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: legal_targets.clone(),
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let self_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let self_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &self_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let opp_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let opp_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &opp_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let self_score = AntiSelfHarmPolicy.score(&self_ctx);
        let opp_score = AntiSelfHarmPolicy.score(&opp_ctx);
        assert!(
            self_score > opp_score,
            "Net card-positive discard/draw should prefer self: self={self_score}, opp={opp_score}"
        );
    }

    #[test]
    fn opponent_discards_and_you_draw_prefers_opponent() {
        let state = make_state();
        let config = AiConfig::default();
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DiscardCard {
                count: 1,
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        )
        .sub_ability(sub);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(PendingCast::new(
                    ObjectId(100),
                    CardId(100),
                    ability,
                    ManaCost::zero(),
                )),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Player(PlayerId(0)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let self_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(0))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let self_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &self_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        let opp_candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Player(PlayerId(1))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let opp_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &opp_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let self_score = AntiSelfHarmPolicy.score(&self_ctx);
        let opp_score = AntiSelfHarmPolicy.score(&opp_ctx);
        assert!(
            opp_score > self_score,
            "Targeted discard plus untargeted draw should still prefer opponent: self={self_score}, opp={opp_score}"
        );
    }

    #[test]
    fn plus_counter_is_beneficial() {
        let effect = Effect::AddCounter {
            counter_type: "+1/+1".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn minus_counter_is_harmful() {
        let effect = Effect::AddCounter {
            counter_type: "-1/-1".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Harmful);
    }

    #[test]
    fn unknown_effect_defaults_to_contextual() {
        let effect = Effect::GenericEffect {
            static_abilities: Vec::new(),
            target: None,
            duration: None,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Contextual);
    }

    /// Regression: AI should not cast a pump spell when it has no creatures,
    /// since the only targets would be opponent creatures.
    #[test]
    fn pre_cast_penalises_pump_with_no_friendly_creatures() {
        let mut state = make_state();
        // Only opponent has a creature — AI has none.
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);

        // Put Giant Growth in AI's hand so source_object() finds it.
        let spell_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Typed(engine::types::ability::TypedFilter::new(
                    TypeFilter::Creature,
                )),
            },
        )];

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(300),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting pump with no friendly creatures should be heavily penalised, got {score}"
        );
    }

    #[test]
    fn pre_cast_penalises_bounce_with_only_friendly_targets() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Otter", 1, 1);

        let spell_id = create_object(
            &mut state,
            CardId(301),
            PlayerId(0),
            "Boomerang Basics".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::Typed(
                    engine::types::ability::TypedFilter::new(TypeFilter::Permanent)
                        .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
                ),
                destination: None,
            },
        )];

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(301),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting bounce with only friendly targets should be heavily penalised, got {score}"
        );
    }

    #[test]
    fn pre_cast_allows_explicit_self_bounce_patterns() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Otter", 1, 1);

        let spell_id = create_object(
            &mut state,
            CardId(302),
            PlayerId(0),
            "Deputy of Acquittals".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::Typed(
                    engine::types::ability::TypedFilter::new(TypeFilter::Creature)
                        .controller(engine::types::ability::ControllerRef::You),
                ),
                destination: None,
            },
        )];

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(302),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= 0.0,
            "Explicit self-bounce patterns should not be treated as self-harm, got {score}"
        );
    }

    /// When the AI controls at least one creature, the pre-cast check should
    /// not penalise casting a pump spell.
    #[test]
    fn pre_cast_allows_pump_with_friendly_creatures() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);

        let spell_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Typed(engine::types::ability::TypedFilter::new(
                    TypeFilter::Creature,
                )),
            },
        )];

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(300),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= 0.0,
            "Casting pump with own creatures should not be penalised, got {score}"
        );
    }

    /// Casting a creature-only destruction spell when only the AI's own
    /// creatures exist should be penalised (symmetric to the pump check).
    #[test]
    fn pre_cast_penalises_destroy_with_no_opponent_creatures() {
        let mut state = make_state();
        // Only AI has a creature — opponent has none.
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let spell_id = create_object(
            &mut state,
            CardId(400),
            PlayerId(0),
            "Murder".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(engine::types::ability::TypedFilter::new(
                    TypeFilter::Creature,
                )),
                cant_regenerate: false,
            },
        )];

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(400),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting destroy with only own creatures should be penalised, got {score}"
        );
    }

    /// Burn spells with TargetFilter::Any can still target the opponent player,
    /// so they should NOT be penalised even when no opponent creatures exist.
    #[test]
    fn pre_cast_allows_burn_with_any_target_and_no_opponent_creatures() {
        let mut state = make_state();
        // Only AI has creatures — but burn can go face.
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);

        let spell_id = create_object(
            &mut state,
            CardId(500),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = vec![engine::types::ability::AbilityDefinition::new(
            engine::types::ability::AbilityKind::Spell,
            Effect::DealDamage {
                amount: engine::types::ability::QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )];

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(500),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score >= 0.0,
            "Burn with Any target should not be penalised (can go face), got {score}"
        );
    }

    fn add_aura(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.keywords
            .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                TypeFilter::Creature,
            ))));
        // Rancor-style: enchanted creature gets +2/+0 and has trample
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature)
                        .properties(vec![FilterProp::EnchantedBy]),
                ))
                .modifications(vec![
                    ContinuousModification::AddPower { value: 2 },
                    ContinuousModification::AddToughness { value: 0 },
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Trample,
                    },
                ]),
        );
        id
    }

    /// Regression: AI should enchant its own creatures with beneficial auras,
    /// not opponent creatures. Rancor (+2/+0 and trample) is beneficial.
    #[test]
    fn beneficial_aura_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_aura(&mut state, PlayerId(0), "Rancor");
        let config = AiConfig::default();

        let score_own = score_aura_target(&state, &config, aura_id, own_id, opp_id, own_id);
        let score_opp = score_aura_target(&state, &config, aura_id, own_id, opp_id, opp_id);

        assert!(
            score_own > score_opp,
            "Beneficial aura should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
        assert!(
            score_opp < 0.0,
            "Opponent creature score should be negative"
        );
    }

    fn score_aura_target(
        state: &GameState,
        config: &AiConfig,
        aura_id: ObjectId,
        own_id: ObjectId,
        opp_id: ObjectId,
        target_id: ObjectId,
    ) -> f64 {
        let (decision, candidate) = make_aura_target_selection_ctx(
            state,
            aura_id,
            vec![TargetRef::Object(own_id), TargetRef::Object(opp_id)],
            Some(TargetRef::Object(target_id)),
        );
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config,
            context: &crate::context::AiContext::empty(&config.weights),
        };
        AntiSelfHarmPolicy.score(&ctx)
    }

    /// Pre-cast check: AI should not cast a beneficial aura when it has no creatures.
    #[test]
    fn pre_cast_penalises_beneficial_aura_with_no_friendly_creatures() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_aura(&mut state, PlayerId(0), "Rancor");
        let card_id = state.objects[&aura_id].card_id;
        let config = AiConfig::default();

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: aura_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting beneficial aura with no friendly creatures should be penalised, got {score}"
        );
    }

    fn add_harmful_aura(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.keywords
            .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                TypeFilter::Creature,
            ))));
        // Pacifism-style: enchanted creature can't attack or block
        obj.static_definitions
            .push(StaticDefinition::new(StaticMode::CantAttack).affected(TargetFilter::SelfRef));
        id
    }

    fn add_unblockable_aura(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.card_types.subtypes.push("Aura".to_string());
        obj.keywords
            .push(Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(
                TypeFilter::Creature,
            ))));
        // Aqueous Form-style: enchanted creature can't be blocked
        obj.static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantBeBlocked).affected(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Creature)
                        .properties(vec![FilterProp::EnchantedBy]),
                )),
            );
        id
    }

    /// Harmful auras (Pacifism) should target opponent creatures, not own.
    #[test]
    fn harmful_aura_prefers_opponent_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_harmful_aura(&mut state, PlayerId(0), "Pacifism");
        let config = AiConfig::default();

        let score_own = score_aura_target(&state, &config, aura_id, own_id, opp_id, own_id);
        let score_opp = score_aura_target(&state, &config, aura_id, own_id, opp_id, opp_id);

        assert!(
            score_opp > score_own,
            "Harmful aura should prefer opponent creature: own={score_own}, opp={score_opp}"
        );
    }

    /// Beneficial non-modification auras (Aqueous Form: "can't be blocked")
    /// should target own creatures.
    #[test]
    fn beneficial_cant_be_blocked_aura_prefers_own_creature() {
        let mut state = make_state();
        let own_id = add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let opp_id = add_creature(&mut state, PlayerId(1), "Goblin", 2, 2);
        let aura_id = add_unblockable_aura(&mut state, PlayerId(0), "Aqueous Form");
        let config = AiConfig::default();

        let score_own = score_aura_target(&state, &config, aura_id, own_id, opp_id, own_id);
        let score_opp = score_aura_target(&state, &config, aura_id, own_id, opp_id, opp_id);

        assert!(
            score_own > score_opp,
            "CantBeBlocked aura should prefer own creature: own={score_own}, opp={score_opp}"
        );
        assert!(score_own > 0.0, "Own creature score should be positive");
    }

    /// Pre-cast: harmful aura (Pacifism) with only own creatures should be penalised.
    #[test]
    fn pre_cast_penalises_harmful_aura_with_no_opponent_creatures() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "Bear", 2, 2);
        let aura_id = add_harmful_aura(&mut state, PlayerId(0), "Pacifism");
        let card_id = state.objects[&aura_id].card_id;
        let config = AiConfig::default();

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: aura_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
        };

        let score = AntiSelfHarmPolicy.score(&ctx);
        assert!(
            score < -5.0,
            "Casting harmful aura with only own creatures should be penalised, got {score}"
        );
    }

    /// Helper to create a target selection context for an aura (no active effects).
    fn make_aura_target_selection_ctx(
        state: &GameState,
        aura_id: ObjectId,
        legal_targets: Vec<TargetRef>,
        candidate_target: Option<TargetRef>,
    ) -> (AiDecisionContext, CandidateAction) {
        // Auras have no active abilities — use a GenericEffect placeholder since
        // the policy should fall through to static_definitions for polarity.
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: Vec::new(),
                target: None,
                duration: None,
            },
            Vec::new(),
            aura_id,
            PlayerId(0),
        );
        let card_id = state.objects[&aura_id].card_id;
        let pending_cast = PendingCast::new(aura_id, card_id, ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets,
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: candidate_target,
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        (decision, candidate)
    }
}
