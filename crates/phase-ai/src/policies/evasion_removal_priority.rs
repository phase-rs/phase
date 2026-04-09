use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;

use super::context::PolicyContext;
use super::effect_classify::is_spell_beneficial;
use super::registry::TacticalPolicy;
use super::strategy_helpers::ai_can_block;

pub struct EvasionRemovalPriorityPolicy;

impl TacticalPolicy for EvasionRemovalPriorityPolicy {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target_id)),
        } = &ctx.candidate.action
        else {
            return 0.0;
        };

        // Only for harmful effects (removal)
        if is_spell_beneficial(ctx) {
            return 0.0;
        }

        let Some(target) = ctx.state.objects.get(target_id) else {
            return 0.0;
        };

        // Only relevant for creatures
        if !target.card_types.core_types.contains(&CoreType::Creature) {
            return 0.0;
        }

        let power = target.power.unwrap_or(0) as f64;
        let mult = ctx.penalties().evasion_removal_bonus_mult;

        let has_flying = target.has_keyword(&Keyword::Flying);
        let has_shadow = target.has_keyword(&Keyword::Shadow);
        let has_menace = target.has_keyword(&Keyword::Menace);

        if !has_flying && !has_shadow && !has_menace {
            return 0.0;
        }

        // Check if AI can block this evasive creature
        let can_block = ai_can_block(ctx.state, ctx.ai_player, *target_id);

        if !can_block {
            // AI has no answer for this creature in combat — high removal priority
            (power * mult).min(3.0)
        } else if has_menace {
            // Menace: AI might have a blocker but needs 2 legal blockers — count
            // only creatures that can actually block this attacker (flying/shadow check)
            let legal_blocker_count = ctx
                .state
                .battlefield
                .iter()
                .filter(|&&id| {
                    ctx.state.objects.get(&id).is_some_and(|obj| {
                        obj.controller == ctx.ai_player
                            && !obj.tapped
                            && obj.card_types.core_types.contains(&CoreType::Creature)
                            && crate::combat_ai::can_block_check(obj, target)
                    })
                })
                .count();
            if legal_blocker_count < 2 {
                (power * mult * 0.5).min(3.0)
            } else {
                0.0
            }
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, ResolvedAbility, TargetFilter, TargetRef};
    use engine::types::game_state::{GameState, PendingCast, TargetSelectionSlot, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    #[test]
    fn bonus_for_unblockable_flyer() {
        let mut state = GameState::new_two_player(42);

        // Opponent's flyer
        let flyer = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Dragon".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&flyer).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(4);
        obj.toughness = Some(4);
        obj.keywords.push(Keyword::Flying);

        // AI has a ground creature (can't block flyer)
        let ground = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ground).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);

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
                    legal_targets: vec![TargetRef::Object(flyer)],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(flyer)),
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

        let score = EvasionRemovalPriorityPolicy.score(&ctx);
        assert!(
            score > 1.0,
            "Should give significant bonus for unblockable flyer, got {score}"
        );
    }

    #[test]
    fn no_bonus_for_ground_creature() {
        let mut state = GameState::new_two_player(42);

        // Opponent's ground creature
        let ground_opp = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Elephant".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&ground_opp).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(4);
        obj.toughness = Some(4);

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
                    legal_targets: vec![TargetRef::Object(ground_opp)],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(TargetRef::Object(ground_opp)),
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

        let score = EvasionRemovalPriorityPolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No bonus for ground creature, got {score}"
        );
    }
}
