//! Free outlet activation policy.
//!
//! Scores free sacrifice-outlet activations (no mana cost) based on whether
//! a death-trigger payoff is currently on the AI player's battlefield.
//!
//! CR 603.6c: leaves-the-battlefield dies triggers fire when a creature moves
//! from battlefield to graveyard — the moment of sacrifice. CR 603.10a: some
//! zone-change triggers look back in time; the trigger checks the last known
//! information of the creature. CR 701.21: sacrifice is the keyword action that
//! moves the permanent to the graveyard. CR 701.21a: a sacrificed permanent
//! moves directly into its owner's graveyard — sacrifice is not destruction
//! and bypasses regenerate / indestructible.

use engine::game::game_object::GameObject;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::effect_classify::{effect_polarity, EffectPolarity};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::sacrifice_cost;
use crate::features::aristocrats::{ability_is_sacrifice_outlet, is_free_outlet_ability};
use crate::features::DeckFeatures;

/// Commitment threshold separating aristocrats path from non-aristocrats path.
const COMMITMENT_FLOOR: f32 = 0.1;
/// Bonus when at least one death-trigger payoff is on the battlefield.
/// CR 603.6c: payoffs fire immediately when the creature dies.
const DELTA_WITH_PAYOFF: f64 = 2.5;
/// Penalty when no payoff is on board — cracking a free outlet wastes a
/// creature without generating value. CR 701.21.
const DELTA_NO_PAYOFF: f64 = -1.5;
/// Penalty for non-aristocrats decks sacrificing without clear effect value.
const DELTA_NO_SYNERGY_PENALTY: f64 = -5.0;
/// Sacrifice cost threshold: non-aristocrats decks should never sacrifice a
/// permanent worth more than this.
const MAX_NON_ARISTOCRATS_SAC_COST: f64 = 4.0;

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
        if features.aristocrats.commitment >= COMMITMENT_FLOOR {
            Some(features.aristocrats.commitment)
        } else {
            // activation-constant: universal sac-outlet guidance for non-aristocrats decks.
            Some(1.0)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Gate: only free sacrifice-outlet activations are in scope.
        // CR 701.21: the cost must sacrifice a creature (not a land/artifact).
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

        if !ability_is_sacrifice_outlet(ability) {
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

        // Aristocrats path: deck has sacrifice synergy — evaluate payoff presence.
        if features.aristocrats.commitment >= COMMITMENT_FLOOR && is_free_outlet_ability(ability) {
            let death_triggers_on_board = count_death_triggers_on_board(
                ctx.state,
                ctx.ai_player,
                &features.aristocrats.death_trigger_names,
            );

            return if death_triggers_on_board > 0 {
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
            };
        }

        // Non-aristocrats path: no sacrifice synergy — only allow if the
        // effect itself justifies the sacrifice cost.
        let cheapest_sac_cost = cheapest_sacrificeable_cost(ctx);
        if cheapest_sac_cost > MAX_NON_ARISTOCRATS_SAC_COST {
            return PolicyVerdict::Reject {
                reason: PolicyReason::new("sac_outlet_too_expensive"),
            };
        }

        let polarity = effect_polarity(&ability.effect);
        match polarity {
            EffectPolarity::Harmful | EffectPolarity::Beneficial => PolicyVerdict::Score {
                delta: 0.5,
                reason: PolicyReason::new("sac_outlet_effect_justified"),
            },
            EffectPolarity::Contextual => PolicyVerdict::Score {
                delta: DELTA_NO_SYNERGY_PENALTY,
                reason: PolicyReason::new("sac_outlet_no_synergy"),
            },
        }
    }
}

/// Cheapest sacrifice cost among AI-controlled creatures on the battlefield.
fn cheapest_sacrificeable_cost(ctx: &PolicyContext<'_>) -> f64 {
    ctx.state
        .battlefield
        .iter()
        .filter_map(|&id| {
            let obj = ctx.state.objects.get(&id)?;
            (obj.controller == ctx.ai_player
                && obj
                    .card_types
                    .core_types
                    .contains(&engine::types::card_type::CoreType::Creature))
            .then(|| sacrifice_cost(ctx.state, id, ctx.penalties()))
        })
        .fold(f64::INFINITY, f64::min)
}

/// Count AI-controlled death-trigger payoff objects currently on the battlefield.
/// Uses `death_trigger_names` as an identity-lookup list — the structural
/// classification already happened at deck-build time in `aristocrats::detect`.
fn count_death_triggers_on_board(
    state: &GameState,
    player: PlayerId,
    death_trigger_names: &[String],
) -> usize {
    if death_trigger_names.is_empty() {
        return 0;
    }
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj: &&GameObject| obj.controller == player && obj.zone == Zone::Battlefield)
        .filter(|obj| death_trigger_names.iter().any(|name| name == &obj.name))
        .count()
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
        TargetFilter, TypedFilter,
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
            },
        );
        ability.cost = Some(AbilityCost::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            count: 1,
        });
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
                AbilityCost::Sacrifice {
                    target: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                    count: 1,
                },
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
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        }
    }

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    #[test]
    fn activates_universally() {
        let state = GameState::new_two_player(42);
        // Non-aristocrats: returns Some(1.0)
        let features = DeckFeatures::default();
        assert_eq!(
            FreeOutletActivationPolicy.activation(&features, &state, AI),
            Some(1.0)
        );
        // Aristocrats: returns commitment value
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
    fn non_free_outlet_rejects_without_cheap_sacrifice() {
        // A non-free sac outlet (has mana cost) with no creatures on board to
        // sacrifice cheaply → Reject via the non-aristocrats path.
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
        };

        let verdict = FreeOutletActivationPolicy.verdict(&ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Reject { .. }),
            "expected Reject when no cheap sacrificeable creature exists"
        );
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
