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
//! `self_protection_classify` for effect/threat classification only.
//!
//! CR 701.21: Sacrifice moves the chosen land to the graveyard.
//! CR 702.18a: Shroud prevents targeting — only valuable when a spell/ability is
//! about to target the protected permanent.
//! CR 117.1a: Holding the ability until a threat appears is strictly better than
//! pre-emptive activation.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_protection_classify::{
    any_land_sacrifice_protection_payoff, is_land_sacrifice_self_protection_activation,
    self_protection_activation_payoff,
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
        let (source_id, ability) = match activation_ability(ctx) {
            Some(activation) => activation,
            None => {
                return PolicyVerdict::neutral(PolicyReason::new("sacrifice_land_protection_na"));
            }
        };

        if !is_land_sacrifice_self_protection_activation(&ability) {
            return PolicyVerdict::neutral(PolicyReason::new("sacrifice_land_protection_na"));
        }

        let exact_payoff =
            self_protection_activation_payoff(ctx.state, ctx.ai_player, source_id, &ability);
        if exact_payoff == Some(true)
            || (exact_payoff.is_none()
                && any_land_sacrifice_protection_payoff(ctx.state, ctx.ai_player, &ability))
        {
            return PolicyVerdict::neutral(PolicyReason::new(
                "sacrifice_land_protection_answerable_threat",
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

fn activation_ability(
    ctx: &PolicyContext<'_>,
) -> Option<(
    engine::types::identifiers::ObjectId,
    engine::types::ability::AbilityDefinition,
)> {
    let GameAction::ActivateAbility { source_id, .. } = &ctx.candidate.action else {
        return None;
    };
    ctx.effective_activated_ability()
        .map(|ability| (*source_id, ability))
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
                end_cost: None,
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
                CardId(100 + i as u64),
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
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
                end_cost: None,
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
    fn safekeeper_rejected_on_opponent_board_pressure_only() {
        use super::super::self_protection_classify::THREAT_FLOOR;
        use crate::eval::threat_level;
        use engine::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let opp = PlayerId(1);
        state.active_player = opp;
        add_lands(&mut state, 5);
        let threat = create_object(
            &mut state,
            CardId(50),
            opp,
            "Big Threat".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&threat).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(14);
        obj.toughness = Some(14);
        assert!(threat_level(&state, AI, opp) >= THREAT_FLOOR);

        let id = safekeeper_on_battlefield(&mut state);
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => {
                panic!("board pressure alone must not waive land-sacrifice shroud gate")
            }
        }
    }

    #[test]
    fn safekeeper_allowed_when_removal_targets_ai_creature() {
        use engine::types::ability::{ResolvedAbility, TargetRef};
        use engine::types::game_state::{StackEntry, StackEntryKind};

        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        add_lands(&mut state, 5);
        let id = safekeeper_on_battlefield(&mut state);
        let creature = create_object(
            &mut state,
            CardId(2),
            AI,
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        let opp = PlayerId(1);
        let spell_id = create_object(
            &mut state,
            CardId(99),
            opp,
            "Doom Blade".to_string(),
            Zone::Stack,
        );
        let ability = ResolvedAbility::new(
            engine::types::ability::Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(creature)],
            spell_id,
            opp,
        );
        state.stack.push_back(StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: opp,
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: Some(Box::new(ability)),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });

        match activate_verdict(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_answerable_threat");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("must allow when removal targets AI creature"),
        }
    }

    #[test]
    fn safekeeper_rejected_at_declare_blockers_without_stack() {
        use engine::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::DeclareBlockers;
        add_lands(&mut state, 5);
        let id = safekeeper_on_battlefield(&mut state);
        match activate_verdict(&state, id) {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "sacrifice_land_protection_no_payoff");
            }
            PolicyVerdict::Score { .. } => {
                panic!("shroud has no combat-only payoff without a stack target")
            }
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
            &["Creature".to_string()],
            &["Human".to_string(), "Wizard".to_string()],
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
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
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

    const SYLVAN_SAFEKEEPER: &str = "Sacrifice a land: Target creature you control gains shroud until end of turn. (It can't be the target of spells or abilities.)";
    const SHOCK: &str = "Shock deals 2 damage to any target.";
    const SCAVENGING_OOZE: &str = "{G}: Exile target card from a graveyard. If it was a creature card, put a +1/+1 counter on this creature and you gain 1 life.";
    const CREMATE: &str = "Exile target card from a graveyard.\nDraw a card.";

    /// Which `PolicyVerdict` variant a policy returned.
    #[derive(Debug, PartialEq)]
    enum VerdictVariant {
        Reject,
        Score,
    }

    /// The verdict variant and reason kind of this policy's and
    /// `ReactiveSelfProtectionPolicy`'s verdicts on activating `source_id`'s
    /// first ability.
    fn verdict_kinds(
        state: &GameState,
        source_id: ObjectId,
    ) -> [(VerdictVariant, &'static str); 2] {
        use super::super::reactive_self_protection::ReactiveSelfProtectionPolicy;
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
        [
            SacrificeLandProtectionPolicy.verdict(&ctx),
            ReactiveSelfProtectionPolicy.verdict(&ctx),
        ]
        .map(|verdict| match verdict {
            PolicyVerdict::Reject { reason } => (VerdictVariant::Reject, reason.kind),
            PolicyVerdict::Score { reason, .. } => (VerdictVariant::Score, reason.kind),
        })
    }

    /// P0 controls a 2/2, Sylvan Safekeeper and five Forests.
    fn safekeeper_board() -> (engine::game::scenario::GameScenario, ObjectId, ObjectId) {
        use engine::game::scenario::{GameScenario, P0};
        use engine::types::mana::ManaColor;
        use engine::types::phase::Phase;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
        let safekeeper = scenario
            .add_creature_from_oracle(P0, "Sylvan Safekeeper", 1, 1, SYLVAN_SAFEKEEPER)
            .id();
        for _ in 0..5 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        (scenario, bear, safekeeper)
    }

    #[test]
    fn safekeeper_allowed_when_a_cast_spell_targets_ai_creature() {
        use engine::game::scenario::P1;
        use engine::types::mana::ManaCost;
        let (mut scenario, bear, safekeeper) = safekeeper_board();
        let murder = scenario
            .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
            .with_mana_cost(ManaCost::zero())
            .id();
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P1;
            state.waiting_for = WaitingFor::Priority { player: P1 };
        }
        runner.cast(murder).target_objects(&[bear]).commit();
        assert_eq!(
            verdict_kinds(runner.state(), safekeeper),
            [
                (
                    VerdictVariant::Score,
                    "sacrifice_land_protection_answerable_threat"
                ),
                (
                    VerdictVariant::Score,
                    "reactive_self_protection_exact_payoff"
                )
            ]
        );
    }

    #[test]
    fn safekeeper_rejected_against_an_attack_trigger_that_targets_nothing() {
        use engine::game::ability_utils::flatten_targets_in_chain;
        use engine::game::combat::AttackTarget;
        use engine::game::scenario::P1;
        use engine::types::ability::TargetRef;
        const SABOTAGE_STRATEGIST: &str = "Flying, vigilance\nWhenever one or more creatures attack you, those creatures get -1/-0 until end of turn.\nExhaust — {5}{U}{U}: Put three +1/+1 counters on this creature. (Activate each exhaust ability only once.)";
        let (mut scenario, bear, safekeeper) = safekeeper_board();
        let strategist = {
            let mut builder = scenario.add_creature(P1, "Sabotage Strategist", 2, 2);
            builder.from_oracle_text_with_keywords(
                &["Exhaust", "Flying", "Vigilance"],
                SABOTAGE_STRATEGIST,
            );
            builder.id()
        };
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(bear, AttackTarget::Player(P1))])
            .expect("declare attackers");
        for _ in 0..4 {
            if runner
                .state()
                .stack
                .iter()
                .any(|e| e.source_id == strategist)
            {
                break;
            }
            runner.act(GameAction::PassPriority).expect("pass priority");
        }
        assert!(
            runner
                .state()
                .stack
                .iter()
                .any(|entry| entry.source_id == strategist
                    && entry.ability().is_some_and(|root| {
                        flatten_targets_in_chain(root) == vec![TargetRef::Object(bear)]
                    })),
            "reach guard: P1's trigger holds the attacking 2/2"
        );
        assert_eq!(
            verdict_kinds(runner.state(), safekeeper),
            [
                (
                    VerdictVariant::Reject,
                    "sacrifice_land_protection_no_payoff"
                ),
                (VerdictVariant::Reject, "reactive_self_protection_no_payoff")
            ]
        );
    }

    #[test]
    fn safekeeper_rejected_when_an_opponent_exiles_the_card_of_a_creature_that_died() {
        use crate::policies::stack_awareness::{chain_has_effect, is_harmful};
        use engine::game::effects::stack_reach::stack_entry_node_reach;
        use engine::game::scenario::{GameRunner, P1};
        use engine::types::ability::TargetRef;
        use engine::types::card_type::CoreType;
        use engine::types::mana::{ManaColor, ManaCost};
        let p1_priority = |runner: &mut GameRunner| {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P1;
            state.waiting_for = WaitingFor::Priority { player: P1 };
        };
        // The Ooze's "if it was a creature card" node leaves the exact payoff
        // undecided, so the fallback decides; the Cremate is decided exactly.
        for (exiler, exact_payoff) in [("Scavenging Ooze", None), ("Cremate", Some(false))] {
            // P1's Shock kills P0's 2/2, then P1's Scavenging Ooze or Cremate
            // aims at its card in P0's graveyard.
            let (mut scenario, bear, safekeeper) = safekeeper_board();
            let shock = scenario
                .add_spell_to_hand_from_oracle(P1, "Shock", true, SHOCK)
                .with_mana_cost(ManaCost::zero())
                .id();
            let ooze = scenario
                .add_creature_from_oracle(P1, "Scavenging Ooze", 2, 2, SCAVENGING_OOZE)
                .id();
            let cremate = scenario
                .add_spell_to_hand_from_oracle(P1, "Cremate", true, CREMATE)
                .with_mana_cost(ManaCost::zero())
                .id();
            scenario.add_basic_land(P1, ManaColor::Green);
            let mut runner = scenario.build();
            p1_priority(&mut runner);
            runner.cast(shock).target_objects(&[bear]).commit();
            runner.advance_until_stack_empty();
            let card = runner
                .state()
                .objects
                .values()
                .find(|object| {
                    object.name == "Bear" && object.owner == AI && object.zone == Zone::Graveyard
                })
                .map(|object| object.id)
                .expect("the 2/2 died to the Shock");
            p1_priority(&mut runner);
            if exiler == "Scavenging Ooze" {
                runner
                    .act(GameAction::ActivateAbility {
                        source_id: ooze,
                        ability_index: 0,
                        payment_mode: engine::types::game_state::CastPaymentMode::Auto,
                    })
                    .expect("activate the Ooze");
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(card)),
                    })
                    .expect("aim at the card");
            } else {
                runner.cast(cremate).target_objects(&[card]).commit();
            }
            let state = runner.state();
            let entry = state.stack.back().expect("the entry aimed at the card");
            let dead = &state.objects[&card];
            assert!(
                dead.zone == Zone::Graveyard
                    && dead.controller == AI
                    && dead.card_types.core_types.contains(&CoreType::Creature),
                "reach guard: the AI's creature card"
            );
            assert!(
                entry.controller == P1
                    && chain_has_effect(entry, is_harmful)
                    && stack_entry_node_reach(state, entry)
                        .iter()
                        .any(|reach| reach.acted_on.contains(&TargetRef::Object(card))),
                "reach guard: P1's harmful {exiler} acts on the card"
            );
            let ability = &state.objects[&safekeeper].abilities[0];
            assert_eq!(
                self_protection_activation_payoff(state, AI, safekeeper, ability),
                exact_payoff,
                "reach guard: {exiler}"
            );
            // CR 109.2 + CR 400.7: Sylvan Safekeeper's "target creature you
            // control" is a creature permanent, and the card is a new object,
            // so shroud cannot stop the exile.
            assert!(
                !any_land_sacrifice_protection_payoff(state, AI, ability),
                "{exiler}"
            );
            assert_eq!(
                verdict_kinds(state, safekeeper)[0],
                (
                    VerdictVariant::Reject,
                    "sacrifice_land_protection_no_payoff"
                ),
                "{exiler}"
            );
        }
    }
}
