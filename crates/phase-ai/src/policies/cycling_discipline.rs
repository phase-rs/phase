//! Patience for Cycling and Typecycling activations.
//!
//! Cycling is card-neutral, but the generic activated-ability prior treats it
//! as immediately better than passing. That made the AI cycle whenever the
//! option existed, including cycling away the only land available for its next
//! planned land drop. This policy removes that automatic edge while keeping all
//! cycling scores finite so a real tactical payoff can still justify the line.

use engine::types::ability::AbilityTag;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;
use crate::plan::PlanSnapshot;

pub struct CyclingDisciplinePolicy;

impl TacticalPolicy for CyclingDisciplinePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CyclingDiscipline
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
        Some(1.0) // activation-constant: the Cycling tag self-gates this universal policy
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let Some(ability) = ctx.effective_activated_ability() else {
            return PolicyVerdict::neutral(PolicyReason::new("cycling_discipline_na"));
        };

        if ability.ability_tag != Some(AbilityTag::Cycling) {
            return PolicyVerdict::neutral(PolicyReason::new("cycling_discipline_na"));
        }

        if source_is_sole_needed_land(ctx) {
            PolicyVerdict::strong(
                ctx.penalties().cycling_needed_land_penalty,
                PolicyReason::new("cycling_discipline_needed_land"),
            )
        } else {
            PolicyVerdict::preference(
                ctx.penalties().cycling_patience_penalty,
                PolicyReason::new("cycling_discipline_patience"),
            )
        }
    }
}

fn source_is_sole_needed_land(ctx: &PolicyContext<'_>) -> bool {
    let GameAction::ActivateAbility { source_id, .. } = &ctx.candidate.action else {
        return false;
    };
    let player = &ctx.state.players[ctx.ai_player.0 as usize];
    let Some(source) = ctx.state.objects.get(source_id) else {
        return false;
    };

    if source.zone != Zone::Hand
        || !player.hand.contains(source_id)
        || !source.card_types.core_types.contains(&CoreType::Land)
    {
        return false;
    }

    let lands_in_hand = player
        .hand
        .iter()
        .filter_map(|id| ctx.state.objects.get(id))
        .filter(|object| object.card_types.core_types.contains(&CoreType::Land))
        .take(2)
        .count();
    if lands_in_hand != 1 {
        return false;
    }

    let controlled_lands = ctx
        .state
        .battlefield
        .iter()
        .filter_map(|id| ctx.state.objects.get(id))
        .filter(|object| {
            object.controller == ctx.ai_player
                && object.card_types.core_types.contains(&CoreType::Land)
        })
        .count();

    ctx.context
        .session
        .plan
        .get(&ctx.ai_player)
        .and_then(|plan| next_planned_land_target(plan, controlled_lands))
        .is_some()
}

