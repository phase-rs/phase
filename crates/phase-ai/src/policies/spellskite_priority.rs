//! Spellskite redirect tactical policy.
//!
//! Issue #1990: with Spellskite on the battlefield the AI repeatedly pays
//! {U/P} (often 2 life) to activate "change a target … to this creature" when
//! there is no hostile spell worth redirecting, or when the only relevant spells
//! already target Spellskite (a no-op re-activation).
//!
//! Mirrors `equipment_priority`: hard-reject pointless activations; leave real
//! redirects neutral for other policies to score.

use engine::game::effects::change_targets::legal_new_targets_for_stack_entry;
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::effect_classify::{effect_polarity, EffectPolarity};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

fn reject(kind: &'static str) -> PolicyVerdict {
    PolicyVerdict::Reject {
        reason: PolicyReason::new(kind),
    }
}

fn score(delta: f64, kind: &'static str) -> PolicyVerdict {
    PolicyVerdict::Score {
        delta,
        reason: PolicyReason::new(kind),
    }
}

/// Spellskite-shaped: change a target of a spell/ability to this permanent.
fn is_forced_self_retarget(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeTargets {
            forced_to: Some(TargetFilter::SelfRef),
            ..
        }
    )
}

/// Opponent stack entry with a harmful effect on the AI (player or permanent)
/// that does not already target Spellskite.
fn stack_has_redirectable_threat(
    state: &GameState,
    ai_player: PlayerId,
    spellskite_id: ObjectId,
) -> bool {
    let spellskite_target = TargetRef::Object(spellskite_id);
    state.stack.iter().enumerate().any(|(entry_index, entry)| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        let harms_ai = engine::game::effects::stack_reach::stack_entry_node_reach(state, entry)
            .iter()
            .any(|reach| {
                matches!(effect_polarity(&reach.node.effect), EffectPolarity::Harmful)
                    && reach.acted_on.iter().any(|t| match t {
                        TargetRef::Player(pid) => *pid == ai_player,
                        TargetRef::Object(obj_id) => state.objects.get(obj_id).is_some_and(|o| {
                            o.controller == ai_player
                                // CR 702.12b: an indestructible permanent can't
                                // be destroyed.
                                && !(matches!(reach.node.effect, Effect::Destroy { .. })
                                    && o.has_keyword(&Keyword::Indestructible))
                        }),
                    })
            });
        if !harms_ai {
            return false;
        }
        if !legal_new_targets_for_stack_entry(state, entry_index).contains(&spellskite_target) {
            return false;
        }
        let already_on_spellskite = ability
            .targets
            .iter()
            .any(|t| matches!(t, TargetRef::Object(id) if *id == spellskite_id));
        !already_on_spellskite
    })
}

pub struct SpellskitePriorityPolicy;

