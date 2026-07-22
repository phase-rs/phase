//! Reactive self-protection tactical policy.
//!
//! Rejects the AI casting OR activating "save yourself" effects without a
//! payoff. Casts retain the broad immediate-threat/combat gate; activated
//! object-protection abilities require an exact recipient, an answerable stack
//! threat, or a concrete combat interaction. Empirically observed: AI casting
//! Teferi's Protection on turn 3 against an empty board; AI repeatedly paying
//! "discard a card: ~ gains protection from everything" until its hand is empty;
//! AI activating Sylvan Safekeeper ("sacrifice a land: target creature you
//! control gains shroud") on turn 1 for no reason (issue #771); and AI paying 3
//! life for Arco-Flagellant's indestructible grant once every turn.
//!
//! Classification and threat assessment live in `self_protection_classify` —
//! this policy is the spell/activation gate only. Land-sacrifice outlets also
//! pass through `SacrificeLandProtectionPolicy` for defense in depth.
//!
//! CR 117.1a: instants can be cast at any time priority is held — leaving
//! protection in hand for the moment a threat arrives is strictly better
//! than burning it pre-emptively.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_protection_classify::{
    any_immediate_threat, combat_step_allows_protection, is_self_protection_effect,
    self_protection_activation_payoff,
};
use crate::cast_facts::collect_definition_effects;
use crate::features::DeckFeatures;

pub struct ReactiveSelfProtectionPolicy;

