use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::keywords::GiftKind;
use engine::types::player::PlayerId;

use crate::eval::evaluate_creature;
use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::effect_classify::{effect_polarity, EffectPolarity};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

pub struct DownsideAwarenessPolicy;

impl DownsideAwarenessPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        // Only at cast time — gift cost is paid regardless of target
        if !matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            return 0.0;
        }

        let effects = ctx.effects();
        let mut gift_penalty = 0.0;

        for effect in &effects {
            if let Effect::GiftDelivery { kind } = effect {
                gift_penalty += match kind {
                    GiftKind::Card => ctx.penalties().gift_card_penalty,
                    GiftKind::Treasure => ctx.penalties().gift_treasure_penalty,
                    GiftKind::Food => ctx.penalties().gift_food_penalty,
                    GiftKind::TappedFish => ctx.penalties().gift_fish_penalty,
                };
            }
        }

        if gift_penalty >= 0.0 {
            return 0.0;
        }

        // If primary effect is removal but there's nothing worth removing,
        // the gift cost is pure downside — double the penalty
        let primary_is_removal = effects
            .first()
            .is_some_and(|e| matches!(effect_polarity(e), EffectPolarity::Harmful));
        if primary_is_removal && !has_worthy_removal_target(ctx) {
            gift_penalty *= 2.0;
        }

        gift_penalty
    }
}

impl TacticalPolicy for DownsideAwarenessPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::DownsideAwareness
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
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
        // Range check (issue #5473): the largest gift penalty is `gift_card`
        // (-3.0), doubled to -6.0 for pure-downside removal — so `score()` never
        // leaves [-6.0, 0.0], comfortably inside the critical band. No rescale is
        // needed; PolicyVerdict::score is identity here and simply upholds the
        // band contract uniformly (no raw Score literal).
        PolicyVerdict::score(
            self.score(ctx),
            PolicyReason::new("downside_awareness_score"),
        )
    }
}

/// Check if any opponent creature on the battlefield is valuable enough to justify
/// casting removal with a gift cost.
fn has_worthy_removal_target(ctx: &PolicyContext<'_>) -> bool {
    let threshold = ctx.penalties().worthy_target_threshold;
    ctx.state.battlefield.iter().any(|&id| {
        ctx.state.objects.get(&id).is_some_and(|o| {
            o.controller != ctx.ai_player
                && o.card_types.core_types.contains(&CoreType::Creature)
                && evaluate_creature(ctx.state, id) > threshold
        })
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, BounceSelection, TargetFilter};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
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
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    fn make_cast_ctx(
        state: &mut GameState,
        effect: Effect,
        sub_effect: Option<Effect>,
    ) -> (AiDecisionContext, CandidateAction) {
        // Create a spell object in the state with the right abilities
        let spell_id = create_object(
            state,
            CardId(100),
            PlayerId(1),
            "Gift Spell".to_string(),
            Zone::Hand,
        );
        let mut abilities = vec![AbilityDefinition::new(AbilityKind::Spell, effect.clone())];
        if let Some(ref sub) = sub_effect {
            abilities.push(AbilityDefinition::new(AbilityKind::Spell, sub.clone()));
        }
        state.objects.get_mut(&spell_id).unwrap().abilities = Arc::new(abilities);

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(1),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(100),
                targets: Vec::new(),

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(1)), TacticalClass::Spell),
        };
        (decision, candidate)
    }

    fn score_policy(
        state: &GameState,
        decision: &AiDecisionContext,
        candidate: &CandidateAction,
    ) -> f64 {
        let config = AiConfig::default();
        let ctx = PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(1),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        DownsideAwarenessPolicy.score(&ctx)
    }

    #[test]
    fn gift_card_penalizes_cast() {
        let mut state = make_state();
        let (decision, candidate) = make_cast_ctx(
            &mut state,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            Some(Effect::GiftDelivery {
                kind: GiftKind::Card,
            }),
        );
        let score = score_policy(&state, &decision, &candidate);
        assert!(score < -2.0, "Gift card should penalize, got {score}");
    }

    #[test]
    fn gift_fish_penalizes_less_than_card() {
        let mut state_card = make_state();
        let (dec_card, can_card) = make_cast_ctx(
            &mut state_card,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            Some(Effect::GiftDelivery {
                kind: GiftKind::Card,
            }),
        );
        let score_card = score_policy(&state_card, &dec_card, &can_card);

        let mut state_fish = make_state();
        let (dec_fish, can_fish) = make_cast_ctx(
            &mut state_fish,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            Some(Effect::GiftDelivery {
                kind: GiftKind::TappedFish,
            }),
        );
        let score_fish = score_policy(&state_fish, &dec_fish, &can_fish);
        assert!(
            score_fish > score_card,
            "Fish penalty should be less than card: fish={score_fish}, card={score_card}"
        );
    }

    #[test]
    fn no_penalty_without_gift() {
        let mut state = make_state();
        let (decision, candidate) = make_cast_ctx(
            &mut state,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            None,
        );
        let score = score_policy(&state, &decision, &candidate);
        assert!(score.abs() < 0.01, "No penalty without gift, got {score}");
    }

    #[test]
    fn gift_penalty_doubles_no_targets() {
        let mut state = make_state();
        // No opponent creatures — gift removal has no worthy target
        let (decision, candidate) = make_cast_ctx(
            &mut state,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            Some(Effect::GiftDelivery {
                kind: GiftKind::TappedFish,
            }),
        );
        let base_score = score_policy(&state, &decision, &candidate);

        // Now add a strong opponent creature
        let mut state_with_creature = make_state();
        add_creature(&mut state_with_creature, PlayerId(0), 5, 5);
        let (decision2, candidate2) = make_cast_ctx(
            &mut state_with_creature,
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            Some(Effect::GiftDelivery {
                kind: GiftKind::TappedFish,
            }),
        );
        let target_score = score_policy(&state_with_creature, &decision2, &candidate2);

        assert!(
            base_score < target_score,
            "No-target penalty should be worse: no_target={base_score}, with_target={target_score}"
        );
    }

    #[test]
    fn gift_not_applied_during_targeting() {
        let state = make_state();
        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(1),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(ObjectId(1))),
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(1)), TacticalClass::Target),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(1),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let score = DownsideAwarenessPolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No penalty during targeting, got {score}"
        );
    }
}