impl TacticalPolicy for SpellskitePriorityPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SpellskitePriority
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
        // activation-constant: forced-self ChangeTargets guard, universal.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || score(0.0, "spellskite_priority_na");

        let GameAction::ActivateAbility { source_id, .. } = &ctx.candidate.action else {
            return na();
        };

        if !ctx.effects().iter().any(|e| is_forced_self_retarget(e)) {
            return na();
        }

        if stack_has_redirectable_threat(ctx.state, ctx.ai_player, *source_id) {
            score(0.0, "spellskite_redirect_available")
        } else {
            reject("spellskite_no_redirectable_threat")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, ResolvedAbility, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{GameState, StackEntry, StackEntryKind};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn spellskite(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Spellskite".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.core_types.push(CoreType::Creature);
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChangeTargets {
                target: TargetFilter::Any,
                scope: engine::types::game_state::RetargetScope::Single,
                forced_to: Some(TargetFilter::SelfRef),
            },
        ));
        id
    }

    fn ai_creature(state: &mut GameState) -> ObjectId {
        let id = create_object(state, CardId(2), AI, "Bear".to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.toughness = Some(2);
        id
    }

    fn push_opp_destroy_spell(state: &mut GameState, target: ObjectId) {
        let effect = Effect::Destroy {
            target: TargetFilter::Any,
            cant_regenerate: false,
        };
        let ability =
            ResolvedAbility::new(effect, vec![TargetRef::Object(target)], ObjectId(900), OPP);
        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        state.stack.push_back(StackEntry {
            id: entry_id,
            source_id: ObjectId(900),
            controller: OPP,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ability)),
                card_id: CardId(900),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    fn push_opp_nonartifact_creature_spell(state: &mut GameState, target: ObjectId) {
        let effect = Effect::Destroy {
            target: TargetFilter::Typed(
                TypedFilter::creature().with_type(TypeFilter::Non(Box::new(TypeFilter::Artifact))),
            ),
            cant_regenerate: false,
        };
        push_opp_stack_entry(state, effect, vec![TargetRef::Object(target)]);
    }

    fn push_opp_player_life_loss(state: &mut GameState) {
        let effect = Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 3 },
            target: Some(TargetFilter::Player),
        };
        push_opp_stack_entry(state, effect, vec![TargetRef::Player(AI)]);
    }

    fn push_opp_stack_entry(state: &mut GameState, effect: Effect, targets: Vec<TargetRef>) {
        let ability = ResolvedAbility::new(effect, targets, ObjectId(900), OPP);
        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        state.stack.push_back(StackEntry {
            id: entry_id,
            source_id: ObjectId(900),
            controller: OPP,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ability)),
                card_id: CardId(900),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    fn policy_verdict(state: &GameState, spellskite_id: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id: spellskite_id,
                ability_index: 0,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
        };
        let decision = AiDecisionContext {
            waiting_for: engine::types::game_state::WaitingFor::Priority { player: AI },
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
        SpellskitePriorityPolicy.verdict(&ctx)
    }

    fn assert_reject(verdict: PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, kind),
            PolicyVerdict::Score { .. } => panic!("expected reject: {kind}"),
        }
    }

    fn assert_score(verdict: PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, kind);
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("expected score: {kind}"),
        }
    }

    #[test]
    fn reject_when_empty_stack() {
        let mut state = GameState::new_two_player(42);
        let sk = spellskite(&mut state);
        assert_reject(
            policy_verdict(&state, sk),
            "spellskite_no_redirectable_threat",
        );
    }

    #[test]
    fn reject_when_harmful_spell_already_targets_spellskite() {
        let mut state = GameState::new_two_player(42);
        let sk = spellskite(&mut state);
        push_opp_destroy_spell(&mut state, sk);
        assert_reject(
            policy_verdict(&state, sk),
            "spellskite_no_redirectable_threat",
        );
    }

    #[test]
    fn neutral_when_harmful_spell_targets_ai_creature() {
        let mut state = GameState::new_two_player(42);
        let sk = spellskite(&mut state);
        let bear = ai_creature(&mut state);
        push_opp_destroy_spell(&mut state, bear);
        assert_score(policy_verdict(&state, sk), "spellskite_redirect_available");
    }

    #[test]
    fn reject_when_harmful_player_spell_cannot_target_spellskite() {
        let mut state = GameState::new_two_player(42);
        let sk = spellskite(&mut state);
        push_opp_player_life_loss(&mut state);
        assert_reject(
            policy_verdict(&state, sk),
            "spellskite_no_redirectable_threat",
        );
    }

    #[test]
    fn reject_when_harmful_spell_cannot_legally_target_spellskite() {
        let mut state = GameState::new_two_player(42);
        let sk = spellskite(&mut state);
        let bear = ai_creature(&mut state);
        push_opp_nonartifact_creature_spell(&mut state, bear);
        assert_reject(
            policy_verdict(&state, sk),
            "spellskite_no_redirectable_threat",
        );
    }

    #[test]
    fn a_boon_on_an_ai_creature_is_not_a_redirectable_threat() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::game_state::WaitingFor;
        use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
        use engine::types::phase::Phase;
        const BOON_OF_EREBOS: &str =
            "Target creature gets +2/+0 until end of turn. Regenerate it. You lose 2 life.";
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let ai_creature = scenario.add_creature(P0, "Bear", 2, 2).id();
        let skite = scenario.add_creature(P0, "Spellskite", 0, 4).id();
        let boon = scenario
            .add_spell_to_hand_from_oracle(P1, "Boon of Erebos", true, BOON_OF_EREBOS)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            })
            .id();
        scenario.add_basic_land(P1, ManaColor::Black);
        let mut runner = scenario.build();
        {
            let state = runner.state_mut();
            state.active_player = P1;
            state.priority_player = P1;
            state.waiting_for = WaitingFor::Priority { player: P1 };
        }
        runner.cast(boon).target_object(ai_creature).commit();
        let state = runner.state();
        assert_eq!(
            state
                .stack
                .back()
                .and_then(|e| e.ability())
                .map(|root| root.targets.clone()),
            Some(vec![TargetRef::Object(ai_creature)]),
            "reach guard: the Boon targets the AI's creature"
        );
        assert!(
            legal_new_targets_for_stack_entry(state, state.stack.len() - 1)
                .contains(&TargetRef::Object(skite)),
            "reach guard: Spellskite is a legal new target"
        );
        assert!(!stack_has_redirectable_threat(state, P0, skite));
    }

    #[test]
    fn a_destroy_aimed_at_an_indestructible_creature_is_not_a_redirectable_threat() {
        use engine::game::scenario::{GameScenario, P0, P1};
        use engine::types::game_state::WaitingFor;
        use engine::types::keywords::Keyword;
        use engine::types::mana::{ManaColor, ManaCost};
        use engine::types::phase::Phase;
        const MURDER: &str = "Destroy target creature.";
        for indestructible in [false, true] {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let ai_creature = {
                let mut builder = scenario.add_creature(P0, "Ai", 2, 2);
                if indestructible {
                    builder.with_keyword(Keyword::Indestructible);
                }
                builder.id()
            };
            let skite = scenario.add_creature(P0, "Spellskite", 0, 4).id();
            let murder = scenario
                .add_spell_to_hand_from_oracle(P1, "Murder", true, MURDER)
                .with_mana_cost(ManaCost::generic(1))
                .id();
            scenario.add_basic_land(P1, ManaColor::Black);
            let mut runner = scenario.build();
            {
                let state = runner.state_mut();
                state.active_player = P1;
                state.priority_player = P1;
                state.waiting_for = WaitingFor::Priority { player: P1 };
            }
            runner.cast(murder).target_object(ai_creature).commit();
            let state = runner.state();
            assert!(
                legal_new_targets_for_stack_entry(state, state.stack.len() - 1)
                    .contains(&TargetRef::Object(skite)),
                "reach guard: Spellskite is a legal new target"
            );
            assert_eq!(
                stack_has_redirectable_threat(state, P0, skite),
                !indestructible
            );
        }
    }
}
