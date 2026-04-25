use engine::game::game_object::GameObject;
use engine::types::ability::{Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::effect_classify::{effect_polarity, EffectPolarity};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct RecursionAwarenessPolicy;

impl RecursionAwarenessPolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target_id)),
        } = &ctx.candidate.action
        else {
            return 0.0;
        };

        // Only for harmful effects
        let effects = ctx.effects();
        if effects.is_empty()
            || !effects
                .iter()
                .any(|e| matches!(effect_polarity(e), EffectPolarity::Harmful))
        {
            return 0.0;
        }

        let Some(target) = ctx.state.objects.get(target_id) else {
            return 0.0;
        };

        // Only relevant for creatures
        if !target.card_types.core_types.contains(&CoreType::Creature) {
            return 0.0;
        }

        let has_recursion = crate::zone_eval::has_recursion_keyword(target);
        let has_death = has_death_trigger(target);

        if !has_recursion && !has_death {
            return 0.0;
        }

        // Check what effect type we're using
        let sends_to_graveyard = effects.iter().any(|e| {
            matches!(
                e,
                Effect::Destroy { .. } | Effect::DealDamage { .. } | Effect::Sacrifice { .. }
            )
        });
        let sends_to_exile = effects.iter().any(|e| {
            matches!(
                e,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    ..
                }
            )
        });

        let mut score = 0.0;

        if has_recursion {
            if sends_to_graveyard {
                // Creature will come back from graveyard — destroy/damage is less effective
                score += ctx.penalties().recursion_destroy_penalty;
            }
            if sends_to_exile {
                // Exile permanently removes recursive threats
                score += ctx.penalties().recursion_exile_bonus;
            }
        } else if has_death && sends_to_graveyard {
            // Death trigger generates value but creature stays dead — mild penalty
            score += ctx.penalties().death_trigger_destroy_penalty;
        }

        score
    }
}

impl TacticalPolicy for RecursionAwarenessPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::RecursionAwareness
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::SelectTarget]
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
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("recursion_awareness_score"),
        }
    }
}

/// Check if a creature has triggers that fire when it leaves the battlefield
/// (dies triggers, leaves-play triggers).
fn has_death_trigger(obj: &GameObject) -> bool {
    obj.trigger_definitions.iter_unchecked().any(|trigger| {
        matches!(trigger.mode, TriggerMode::ChangesZone)
            && trigger.origin == Some(Zone::Battlefield)
            && matches!(
                trigger.destination,
                Some(Zone::Graveyard) | None // None = any destination (includes dies)
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{ResolvedAbility, TargetFilter};
    use engine::types::game_state::{PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;

    #[test]
    fn penalizes_destroy_on_recursive_creature() {
        let mut state = engine::types::game_state::GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Recursive".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.keywords.push(Keyword::Escape {
            cost: engine::types::mana::ManaCost::zero(),
            exile_count: 3,
        });

        let config = AiConfig::default();
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(100), CardId(100), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(creature)],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = RecursionAwarenessPolicy.score(&ctx);
        assert!(
            score < -1.0,
            "Should penalize destroy on recursive creature, got {score}"
        );
    }

    #[test]
    fn bonus_for_exile_on_recursive_creature() {
        let mut state = engine::types::game_state::GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Recursive".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.keywords.push(Keyword::Escape {
            cost: engine::types::mana::ManaCost::zero(),
            exile_count: 3,
        });

        let config = AiConfig::default();
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            Vec::new(),
            ObjectId(100),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(100), CardId(100), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![TargetRef::Object(creature)],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(creature)),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let score = RecursionAwarenessPolicy.score(&ctx);
        assert!(
            score > 0.5,
            "Should bonus exile on recursive creature, got {score}"
        );
    }
}