fn next_planned_land_target(plan: &PlanSnapshot, controlled_lands: usize) -> Option<usize> {
    plan.expected_lands
        .iter()
        .copied()
        .map(usize::from)
        .filter(|target| *target > controlled_lands)
        .min()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::policies::registry::PolicyRegistry;
    use crate::policies::self_cost_value::SelfCostValuePolicy;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, Effect, FilterProp,
        StaticDefinition, TargetFilter, TypedFilter,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::{CyclingCost, Keyword};
    use engine::types::mana::ManaCost;

    const AI: PlayerId = PlayerId(0);

    fn baseline_plan() -> PlanSnapshot {
        PlanSnapshot {
            expected_lands: [1, 2, 3, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 6, 6],
            ..PlanSnapshot::default()
        }
    }

    fn ramp_plan() -> PlanSnapshot {
        PlanSnapshot {
            expected_lands: [1, 2, 4, 5, 6, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7],
            ..PlanSnapshot::default()
        }
    }

    fn add_cycler(
        state: &mut GameState,
        name: &str,
        core_type: CoreType,
        keyword: Keyword,
    ) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, AI, name.to_string(), Zone::Hand);
        let ability = engine::database::synthesis::cycling_ability_for_keyword(&keyword)
            .expect("cycling keyword must synthesize an activated ability");
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(core_type);
        object.base_card_types = object.card_types.clone();
        Arc::make_mut(&mut object.abilities).push(ability);
        id
    }

    fn add_land(state: &mut GameState, zone: Zone) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        let id = create_object(state, card_id, AI, "Land".to_string(), zone);
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Land);
        object.base_card_types = object.card_types.clone();
        id
    }

    fn candidate(source_id: ObjectId) -> CandidateAction {
        CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        }
    }

    fn policy_context<'a>(
        state: &'a GameState,
        candidate: &'a CandidateAction,
        decision: &'a AiDecisionContext,
        config: &'a AiConfig,
        context: &'a AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: AI,
            config,
            context,
            cast_facts: None,
            search_depth: super::super::context::SearchDepth::Root,
        }
    }

    fn ai_context(config: &AiConfig, plan: Option<PlanSnapshot>) -> AiContext {
        let mut session = AiSession::empty();
        if let Some(plan) = plan {
            session.plan.insert(AI, plan);
        }
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        context
    }

    fn cycling_verdict_with_config(
        state: &GameState,
        source_id: ObjectId,
        plan: Option<PlanSnapshot>,
        config: &AiConfig,
    ) -> PolicyVerdict {
        let candidate = candidate(source_id);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: vec![candidate.clone()],
        };
        let context = ai_context(config, plan);
        CyclingDisciplinePolicy.verdict(&policy_context(
            state, &candidate, &decision, config, &context,
        ))
    }

    fn cycling_verdict(
        state: &GameState,
        source_id: ObjectId,
        plan: Option<PlanSnapshot>,
    ) -> PolicyVerdict {
        let config = AiConfig::default();
        cycling_verdict_with_config(state, source_id, plan, &config)
    }

    fn self_cost_verdict(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let candidate = candidate(source_id);
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: vec![candidate.clone()],
        };
        let config = AiConfig::default();
        let context = ai_context(&config, None);
        SelfCostValuePolicy.verdict(&policy_context(
            state, &candidate, &decision, &config, &context,
        ))
    }

    fn assert_score(verdict: PolicyVerdict, expected_kind: &str, expected_delta: f64) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, expected_kind);
                assert_eq!(delta, expected_delta);
                assert!(delta.is_finite());
            }
            PolicyVerdict::Reject { reason } => {
                panic!("expected finite score, got reject {}", reason.kind)
            }
        }
    }

    #[test]
    fn ordinary_cycling_gets_finite_patience() {
        let mut state = GameState::new_two_player(42);
        let cycler = add_cycler(
            &mut state,
            "Cycler",
            CoreType::Creature,
            Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(2))),
        );

        assert_score(
            cycling_verdict(&state, cycler, None),
            "cycling_discipline_patience",
            AiConfig::default()
                .policy_penalties
                .cycling_patience_penalty,
        );
    }

    #[test]
    fn cycling_patience_uses_configured_penalty() {
        let mut state = GameState::new_two_player(42);
        let cycler = add_cycler(
            &mut state,
            "Cycler",
            CoreType::Creature,
            Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(2))),
        );
        let mut config = AiConfig::default();
        config.policy_penalties.cycling_patience_penalty = -1.25;

        assert_score(
            cycling_verdict_with_config(&state, cycler, None, &config),
            "cycling_discipline_patience",
            -1.25,
        );
    }

    #[test]
    fn sole_planned_land_gets_stronger_finite_patience() {
        let mut state = GameState::new_two_player(42);
        for _ in 0..5 {
            add_land(&mut state, Zone::Battlefield);
        }
        let cycler = add_cycler(
            &mut state,
            "Cycling Land",
            CoreType::Land,
            Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(2))),
        );

        assert_score(
            cycling_verdict(&state, cycler, Some(baseline_plan())),
            "cycling_discipline_needed_land",
            AiConfig::default()
                .policy_penalties
                .cycling_needed_land_penalty,
        );

        add_land(&mut state, Zone::Hand);
        assert_score(
            cycling_verdict(&state, cycler, Some(baseline_plan())),
            "cycling_discipline_patience",
            AiConfig::default()
                .policy_penalties
                .cycling_patience_penalty,
        );
    }

    #[test]
    fn completed_land_plan_uses_ordinary_patience() {
        let mut state = GameState::new_two_player(42);
        for _ in 0..6 {
            add_land(&mut state, Zone::Battlefield);
        }
        let cycler = add_cycler(
            &mut state,
            "Cycling Land",
            CoreType::Land,
            Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(2))),
        );

        assert_score(
            cycling_verdict(&state, cycler, Some(baseline_plan())),
            "cycling_discipline_patience",
            AiConfig::default()
                .policy_penalties
                .cycling_patience_penalty,
        );
    }

    #[test]
    fn planned_land_targets_follow_baseline_and_ramp_caps() {
        assert_eq!(next_planned_land_target(&baseline_plan(), 5), Some(6));
        assert_eq!(next_planned_land_target(&baseline_plan(), 6), None);
        assert_eq!(next_planned_land_target(&ramp_plan(), 6), Some(7));
        assert_eq!(next_planned_land_target(&ramp_plan(), 7), None);
    }

    #[test]
    fn typecycling_defers_self_cost_but_untagged_tutor_does_not() {
        let mut state = GameState::new_two_player(42);
        let typecycler = add_cycler(
            &mut state,
            "Wizardcycler",
            CoreType::Creature,
            Keyword::Typecycling {
                cost: ManaCost::generic(1),
                subtype: "Wizard".to_string(),
            },
        );
        assert_score(
            self_cost_verdict(&state, typecycler),
            "self_cost_cycling_deferred",
            0.0,
        );

        let untagged_id = {
            let card_id = CardId(state.next_object_id);
            let id = create_object(
                &mut state,
                card_id,
                AI,
                "Discard Tutor".to_string(),
                Zone::Hand,
            );
            let mut untagged = state.objects[&typecycler].abilities[0].clone();
            untagged.ability_tag = None;
            let object = state.objects.get_mut(&id).unwrap();
            object.card_types.core_types.push(CoreType::Creature);
            object.base_card_types = object.card_types.clone();
            Arc::make_mut(&mut object.abilities).push(untagged);
            id
        };
        assert!(matches!(
            self_cost_verdict(&state, untagged_id),
            PolicyVerdict::Reject { .. }
        ));
    }

    #[test]
    fn runtime_granted_typecycling_uses_the_effective_action_index() {
        let mut state = GameState::new_two_player(42);
        let grantor = create_object(
            &mut state,
            CardId(100),
            AI,
            "Homing Sliver".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&grantor)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::continuous()
                    .affected(TargetFilter::Typed(
                        TypedFilter::card()
                            .subtype("Sliver".to_string())
                            .properties(vec![FilterProp::InZone { zone: Zone::Hand }]),
                    ))
                    .modifications(vec![ContinuousModification::AddKeyword {
                        keyword: Keyword::Typecycling {
                            cost: ManaCost::NoCost,
                            subtype: "Sliver".to_string(),
                        },
                    }]),
            );

        let hand_sliver = create_object(
            &mut state,
            CardId(101),
            AI,
            "Striking Sliver".to_string(),
            Zone::Hand,
        );
        {
            let object = state.objects.get_mut(&hand_sliver).unwrap();
            object.card_types.core_types.push(CoreType::Creature);
            object.card_types.subtypes.push("Sliver".to_string());
            object.base_card_types = object.card_types.clone();
            Arc::make_mut(&mut object.abilities)
                .push(AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp));
        }

        let runtime_candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: hand_sliver,
                ability_index: 1,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: vec![runtime_candidate.clone()],
        };
        let config = AiConfig::default();
        let context = ai_context(&config, None);
        let ctx = policy_context(&state, &runtime_candidate, &decision, &config, &context);

        let effective = ctx
            .effective_activated_ability()
            .expect("runtime Typecycling must resolve at the engine-provided index");
        assert_eq!(effective.ability_tag, Some(AbilityTag::Cycling));
        assert_score(
            CyclingDisciplinePolicy.verdict(&ctx),
            "cycling_discipline_patience",
            config.policy_penalties.cycling_patience_penalty,
        );
        assert_score(
            SelfCostValuePolicy.verdict(&ctx),
            "self_cost_cycling_deferred",
            0.0,
        );

        let printed_candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: hand_sliver,
                ability_index: 0,
            },
            metadata: runtime_candidate.metadata.clone(),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: vec![printed_candidate.clone()],
        };
        assert_score(
            CyclingDisciplinePolicy.verdict(&policy_context(
                &state,
                &printed_candidate,
                &decision,
                &config,
                &context,
            )),
            "cycling_discipline_na",
            0.0,
        );
    }

    #[test]
    fn untagged_activation_is_not_cycling() {
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(state.next_object_id);
        let source = create_object(
            &mut state,
            card_id,
            AI,
            "Draw Ability".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities)
            .push(AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp));

        assert_score(
            cycling_verdict(&state, source, None),
            "cycling_discipline_na",
            0.0,
        );
    }

    #[test]
    fn policy_is_registered() {
        assert!(PolicyRegistry::default().has_policy(PolicyId::CyclingDiscipline));
    }
}
