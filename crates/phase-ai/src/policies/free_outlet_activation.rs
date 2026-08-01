//! Free outlet activation policy.
//!
//! Scores **free** sacrifice-outlet activations (no mana cost) for
//! aristocrats-committed decks, based on whether a death-trigger payoff is
//! currently on the AI player's battlefield. That payoff-presence signal is the
//! one question this policy can uniquely answer, and it is the only question it
//! answers.
//!
//! **This policy holds no cost-vs-benefit authority.** Whether a self-cost
//! activation's payoff is worth the resource it spends — for every sacrifice
//! outlet, mana-costed ones included — is `SelfCostValuePolicy`'s
//! (`self_cost_value.rs`) alone, priced through `self_cost::appraise_benefit`
//! against `self_cost::real_self_cost`.
//!
//! This policy used to carry a second, cruder answer to that same question: a
//! non-aristocrats branch that hard-`Reject`ed whenever the cheapest
//! AI-controlled *creature* cost more than a flat 4.0, and otherwise rewarded
//! the activation +0.5 on the top-level effect's polarity alone, reading
//! neither the payoff's magnitude nor the rest of the chain. Its cost binding
//! was creature-only and blind to the ability's actual sacrifice filter, so it
//! disagreed with the filter-faithful binding one policy away. It was also
//! wrong on its own terms: `cheapest_sacrificeable_cost` folded over
//! AI-controlled creatures, so a SelfRef outlet that sacrifices *itself* (the
//! Clue / Tyrite Sanctum class) was hard-`Reject`ed whenever the AI controlled
//! no creature at all — the fold's `INFINITY` identity exceeding the
//! threshold — and, when the AI did control one, was granted its +0.5 gated on
//! the price of a wholly unrelated creature. Both branches are gone; one
//! authority prices the trade.
//!
//! CR 603.6c: leaves-the-battlefield dies triggers fire when a creature moves
//! from battlefield to graveyard — the moment of sacrifice. CR 603.10a: some
//! zone-change triggers look back in time; the trigger checks the last known
//! information of the creature. CR 701.21: sacrifice is the keyword action that
//! moves the permanent to the graveyard. CR 701.21a: a sacrificed permanent
//! moves directly into its owner's graveyard — sacrifice is not destruction
//! and bypasses regenerate / indestructible.

use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::self_cost::count_death_triggers_on_board;
use crate::features::aristocrats::is_free_outlet_ability;
use crate::features::DeckFeatures;

/// Deck-commitment floor below which this policy opts out entirely.
const COMMITMENT_FLOOR: f32 = 0.1;
/// Bonus when at least one death-trigger payoff is on the battlefield.
/// CR 603.6c: payoffs fire immediately when the creature dies.
const DELTA_WITH_PAYOFF: f64 = 2.5;
/// Penalty when no payoff is on board — cracking a free outlet wastes a
/// creature without generating value. CR 701.21.
const DELTA_NO_PAYOFF: f64 = -1.5;

pub struct FreeOutletActivationPolicy;

