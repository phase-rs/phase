use engine::types::ability::{
    ContinuousModification, Effect, PtValue, QuantityExpr, TargetFilter, TargetRef, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::statics::StaticMode;

use crate::eval::{evaluate_creature, threat_level};

use super::context::PolicyContext;
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
        matches!(effect_polarity(effect), EffectPolarity::Harmful) && targets_creatures_only(effect)
    });

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

    if !has_beneficial_creature_target && !has_harmful_creature_only_target {
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

    penalty
}

/// Returns true if the effect exclusively targets creatures (not "any target").
/// Used for harmful spells: burn with TargetFilter::Any can still go face.
fn targets_creatures_only(effect: &Effect) -> bool {
    let filter = extract_target_filter(effect);
    matches!(
        filter,
        Some(TargetFilter::Typed(typed))
            if typed.type_filters.iter().any(|t| matches!(t, TypeFilter::Creature))
    )
}

/// Returns true if an effect's target filter is creature-typed (or Any).
fn targets_creatures(effect: &Effect) -> bool {
    let Some(filter) = extract_target_filter(effect) else {
        return false;
    };
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(typed) => typed
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Creature)),
        _ => false,
    }
}

/// Extract the target filter from an effect, if present.
fn extract_target_filter(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        // Beneficial effects
        Effect::Pump { target, .. }
        | Effect::AddCounter { target, .. }
        | Effect::Animate { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::Regenerate { target, .. }
        | Effect::Untap { target }
        | Effect::PreventDamage { target, .. }
        // Harmful effects
        | Effect::Destroy { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Tap { target }
        | Effect::RemoveCounter { target, .. } => Some(target),
        _ => None,
    }
}

/// Three-valued polarity: whether an effect benefits or harms its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectPolarity {
    /// Target benefits (pump, regenerate, +1/+1 counters, untap, animate)
    Beneficial,
    /// Target is harmed (destroy, damage, -1/-1 counters, sacrifice)
    Harmful,
    /// Depends on context — fall through to default "assume harmful" behavior
    Contextual,
}

