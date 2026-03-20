use engine::types::ability::{Effect, PtValue, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;

use crate::eval::{evaluate_creature, threat_level};

use super::context::PolicyContext;
use super::registry::TacticalPolicy;

pub struct AntiSelfHarmPolicy;

impl TacticalPolicy for AntiSelfHarmPolicy {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
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
        Effect::Regenerate { .. } | Effect::PreventDamage { .. } | Effect::Animate { .. } => {
            EffectPolarity::Beneficial
        }
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
    if effects.is_empty() {
        return false;
    }
    // Use the first effect's polarity as dominant (primary effect drives targeting).
    // If Contextual, fall through to false (assume harmful).
    matches!(effect_polarity(effects[0]), EffectPolarity::Beneficial)
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
    }

    score
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{ResolvedAbility, TargetFilter};
    use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
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
}
