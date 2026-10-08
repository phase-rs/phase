//! Prioritizes combat withdrawals that prevent a controller's certain creature loss.

use engine::ai_support::{
    combat_withdrawal_fact_for_current_target, current_target_selection_targets,
    CombatWithdrawalFact,
};
use engine::game::combat_damage::CombatImpact;
use engine::game::engine::apply_as_current_for_simulation;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

pub struct CombatWithdrawalPolicy;

impl TacticalPolicy for CombatWithdrawalPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CombatWithdrawal
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility, DecisionKind::SelectTarget]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // activation-constant: exact combat-withdrawal action self-gates in verdict.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        match &ctx.candidate.action {
            GameAction::ActivateAbility { .. } => priority_verdict(ctx),
            GameAction::ChooseTarget {
                target: Some(target),
            } => target_verdict(ctx, target),
            _ => PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_na")),
        }
    }
}

fn priority_verdict(ctx: &PolicyContext<'_>) -> PolicyVerdict {
    let Some(actor) = ctx.candidate.metadata.semantic_owner else {
        return PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_na"));
    };
    let mut simulated = ctx.state.clone();
    if apply_as_current_for_simulation(&mut simulated, ctx.candidate.action.clone()).is_err() {
        return PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_na"));
    }

    let facts = exact_target_facts(&simulated, actor);
    if facts.is_empty() {
        return PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_na"));
    }
    if facts.iter().any(|fact| is_lost_rescue(fact, actor)) {
        return PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_rescue_available"));
    }

    PolicyVerdict::reject(PolicyReason::new("combat_withdrawal_futile_activation"))
}

fn target_verdict(
    ctx: &PolicyContext<'_>,
    target: &engine::types::ability::TargetRef,
) -> PolicyVerdict {
    let Some(actor) = ctx.candidate.metadata.semantic_owner else {
        return PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_na"));
    };
    let Some(selected) = combat_withdrawal_fact_for_current_target(ctx.state, actor, target) else {
        return PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_na"));
    };
    if is_lost_rescue(&selected, actor) {
        return PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_rescue_target"));
    }
    if exact_target_facts(ctx.state, actor)
        .iter()
        .any(|fact| is_lost_rescue(fact, actor))
    {
        PolicyVerdict::strong(
            -ctx.penalties().combat_withdrawal_futile_penalty,
            PolicyReason::new("combat_withdrawal_futile_target"),
        )
    } else {
        PolicyVerdict::neutral(PolicyReason::new("combat_withdrawal_no_rescue"))
    }
}

/// Reads sibling targets only through the engine's current-slot authority.
fn exact_target_facts(state: &GameState, actor: PlayerId) -> Vec<CombatWithdrawalFact> {
    current_target_selection_targets(state)
        .into_iter()
        .flatten()
        .filter_map(|target| combat_withdrawal_fact_for_current_target(state, actor, target))
        .collect()
}