fn effect_polarity(effect: &Effect) -> EffectPolarity {
    match effect {
        // Pump: beneficial only if both values are non-negative
        Effect::Pump {
            power, toughness, ..
        } => {
            let p_ok = matches!(power, PtValue::Fixed(v) if *v >= 0)
                || matches!(power, PtValue::Variable(_) | PtValue::Quantity(_));
            let t_ok = matches!(toughness, PtValue::Fixed(v) if *v >= 0)
                || matches!(toughness, PtValue::Variable(_) | PtValue::Quantity(_));
            if p_ok && t_ok {
                EffectPolarity::Beneficial
            } else {
                EffectPolarity::Harmful
            }
        }
        // Counters: +1/+1 is beneficial, -1/-1 is harmful
        Effect::AddCounter { counter_type, .. } => {
            if counter_type.starts_with('+') {
                EffectPolarity::Beneficial
            } else if counter_type.starts_with('-') {
                EffectPolarity::Harmful
            } else {
                EffectPolarity::Contextual
            }
        }
        Effect::Regenerate { .. }
        | Effect::PreventDamage { .. }
        | Effect::Animate { .. }
        | Effect::DoublePT { .. } => EffectPolarity::Beneficial,
        Effect::Untap { .. } => EffectPolarity::Beneficial,
        Effect::Destroy { .. }
        | Effect::DealDamage { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::LoseLife { .. }
        | Effect::RemoveCounter { .. }
        | Effect::Tap { .. } => EffectPolarity::Harmful,
        _ => EffectPolarity::Contextual,
    }
}

/// Returns true if the pending spell's dominant effect is beneficial to its target.
/// Defaults to false (assume harmful) when uncertain — safe fallback since most
/// targeted spells in MTG are removal/damage.
fn is_spell_beneficial(ctx: &PolicyContext<'_>) -> bool {
    let effects = ctx.effects();

    // Check active effects for a clear polarity signal.
    let dominant_polarity = effects.first().map(|e| effect_polarity(e));
    match dominant_polarity {
        Some(EffectPolarity::Beneficial) => return true,
        Some(EffectPolarity::Harmful) => return false,
        _ => {}
    }

    // No clear polarity from active effects (empty or Contextual).
    // Auras carry their beneficial/harmful nature in static definitions.
    if let Some(source) = ctx.source_object() {
        if source.card_types.subtypes.iter().any(|s| s == "Aura") {
            return matches!(aura_polarity(source), EffectPolarity::Beneficial);
        }
    }

    false
}

/// Determines whether an Aura is beneficial or harmful to its target by inspecting
/// both static modes (CantAttack, CantBeBlocked, etc.) and continuous modifications.
fn aura_polarity(source: &engine::game::game_object::GameObject) -> EffectPolarity {
    // First check static modes — these carry clear polarity independent of modifications.
    for sd in &source.static_definitions {
        match static_mode_polarity(&sd.mode) {
            EffectPolarity::Contextual => continue,
            polarity => return polarity,
        }
    }

    // Then check continuous modifications (AddPower, AddKeyword, etc.).
    for sd in &source.static_definitions {
        for m in &sd.modifications {
            match modification_polarity(m) {
                EffectPolarity::Contextual => continue,
                polarity => return polarity,
            }
        }
    }

    EffectPolarity::Contextual
}

/// Classify a static mode as beneficial/harmful to the enchanted permanent.
fn static_mode_polarity(mode: &StaticMode) -> EffectPolarity {
    match mode {
        // Harmful: restricts the enchanted permanent
        StaticMode::CantAttack
        | StaticMode::CantBlock
        | StaticMode::CantUntap
        | StaticMode::MustAttack
        | StaticMode::MustBlock
        | StaticMode::CantGainLife
        | StaticMode::CantBeActivated => EffectPolarity::Harmful,
        // Beneficial: enhances the enchanted permanent
        StaticMode::CantBeBlocked
        | StaticMode::CantBeBlockedExceptBy { .. }
        | StaticMode::CantBeTargeted
        | StaticMode::CantBeCountered
        | StaticMode::Protection
        | StaticMode::CastWithFlash => EffectPolarity::Beneficial,
        // Continuous, cost changes, and others depend on modifications/context
        _ => EffectPolarity::Contextual,
    }
}

/// Classify a continuous modification as beneficial/harmful to its target.
fn modification_polarity(m: &ContinuousModification) -> EffectPolarity {
    match m {
        ContinuousModification::AddPower { value }
        | ContinuousModification::AddToughness { value } => {
            if *value > 0 {
                EffectPolarity::Beneficial
            } else if *value < 0 {
                EffectPolarity::Harmful
            } else {
                EffectPolarity::Contextual
            }
        }
        ContinuousModification::AddDynamicPower { .. }
        | ContinuousModification::AddDynamicToughness { .. } => EffectPolarity::Beneficial,
        ContinuousModification::AddKeyword { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddType { .. }
        | ContinuousModification::AddSubtype { .. } => EffectPolarity::Beneficial,
        ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::RemoveSubtype { .. } => EffectPolarity::Harmful,
        // SetPower/SetToughness, SetColor, etc. are contextual — could go either way.
        _ => EffectPolarity::Contextual,
    }
}

fn score_target_ref(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    let beneficial = is_spell_beneficial(ctx);
    match target {
        TargetRef::Player(player_id) => {
            let is_self = *player_id == ctx.ai_player;
            // Beneficial spells → target self; harmful → target opponent
            if beneficial == is_self {
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

        // Penalize targeting creatures that won't die to this damage.
        // Wasting burn on a creature that survives is worse than going face.
        if !beneficial {
            if let Some(damage) = get_spell_damage_amount(ctx) {
                if let Some(toughness) = object.toughness {
                    let remaining = toughness - object.damage_marked as i32;
                    if damage < remaining {
                        score -= 4.0;
                    }
                }
            }
        }
    }

    score
}

/// Extract the fixed damage amount from the pending spell's DealDamage effect.
/// Returns None for variable damage or non-damage spells.
fn get_spell_damage_amount(ctx: &PolicyContext<'_>) -> Option<i32> {
    ctx.effects().into_iter().find_map(|effect| match effect {
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
        FilterProp, ResolvedAbility, StaticDefinition, TargetFilter, TypedFilter,
    };
    use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
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
    fn plus_counter_is_beneficial() {
        let effect = Effect::AddCounter {
            counter_type: "+1/+1".to_string(),
            count: 1,
            target: TargetFilter::Any,
        };
        assert_eq!(effect_polarity(&effect), EffectPolarity::Beneficial);
    }

    #[test]
    fn minus_counter_is_harmful() {
        let effect = Effect::AddCounter {
            counter_type: "-1/-1".to_string(),
            count: 1,
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