impl TacticalPolicy for ReactiveSelfProtectionPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ReactiveSelfProtection
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell, DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // activation-constant: classifier-gated reactive self-protection policy.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        if let GameAction::ActivateAbility {
            source_id,
            ability_index: _,
        } = &ctx.candidate.action
        {
            let Some(ability) = ctx.effective_activated_ability() else {
                return PolicyVerdict::neutral(PolicyReason::new("reactive_self_protection_na"));
            };
            if !collect_definition_effects(&ability)
                .into_iter()
                .any(is_self_protection_effect)
            {
                return PolicyVerdict::neutral(PolicyReason::new("reactive_self_protection_na"));
            }
            return match self_protection_activation_payoff(
                ctx.state,
                ctx.ai_player,
                *source_id,
                &ability,
            ) {
                Some(true) => PolicyVerdict::neutral(PolicyReason::new(
                    "reactive_self_protection_exact_payoff",
                )),
                Some(false) => PolicyVerdict::Reject {
                    reason: PolicyReason::new("reactive_self_protection_no_payoff"),
                },
                None => {
                    PolicyVerdict::neutral(PolicyReason::new("reactive_self_protection_unmodeled"))
                }
            };
        }

        if !matches!(ctx.candidate.action, GameAction::CastSpell { .. }) {
            return PolicyVerdict::neutral(PolicyReason::new("reactive_self_protection_na"));
        }

        let Some(cast_facts) = ctx.cast_facts() else {
            return PolicyVerdict::neutral(PolicyReason::new("reactive_self_protection_na"));
        };
        let spell_effects: Vec<_> = cast_facts
            .primary_effects
            .iter()
            .flat_map(|ability| collect_definition_effects(ability))
            .collect();
        // A mixed chain or modal spell may have a valuable non-protection line.
        // Reject the cast only when every reachable spell effect is itself a
        // self-protection effect; otherwise preserve the existing fail-open.
        let is_protection_spell =
            !spell_effects.is_empty() && spell_effects.into_iter().all(is_self_protection_effect);
        if !is_protection_spell {
            return PolicyVerdict::neutral(PolicyReason::new("reactive_self_protection_na"));
        }

        if any_immediate_threat(ctx.state, ctx.ai_player) {
            return PolicyVerdict::neutral(PolicyReason::new(
                "reactive_self_protection_threat_present",
            ));
        }

        if combat_step_allows_protection(ctx.state) {
            return PolicyVerdict::neutral(PolicyReason::new(
                "reactive_self_protection_combat_payoff",
            ));
        }

        PolicyVerdict::Reject {
            reason: PolicyReason::new("reactive_self_protection_no_payoff"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::self_protection_classify::THREAT_FLOOR;
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect,
        QuantityExpr, StaticDefinition, TargetFilter, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::eval::threat_level;

    const AI: PlayerId = PlayerId(0);

    fn grant_effect(
        affected: Option<TargetFilter>,
        target: Option<TargetFilter>,
        keyword: Keyword,
    ) -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected,
                modifications: vec![ContinuousModification::AddKeyword { keyword }],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
                source_controller: None,
                source_object: None,
                bypass_beneficiary: None,
            }],
            target,
            duration: None,
        }
    }

    fn ai_object_with_activated_ability(
        state: &mut GameState,
        ability: AbilityDefinition,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Self-Protector".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(ability);
        id
    }

    fn ai_object_with_activated(state: &mut GameState, effect: Effect) -> ObjectId {
        ai_object_with_activated_ability(
            state,
            AbilityDefinition::new(AbilityKind::Activated, effect),
        )
    }

    fn activate_verdict(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        ReactiveSelfProtectionPolicy.verdict(&ctx)
    }

    fn cast_verdict(state: &GameState, object_id: ObjectId) -> PolicyVerdict {
        let object = state.objects.get(&object_id).expect("cast object exists");
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id: object.card_id,
                targets: Vec::new(),
                payment_mode: Default::default(),
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
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
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        ReactiveSelfProtectionPolicy.verdict(&ctx)
    }

    fn indestructible_grant_to_self() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible,
                }],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
                source_controller: None,
                source_object: None,
                bypass_beneficiary: None,
            }],
            target: None,
            duration: None,
        }
    }

    #[test]
    fn classifier_recognises_self_indestructible_grant() {
        assert!(is_self_protection_effect(&indestructible_grant_to_self()));
    }

    #[test]
    fn casting_permanent_with_activated_protection_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(20),
            AI,
            "Arco-Flagellant".to_string(),
            Zone::Hand,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        ));

        match cast_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(delta, 0.0);
                assert_eq!(reason.kind, "reactive_self_protection_na");
            }
            PolicyVerdict::Reject { .. } => {
                panic!("an activated ability is not the permanent spell's effect")
            }
        }
    }

    #[test]
    fn casting_protection_spell_without_threat_is_rejected() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(21),
            AI,
            "Protection Instant".to_string(),
            Zone::Hand,
        );
        let object = state.objects.get_mut(&id).unwrap();
        object.card_types.core_types.push(CoreType::Instant);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        ));

        assert!(matches!(
            cast_verdict(&state, id),
            PolicyVerdict::Reject { .. }
        ));
    }

    #[test]
    fn casting_mixed_protection_chain_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(22),
            AI,
            "Mixed Protection Spell".to_string(),
            Zone::Hand,
        );
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )));
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(ability);

        assert!(matches!(
            cast_verdict(&state, id),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn casting_modal_spell_with_nonprotection_mode_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(23),
            AI,
            "Modal Protection Spell".to_string(),
            Zone::Hand,
        );
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        ability.mode_abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities).push(ability);

        assert!(matches!(
            cast_verdict(&state, id),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn classifier_recognises_self_phaseout() {
        let effect = Effect::PhaseOut {
            target: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
        };
        assert!(is_self_protection_effect(&effect));
    }

    #[test]
    fn classifier_rejects_opponent_indestructible_grant() {
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible,
                }],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
                source_controller: None,
                source_object: None,
                bypass_beneficiary: None,
            }],
            target: None,
            duration: None,
        };
        assert!(!is_self_protection_effect(&effect));
    }

    #[test]
    fn classifier_ignores_unrelated_proliferate_effect() {
        assert!(!is_self_protection_effect(&Effect::Proliferate));
    }

    #[test]
    fn stack_targeting_ai_permanent_counts_as_threat() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let ai_player = PlayerId(1);
        let opp = PlayerId(0);

        let ai_creature = create_object(
            &mut state,
            CardId(1),
            ai_player,
            "AI Creature".to_string(),
            Zone::Battlefield,
        );
        let spell_id = create_object(
            &mut state,
            CardId(99),
            opp,
            "Doom Blade".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(ai_creature)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert!(any_immediate_threat(&state, ai_player));
    }

    #[test]
    fn no_threat_on_empty_state() {
        let state = GameState::new_two_player(42);
        assert!(!any_immediate_threat(&state, PlayerId(1)));
    }

    #[test]
    fn classifier_recognises_self_ref_indestructible_grant() {
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::SelfRef),
                modifications: vec![ContinuousModification::AddKeyword {
                    keyword: Keyword::Indestructible,
                }],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
                source_controller: None,
                source_object: None,
                bypass_beneficiary: None,
            }],
            target: None,
            duration: None,
        };
        assert!(is_self_protection_effect(&effect));
    }

    #[test]
    fn arco_flagellant_oracle_reaches_typed_pay_life_indestructible_activation() {
        use engine::parser::oracle::parse_oracle_text;
        use engine::types::ability::AbilityCost;

        let parsed = parse_oracle_text(
            "Pay 3 life: Arco-Flagellant gains indestructible until end of turn.",
            "Arco-Flagellant",
            &[],
            &["Creature".to_string()],
            &[],
        );
        let ability = parsed
            .abilities
            .iter()
            .find(|ability| ability.kind == AbilityKind::Activated)
            .expect("activated ability must parse");
        assert!(matches!(
            ability.cost.as_ref(),
            Some(AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 3 }
            })
        ));
        assert!(is_self_protection_effect(&ability.effect));
        assert!(!matches!(
            ability.effect.as_ref(),
            Effect::Unimplemented { .. }
        ));
    }

    #[test]
    fn classifier_recognises_parent_target_grant_to_you() {
        assert!(is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You)
            )),
            Keyword::Shroud,
        )));
    }

    #[test]
    fn classifier_rejects_parent_target_grant_to_opponent() {
        assert!(!is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::Opponent)
            )),
            Keyword::Shroud,
        )));
    }

    #[test]
    fn activation_self_ref_protection_no_threat_rejected() {
        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => panic!("expected reject for no-payoff activation"),
        }
    }

    #[test]
    fn activation_with_valuable_modal_branch_without_threat_fails_open() {
        let mut state = GameState::new_two_player(42);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        ability.mode_abilities.push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ));
        let id = ai_object_with_activated_ability(&mut state, ability);

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { reason, .. } => {
                assert_eq!(reason.kind, "reactive_self_protection_unmodeled");
            }
            PolicyVerdict::Reject { .. } => {
                panic!("a valuable alternate activation branch must fail open")
            }
        }
    }

    #[test]
    fn activation_with_valuable_else_branch_without_threat_fails_open() {
        let mut state = GameState::new_two_player(42);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        ability.else_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )));
        let id = ai_object_with_activated_ability(&mut state, ability);

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { reason, .. } => {
                assert_eq!(reason.kind, "reactive_self_protection_unmodeled");
            }
            PolicyVerdict::Reject { .. } => {
                panic!("a valuable else activation branch must fail open")
            }
        }
    }

    #[test]
    fn activation_self_ref_indestructible_low_life_without_threat_rejected() {
        let mut state = GameState::new_two_player(42);
        state.players[AI.0 as usize].life = 5;
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );

        assert!(matches!(
            activate_verdict(&state, id),
            PolicyVerdict::Reject { .. }
        ));
    }

    #[test]
    fn activation_redundant_self_indestructible_rejected() {
        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .keywords
            .push(Keyword::Indestructible);

        assert!(matches!(
            activate_verdict(&state, id),
            PolicyVerdict::Reject { .. }
        ));
    }

    #[test]
    fn activation_parent_target_protection_no_threat_rejected() {
        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
                Keyword::Shroud,
            ),
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => panic!("expected reject for no-payoff activation"),
        }
    }

    #[test]
    fn activation_non_protection_unaffected() {
        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    #[test]
    fn activation_self_protection_with_threat_allowed() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        let opp = PlayerId(1);
        let spell_id = create_object(
            &mut state,
            CardId(99),
            opp,
            "Doom Blade".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(id)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_exact_payoff");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    #[test]
    fn destruction_of_another_creature_does_not_justify_self_indestructible() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        let other = create_object(
            &mut state,
            CardId(2),
            AI,
            "Other Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&other)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let opp = PlayerId(1);
        let spell_id = create_object(
            &mut state,
            CardId(99),
            opp,
            "Doom Blade".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(other)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        assert!(matches!(
            activate_verdict(&state, id),
            PolicyVerdict::Reject { .. }
        ));
    }

    #[test]
    fn lethal_damage_justifies_self_indestructible() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        state.objects.get_mut(&id).unwrap().toughness = Some(3);
        let opp = PlayerId(1);
        let spell_id = create_object(
            &mut state,
            CardId(99),
            opp,
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(id)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { reason, .. } => {
                assert_eq!(reason.kind, "reactive_self_protection_exact_payoff");
            }
            PolicyVerdict::Reject { .. } => panic!("lethal damage is an indestructible payoff"),
        }
    }

    #[test]
    fn nonlethal_damage_to_self_fails_open() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );
        state.objects.get_mut(&id).unwrap().toughness = Some(3);
        let opp = PlayerId(1);
        let spell_id = create_object(&mut state, CardId(99), opp, "Ping".to_string(), Zone::Stack);
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
            vec![TargetRef::Object(id)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(ability),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { reason, .. } => {
                assert_eq!(reason.kind, "reactive_self_protection_unmodeled");
            }
            PolicyVerdict::Reject { .. } => panic!("ambiguous damage must fail open"),
        }
    }

    fn strong_opponent_creature(state: &mut GameState, owner: PlayerId) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            "Big Threat".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(14);
        obj.toughness = Some(14);
        id
    }

    #[test]
    fn board_pressure_not_a_threat_on_ai_own_turn() {
        let mut state = GameState::new_two_player(42);
        let opp = PlayerId(1);
        strong_opponent_creature(&mut state, opp);
        assert!(threat_level(&state, AI, opp) >= THREAT_FLOOR);

        let id = ai_object_with_activated(
            &mut state,
            grant_effect(Some(TargetFilter::SelfRef), None, Keyword::Indestructible),
        );

        state.active_player = AI;
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => panic!("expected reject on own non-combat turn"),
        }

        state.active_player = opp;
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => {
                panic!("board pressure alone cannot justify self-indestructible")
            }
        }
    }

    #[test]
    fn mother_of_runes_own_main_phase_rejected() {
        use engine::types::keywords::ProtectionTarget;

        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
                Keyword::Protection(ProtectionTarget::ChosenColor),
            ),
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => panic!("Mother of Runes must be rejected on own main"),
        }
    }

    #[test]
    fn protection_without_attacker_or_legal_blocker_rejected_during_own_combat() {
        use engine::types::keywords::ProtectionTarget;
        use engine::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::DeclareBlockers;
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
                Keyword::Protection(ProtectionTarget::ChosenColor),
            ),
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => panic!("empty combat has no protection payoff"),
        }
    }

    #[test]
    fn protection_rejected_at_begin_combat() {
        use engine::types::keywords::ProtectionTarget;
        use engine::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::BeginCombat;
        let id = ai_object_with_activated(
            &mut state,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::You),
                )),
                Keyword::Protection(ProtectionTarget::ChosenColor),
            ),
        );
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "reactive_self_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => panic!("begin-of-combat has no payoff; must reject"),
        }
    }
}
