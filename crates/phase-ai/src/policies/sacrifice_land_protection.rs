//! Sacrifice-land-for-protection tactical policy (issue #771).
//!
//! Sylvan Safekeeper ("Sacrifice a land: Target creature you control gains shroud
//! until end of turn.") and the whole land-sacrifice defensive-grant class were
//! being activated every turn for no payoff — burning mana development for a
//! grant that expires (CR 514.2) before any threat can matter.
//!
//! `ReactiveSelfProtectionPolicy` already hard-rejects most no-payoff protection
//! activations; this policy is a defense-in-depth gate keyed on the **land
//! sacrifice cost axis** so land outlets cannot slip through if the generic
//! classifier or search path ever mis-scores them. It reuses
//! `self_protection_classify` for effect/threat assessment — no parallel heuristics.
//!
//! CR 701.21: Sacrifice moves the chosen land to the graveyard.
//! CR 702.18a: Shroud prevents targeting — only valuable when a spell/ability is
//! about to target the protected permanent.
//! CR 117.1a: Holding the ability until a threat appears is strictly better than
//! pre-emptive activation.

use engine::types::ability::AbilityDefinition;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_protection_classify::{
    any_immediate_threat, combat_step_allows_protection,
    is_land_sacrifice_self_protection_activation,
};
use crate::features::DeckFeatures;

/// Below this many lands on the battlefield, sacrificing one for a grant with no
/// threat is especially costly — amplify the veto with an extra preference-band
/// penalty so search cannot prefer the line when multiple pass-equivalent
/// candidates exist. Not a substitute for `Reject` on the no-payoff path.
const LOW_LAND_COUNT_FLOOR: usize = 4;

pub struct SacrificeLandProtectionPolicy;

impl TacticalPolicy for SacrificeLandProtectionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SacrificeLandProtection
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
        // activation-constant: classifier-gated land-sacrifice protection policy.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let ability = match activation_ability(ctx) {
            Some(ability) => ability,
            None => {
                return PolicyVerdict::neutral(PolicyReason::new("sacrifice_land_protection_na"));
            }
        };

        if !is_land_sacrifice_self_protection_activation(ability) {
            return PolicyVerdict::neutral(PolicyReason::new("sacrifice_land_protection_na"));
        }

        if any_immediate_threat(ctx.state, ctx.ai_player) {
            return PolicyVerdict::neutral(PolicyReason::new(
                "sacrifice_land_protection_threat_present",
            ));
        }

        if combat_step_allows_protection(ctx.state) {
            return PolicyVerdict::neutral(PolicyReason::new(
                "sacrifice_land_protection_combat_payoff",
            ));
        }

        let land_count = count_controlled_lands(ctx);
        if land_count < LOW_LAND_COUNT_FLOOR {
            return PolicyVerdict::Reject {
                reason: PolicyReason::new("sacrifice_land_protection_low_lands")
                    .with_fact("lands_on_battlefield", land_count as i64),
            };
        }

        PolicyVerdict::Reject {
            reason: PolicyReason::new("sacrifice_land_protection_no_payoff"),
        }
    }
}

fn activation_ability<'a>(ctx: &'a PolicyContext<'a>) -> Option<&'a AbilityDefinition> {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = &ctx.candidate.action
    else {
        return None;
    };
    ctx.state
        .objects
        .get(source_id)
        .and_then(|object| object.abilities.get(*ability_index))
}

fn count_controlled_lands(ctx: &PolicyContext<'_>) -> usize {
    use engine::types::card_type::CoreType;
    ctx.state
        .battlefield
        .iter()
        .filter_map(|id| ctx.state.objects.get(id))
        .filter(|obj| {
            obj.controller == ctx.ai_player && obj.card_types.core_types.contains(&CoreType::Land)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, SacrificeCost,
        SacrificeRequirement, StaticDefinition, TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn sylvan_safekeeper_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            engine::types::ability::Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous()
                    .affected(TargetFilter::ParentTarget)
                    .modifications(vec![
                        engine::types::ability::ContinuousModification::AddKeyword {
                            keyword: Keyword::Shroud,
                        },
                    ])],
                target: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                duration: None,
            },
        );
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            requirement: SacrificeRequirement::count(1),
        }));
        ability
    }

    fn safekeeper_on_battlefield(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Sylvan Safekeeper".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities)
            .push(sylvan_safekeeper_ability());
        id
    }

    fn add_lands(state: &mut GameState, count: usize) {
        use engine::types::card_type::CoreType;
        for i in 0..count {
            let id = create_object(
                state,
                CardId(100 + i as u32),
                AI,
                format!("Forest {i}"),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Land);
        }
    }

    fn activate_verdict(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
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
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
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
        SacrificeLandProtectionPolicy.verdict(&ctx)
    }

    #[test]
    fn safekeeper_no_threat_rejected() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        add_lands(&mut state, 5);
        let id = safekeeper_on_battlefield(&mut state);
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => panic!("expected reject for no-payoff Safekeeper"),
        }
    }

    #[test]
    fn safekeeper_low_land_count_rejected_with_fact() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        add_lands(&mut state, 2);
        let id = safekeeper_on_battlefield(&mut state);
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_low_lands");
                assert!(reason
                    .facts
                    .iter()
                    .any(|(k, v)| k == &"lands_on_battlefield" && *v == 2));
            }
            PolicyVerdict::Score { .. } => panic!("expected reject when land count is low"),
        }
    }

    #[test]
    fn non_land_sacrifice_activation_unaffected() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Mother of Runes".to_string(),
            Zone::Battlefield,
        );
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            engine::types::ability::Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous()
                    .affected(TargetFilter::ParentTarget)
                    .modifications(vec![
                        engine::types::ability::ContinuousModification::AddKeyword {
                            keyword: Keyword::Shroud,
                        },
                    ])],
                target: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                duration: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(ability);

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("tap-only protection is out of scope"),
        }
    }

    #[test]
    fn draw_ability_unaffected() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Outpost".to_string(),
            Zone::Battlefield,
        );
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            engine::types::ability::Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
            requirement: SacrificeRequirement::count(1),
        }));
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(ability);

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("non-protection sac land must not reject"),
        }
    }

    #[test]
    fn parsed_sylvan_safekeeper_rejected_by_both_policies() {
        use super::super::reactive_self_protection::ReactiveSelfProtectionPolicy;
        use engine::parser::oracle::parse_oracle_text;

        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        add_lands(&mut state, 5);

        let parsed = parse_oracle_text(
            "Sacrifice a land: Target creature you control gains shroud until end of turn.",
            "Sylvan Safekeeper",
            &[],
            &["Creature"],
            &["Human", "Wizard"],
        );
        let ability = parsed
            .abilities
            .into_iter()
            .next()
            .expect("one activated ability");
        let id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Sylvan Safekeeper".to_string(),
            Zone::Battlefield,
        );
        *Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities) = vec![ability];

        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => {
                panic!("SacrificeLandProtection must reject parsed Safekeeper")
            }
        }

        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        match ReactiveSelfProtectionPolicy.verdict(&ctx) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => {
                panic!("ReactiveSelfProtection must reject parsed Safekeeper")
            }
        }
    }
}