impl TacticalPolicy for FreeOutletActivationPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::FreeOutletActivation
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Classifier-gated, the house pattern (`combo_line.rs`, active only for
        // cEDH decks): `None` opts the policy out and the registry skips it
        // before `verdict` is ever called. A deck with no aristocrats
        // commitment has no death-trigger payoff for this policy to weigh, and
        // the cost side is `SelfCostValuePolicy`'s — so there is nothing left
        // to say.
        (features.aristocrats.commitment >= COMMITMENT_FLOOR)
            .then_some(features.aristocrats.commitment)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Gate: only FREE sacrifice-outlet activations are in scope.
        // CR 701.21: the cost must sacrifice a creature (not a land/artifact).
        // A mana-costed outlet leaves this policy's scope entirely and is
        // governed by `SelfCostValuePolicy` alone.
        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("free_outlet_activation_na"),
            };
        };

        let Some(object) = ctx.state.objects.get(source_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("free_outlet_activation_na"),
            };
        };

        let Some(ability) = object.abilities.get(*ability_index) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("free_outlet_activation_na"),
            };
        };

        // `is_free_outlet_ability` subsumes `ability_is_sacrifice_outlet` (it
        // calls it first and short-circuits), so this one predicate is the whole
        // scope gate.
        if !is_free_outlet_ability(ability) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("free_outlet_activation_na"),
            };
        }

        let features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();

        // Commitment is guaranteed by `activation` — the registry is the sole
        // production caller and skips this policy below the floor — and
        // free-ness by the gate above. What remains is the payoff question.
        let death_triggers_on_board = count_death_triggers_on_board(
            ctx.state,
            ctx.ai_player,
            &features.aristocrats.death_trigger_names,
        );

        if death_triggers_on_board > 0 {
            PolicyVerdict::Score {
                delta: DELTA_WITH_PAYOFF,
                reason: PolicyReason::new("free_outlet_activate_with_payoff")
                    .with_fact("death_triggers_on_board", death_triggers_on_board as i64),
            }
        } else {
            PolicyVerdict::Score {
                delta: DELTA_NO_PAYOFF,
                reason: PolicyReason::new("free_outlet_no_payoff_on_board"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::aristocrats::AristocratsFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr,
        SacrificeCost, TargetFilter, TypedFilter,
    };
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn make_free_outlet_ability() -> AbilityDefinition {
        // Sac-only outlet (Goblin Bombardment shape): no mana cost.
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Any,
                damage_source: None,
                excess: None,
            },
        );
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            1,
        )));
        ability
    }

    fn make_mana_outlet_ability() -> AbilityDefinition {
        // Non-free outlet: has mana cost.
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: engine::types::mana::ManaCost::generic(2),
                },
                AbilityCost::Sacrifice(engine::types::ability::SacrificeCost::count(
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                    1,
                )),
            ],
        });
        ability
    }

    fn make_mana_tap_ability() -> AbilityDefinition {
        // Non-outlet mana ability (Forest shape).
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: engine::types::ability::ManaProduction::Fixed {
                    colors: Vec::new(),
                    contribution: engine::types::ability::ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability
    }

    fn context_with_aristocrats(
        commitment: f32,
        outlet_names: Vec<String>,
        death_trigger_names: Vec<String>,
    ) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        let features = DeckFeatures {
            aristocrats: AristocratsFeature {
                outlet_count: outlet_names.len() as u32,
                free_outlet_count: outlet_names.len() as u32,
                death_trigger_count: death_trigger_names.len() as u32,
                fodder_source_count: 1,
                commitment,
                outlet_names,
                death_trigger_names,
            },
            ..DeckFeatures::default()
        };
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
    }

    fn activate_candidate(source_id: ObjectId, ability_index: usize) -> CandidateAction {
        CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
        }
    }

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    #[test]
    fn activation_gates_on_aristocrats_commitment() {
        // The policy opts OUT below the commitment floor: the registry then
        // skips it entirely and pushes no verdict entry at all. Before the
        // de-duplication this returned `Some(1.0)` — "universal sac-outlet
        // guidance" — which is what fed the crude cost gate that no longer
        // exists. Reverting that activation makes the first assertion red.
        let state = GameState::new_two_player(42);
        let features = DeckFeatures::default();
        assert_eq!(
            FreeOutletActivationPolicy.activation(&features, &state, AI),
            None,
            "a non-aristocrats deck must opt this policy out entirely"
        );
        // Above the floor: unchanged — activation is the commitment value.
        let features = DeckFeatures {
            aristocrats: AristocratsFeature {
                commitment: 0.5,
                ..Default::default()
            },
            ..DeckFeatures::default()
        };
        assert_eq!(
            FreeOutletActivationPolicy.activation(&features, &state, AI),
            Some(0.5)
        );
    }

    #[test]
    fn bonus_with_payoff_on_board() {
        let mut state = GameState::new_two_player(42);
        // Add free outlet object to battlefield.
        let outlet_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Goblin Bombardment".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&outlet_id).unwrap().abilities)
            .push(make_free_outlet_ability());
        // Add death-trigger payoff to battlefield.
        let _payoff = create_object(
            &mut state,
            CardId(2),
            AI,
            "Zulaport Cutthroat".to_string(),
            Zone::Battlefield,
        );

        let candidate = activate_candidate(outlet_id, 0);
        let decision = decision();
        let (context, config) = context_with_aristocrats(
            0.9,
            vec!["Goblin Bombardment".to_string()],
            vec!["Zulaport Cutthroat".to_string()],
        );
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

        let verdict = FreeOutletActivationPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "free_outlet_activate_with_payoff");
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert!(reason
                    .facts
                    .iter()
                    .any(|(k, _)| *k == "death_triggers_on_board"));
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn penalty_without_payoff_on_board() {
        let mut state = GameState::new_two_player(42);
        let outlet_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Goblin Bombardment".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&outlet_id).unwrap().abilities)
            .push(make_free_outlet_ability());

        let candidate = activate_candidate(outlet_id, 0);
        let decision = decision();
        // death_trigger_names set but no matching object on battlefield.
        let (context, config) = context_with_aristocrats(
            0.9,
            vec!["Goblin Bombardment".to_string()],
            vec!["Zulaport Cutthroat".to_string()],
        );
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

        let verdict = FreeOutletActivationPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "free_outlet_no_payoff_on_board");
                assert!(delta < 0.0, "expected negative delta, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn mana_costed_outlet_is_out_of_scope() {
        // THE DE-DUPLICATION PIN. A mana-costed sac outlet (Baron Bertram
        // Graywater's whole class) with no cheap creature to sacrifice. This
        // exact fixture used to `Reject` here — "sac_outlet_too_expensive" —
        // which is the cost-vs-benefit authority this policy must no longer
        // hold. It now falls out of scope at the free-ness gate and returns a
        // delta-0 `na`; the trade is priced by `SelfCostValuePolicy` instead.
        //
        // Reachability is not assumed: reaching the old Reject required passing
        // `ability_is_sacrifice_outlet`, so this Composite demonstrably
        // satisfied the outlet predicate. Post-change it fails only the
        // free-ness leg (`cost_has_nonzero_mana`: generic(2) → mana_value > 0).
        //
        // A `Reject` or ANY non-zero delta here means the cost authority leaked
        // back into this policy.
        let mut state = GameState::new_two_player(42);
        let outlet_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Costly Outlet".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&outlet_id).unwrap().abilities)
            .push(make_mana_outlet_ability());

        let candidate = activate_candidate(outlet_id, 0);
        let decision = decision();
        let (context, config) = context_with_aristocrats(
            0.9,
            vec!["Costly Outlet".to_string()],
            vec!["Zulaport Cutthroat".to_string()],
        );
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

        let verdict = FreeOutletActivationPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "free_outlet_activation_na");
                assert_eq!(delta, 0.0, "this policy may hold no cost authority");
            }
            PolicyVerdict::Reject { reason } => {
                panic!("cost-vs-benefit leaked back in as Reject {}", reason.kind)
            }
        }
    }

    #[test]
    fn non_outlet_ability_yields_na() {
        // A mana-tap ability (Forest shape) — not a sac outlet at all.
        let mut state = GameState::new_two_player(42);
        let land_id = create_object(
            &mut state,
            CardId(1),
            AI,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&land_id).unwrap().abilities)
            .push(make_mana_tap_ability());

        let candidate = activate_candidate(land_id, 0);
        let decision = decision();
        let (context, config) = context_with_aristocrats(0.9, vec![], vec![]);
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

        let verdict = FreeOutletActivationPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "free_outlet_activation_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