fn is_lost_rescue(fact: &CombatWithdrawalFact, actor: PlayerId) -> bool {
    let CombatWithdrawalFact::CombatPair {
        attacker_controller,
        blocker_controller,
        impact: CombatImpact::Fixed { survival, .. },
        ..
    } = fact
    else {
        return false;
    };
    (*attacker_controller == actor && !survival.attacker_survives)
        || (*blocker_controller == actor && !survival.blocker_survives)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, EffectKind, PtValue, ResolvedAbility, TargetFilter,
        TargetRef, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{
        PendingCast, TargetEffectDetail, TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
    };
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    const AI: PlayerId = PlayerId(0);
    const OPPONENT: PlayerId = PlayerId(1);

    fn creature(
        state: &mut GameState,
        controller: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let object_id = create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        let object = state
            .objects
            .get_mut(&object_id)
            .expect("created object exists");
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(power);
        object.toughness = Some(toughness);
        object_id
    }

    fn combat_state(
        attacker_power: i32,
        blocker_toughness: i32,
    ) -> (GameState, ObjectId, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = creature(&mut state, AI, "Withdrawal Source", 1, 1);
        let attacker = creature(&mut state, OPPONENT, "Attacker", attacker_power, 4);
        let blocker = creature(&mut state, AI, "Blocker", 2, blocker_toughness);
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo {
                object_id: attacker,
                defending_player: AI,
                attack_target: AttackTarget::Player(AI),
                blocked: true,
                band_id: None,
            }],
            blocker_assignments: HashMap::from([(attacker, vec![blocker])]),
            blocker_to_attacker: HashMap::from([(blocker, vec![attacker])]),
            ..Default::default()
        });
        (state, source, attacker, blocker)
    }

    fn direct_ability(source: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            Vec::new(),
            source,
            AI,
        )
    }

    fn parent_target_ability(source: ObjectId) -> ResolvedAbility {
        let mut root = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            Vec::new(),
            source,
            AI,
        );
        root.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::RemoveFromCombat {
                target: TargetFilter::ParentTarget,
            },
            Vec::new(),
            source,
            AI,
        )));
        root
    }

    fn install_target_selection(
        state: &mut GameState,
        source: ObjectId,
        ability: ResolvedAbility,
        legal_targets: Vec<TargetRef>,
    ) {
        let mut pending = PendingCast::new(source, CardId(99), ability, ManaCost::zero());
        pending.activation_ability_index = Some(0);
        state.waiting_for = WaitingFor::TargetSelection {
            player: AI,
            pending_cast: Box::new(pending),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: legal_targets.clone(),
                optional: false,
                chooser: None,
                effect_kind: EffectKind::NoOp,
                effect_detail: TargetEffectDetail::None,
            }],
            mode_labels: Vec::new(),
            selection: TargetSelectionProgress {
                current_slot: 0,
                selected_slots: Vec::new(),
                current_legal_targets: legal_targets,
            },
        };
    }

    fn verdict_for(state: &GameState, action: GameAction, actor: PlayerId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action,
            metadata: ActionMetadata::for_actor(Some(actor), TacticalClass::Target),
        };
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: vec![candidate.clone()],
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        CombatWithdrawalPolicy.verdict(&PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: super::super::context::SearchDepth::Root,
        })
    }

    fn verdict(state: &GameState, action: GameAction) -> PolicyVerdict {
        verdict_for(state, action, AI)
    }

    #[test]
    fn direct_and_parent_target_replays_keep_the_exact_rescue() {
        for ability_builder in [
            direct_ability as fn(ObjectId) -> ResolvedAbility,
            parent_target_ability,
        ] {
            let (mut state, source, attacker, _) = combat_state(4, 2);
            install_target_selection(
                &mut state,
                source,
                ability_builder(source),
                vec![TargetRef::Object(attacker)],
            );

            assert!(matches!(
                verdict(&state, GameAction::ChooseTarget { target: Some(TargetRef::Object(attacker)) }),
                PolicyVerdict::Score { delta: 0.0, reason }
                    if reason.kind == "combat_withdrawal_rescue_target"
            ));
        }
    }

    #[test]
    fn mixed_targets_penalize_only_the_futile_sibling() {
        let (mut state, source, attacker, _) = combat_state(4, 2);
        install_target_selection(
            &mut state,
            source,
            direct_ability(source),
            vec![TargetRef::Object(attacker), TargetRef::Object(source)],
        );

        assert!(matches!(
            verdict(&state, GameAction::ChooseTarget { target: Some(TargetRef::Object(source)) }),
            PolicyVerdict::Score { delta, reason }
                if delta < 0.0 && reason.kind == "combat_withdrawal_futile_target"
        ));
        assert!(matches!(
            verdict(&state, GameAction::ChooseTarget { target: Some(TargetRef::Object(attacker)) }),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "combat_withdrawal_rescue_target"
        ));
    }

    #[test]
    fn single_futile_target_is_neutral_without_an_exact_rescue_sibling() {
        let (mut state, source, attacker, _) = combat_state(1, 3);
        install_target_selection(
            &mut state,
            source,
            direct_ability(source),
            vec![TargetRef::Object(attacker)],
        );

        assert!(matches!(
            verdict(&state, GameAction::ChooseTarget { target: Some(TargetRef::Object(attacker)) }),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "combat_withdrawal_no_rescue"
        ));
    }

    #[test]
    fn target_selection_uses_the_prompt_controller_as_its_authority() {
        let (mut state, source, attacker, _) = combat_state(4, 2);
        install_target_selection(
            &mut state,
            source,
            direct_ability(source),
            vec![TargetRef::Object(attacker)],
        );

        assert!(matches!(
            verdict_for(
                &state,
                GameAction::ChooseTarget { target: Some(TargetRef::Object(attacker)) },
                OPPONENT,
            ),
            PolicyVerdict::Score { delta: 0.0, reason }
                if reason.kind == "combat_withdrawal_na"
        ));
    }

    #[test]
    fn priority_activation_penalizes_all_futile_targets_and_allows_a_rescue() {
        for (attacker_power, blocker_toughness, expected_reason) in [
            (1, 3, "combat_withdrawal_futile_activation"),
            (4, 2, "combat_withdrawal_rescue_available"),
        ] {
            let (mut state, source, _, _) = combat_state(attacker_power, blocker_toughness);
            let source_object = state.objects.get_mut(&source).expect("source exists");
            Arc::make_mut(&mut source_object.abilities).push(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::RemoveFromCombat {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            ));
            state.waiting_for = WaitingFor::Priority { player: AI };
            state.priority_player = AI;

            let result = verdict(
                &state,
                GameAction::ActivateAbility {
                    source_id: source,
                    ability_index: 0,
                },
            );
            match expected_reason {
                "combat_withdrawal_futile_activation" => assert!(matches!(
                    result,
                    PolicyVerdict::Reject { reason } if reason.kind == expected_reason
                )),
                _ => assert!(matches!(
                    result,
                    PolicyVerdict::Score { delta: 0.0, reason }
                        if reason.kind == expected_reason
                )),
            }
        }
    }

    #[test]
    fn controller_passes_on_a_mandatory_single_futile_withdrawal_without_opening_target_selection()
    {
        let (mut state, source, _, _) = combat_state(1, 3);
        let source_object = state.objects.get_mut(&source).expect("source exists");
        Arc::make_mut(&mut source_object.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::RemoveFromCombat {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
        ));
        state.waiting_for = WaitingFor::Priority { player: AI };
        state.priority_player = AI;

        let config = AiConfig::default();
        let mut rng = SmallRng::seed_from_u64(42);
        let action = crate::choose_action(&state, AI, &config, &mut rng);
        assert_eq!(action, Some(GameAction::PassPriority));

        let mut next = state.clone();
        engine::game::engine::apply(&mut next, AI, action.expect("pass action"))
            .expect("the controller-selected pass is engine-legal");
        assert!(
            !matches!(next.waiting_for, WaitingFor::TargetSelection { .. }),
            "rejecting the activation must not open a target-selection prompt"
        );
    }
}
