//! Condition-gated activation tactical policy.
//!
//! Report (Discord #ai-suggestions, "Mosswort Bridge"): the AI activates a
//! hideaway land's payoff ability (`{G}, {T}: You may play the exiled card
//! without paying its mana cost if creatures you control have total power 10 or
//! greater`) every turn even when it has far less than 10 power, burning mana
//! and the tap for an effect that does nothing.
//!
//! The engine is rules-correct: per CR 602.5 (and the Shelldock Isle ruling) the
//! hideaway payoff ability is *legal* to activate under the threshold — only the
//! `CastFromZone` effect is gated, and it correctly does nothing at resolution
//! (CR 608.2c) when the condition is false. So this is purely an AI-value miss:
//! paying a cost for an activation whose entire payoff is currently gated off.
//!
//! This penalizes activating any ability whose top-level intervening-if
//! `condition` evaluates false right now, so `PassPriority` (or a better line)
//! wins. It generalizes across the whole class of "Cost: do X if [board
//! condition]" activated abilities — every hideaway land (Shelldock Isle,
//! Windbrisk Heights, Spinerock Knoll, Howltooth Hollow, Mosswort Bridge) and
//! beyond — not a single card.
//!
//! Condition evaluation is delegated to the engine
//! (`ability_condition_currently_met`), which only judges board/controller-
//! relative conditions and returns `None` for anything that needs resolution-time
//! context (chosen targets, the cast/trigger event). A soft penalty (not a hard
//! `Reject`) is used because an ability's *cost* can change the very thing its
//! condition checks (e.g. a sacrifice cost that makes "if you control no
//! creatures" true at resolution); the condition here is read pre-cost, so the
//! AI may still take such a line if strongly favored.

use engine::game::casting::ability_condition_currently_met;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Penalty for activating an ability whose entire payoff is gated behind a
/// currently-false condition. Modest — enough to lose to `PassPriority` when
/// nothing else pushes the activation (mirrors `EquipmentPriority`'s
/// no-better-home penalty), never an absolute veto.
const CONDITION_UNMET_PENALTY: f64 = 2.5;

pub struct ConditionGatedActivationPolicy;

impl TacticalPolicy for ConditionGatedActivationPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ConditionGatedActivation
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
        // Applies to every deck; the verdict's condition guard self-gates.
        // activation-constant: conditional-payoff activation check, universal.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let score = |delta: f64, kind: &'static str| PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new(kind),
        };

        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return score(0.0, "condition_gated_na");
        };

        // Only matters when activating costs something — paying a cost for a
        // gated-off payoff is the waste being avoided. Free activations are
        // harmless even when the condition is unmet.
        let has_cost = ctx
            .state
            .objects
            .get(source_id)
            .and_then(|obj| obj.abilities.get(*ability_index))
            .is_some_and(|def| def.cost.is_some());
        if !has_cost {
            return score(0.0, "condition_gated_ok");
        }

        // CR 608.2c: delegate the intervening-if evaluation to the engine. Only
        // `Some(false)` — the payoff is provably gated off right now — is penalized.
        match ability_condition_currently_met(ctx.state, *source_id, *ability_index) {
            Some(false) => score(-CONDITION_UNMET_PENALTY, "condition_gated_payoff_unmet"),
            _ => score(0.0, "condition_gated_ok"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AggregateFunction,
        Comparator, ControllerRef, Effect, ObjectProperty, QuantityExpr, QuantityRef, TargetFilter,
        TypeFilter, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaCost, ManaCostShard};
    use engine::types::zones::Zone;
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;

    const AI: PlayerId = PlayerId(0);

    /// "Sum of power of creatures you control" — Mosswort's condition operand.
    fn your_creatures_power_ge(threshold: i32) -> AbilityCondition {
        AbilityCondition::QuantityCheck {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Aggregate {
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                    filter: TargetFilter::Typed(
                        TypedFilter::default()
                            .with_type(TypeFilter::Creature)
                            .controller(ControllerRef::You),
                    ),
                },
            },
            comparator: Comparator::GE,
            rhs: QuantityExpr::Fixed { value: threshold },
        }
    }

    /// A permanent with one activated ability: `{G}, {T}: <effect>` gated by an
    /// optional `condition`. Models Mosswort's hideaway payoff shape.
    fn source_with_gated_ability(
        state: &mut GameState,
        condition: Option<AbilityCondition>,
        with_cost: bool,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Hideaway".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        // The effect kind is irrelevant — the policy keys off cost + condition.
        let mut def = AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate);
        if with_cost {
            def.cost = Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Green],
                    generic: 0,
                },
            });
        }
        def.condition = condition;
        Arc::make_mut(&mut obj.abilities).push(def);
        id
    }

    /// A vanilla creature with the given power (to drive the Sum-power condition).
    fn creature(state: &mut GameState, power: i32) -> ObjectId {
        let id = create_object(state, CardId(2), AI, "Bear".to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(power);
        obj.power = Some(power);
        obj.base_toughness = Some(power);
        obj.toughness = Some(power);
        id
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
        ConditionGatedActivationPolicy.verdict(&ctx)
    }

    fn assert_score(verdict: PolicyVerdict, kind: &str, delta: f64) {
        match verdict {
            PolicyVerdict::Score { delta: d, reason } => {
                assert_eq!(reason.kind, kind, "reason kind");
                assert_eq!(d, delta, "delta");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    #[test]
    fn gated_activation_penalized_when_condition_unmet() {
        let mut state = GameState::new_two_player(42);
        // Two 3-power creatures → total power 6 < 10 (a real, non-vacuous <10).
        creature(&mut state, 3);
        creature(&mut state, 3);
        let source = source_with_gated_ability(&mut state, Some(your_creatures_power_ge(10)), true);
        assert_score(
            activate_verdict(&state, source),
            "condition_gated_payoff_unmet",
            -CONDITION_UNMET_PENALTY,
        );
    }

    #[test]
    fn gated_activation_ok_when_condition_met() {
        let mut state = GameState::new_two_player(42);
        // Two 6-power creatures → total power 12 >= 10.
        creature(&mut state, 6);
        creature(&mut state, 6);
        let source = source_with_gated_ability(&mut state, Some(your_creatures_power_ge(10)), true);
        assert_score(activate_verdict(&state, source), "condition_gated_ok", 0.0);
    }

    #[test]
    fn unconditional_activation_na() {
        let mut state = GameState::new_two_player(42);
        let source = source_with_gated_ability(&mut state, None, true);
        assert_score(activate_verdict(&state, source), "condition_gated_ok", 0.0);
    }

    /// A free (no-cost) gated ability is not penalized — there's no wasted cost.
    #[test]
    fn free_gated_activation_not_penalized() {
        let mut state = GameState::new_two_player(42);
        creature(&mut state, 3);
        let source =
            source_with_gated_ability(&mut state, Some(your_creatures_power_ge(10)), false);
        assert_score(activate_verdict(&state, source), "condition_gated_ok", 0.0);
    }

    #[test]
    fn non_activate_decision_na() {
        let state = GameState::new_two_player(42);
        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Pass,
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
        assert_score(
            ConditionGatedActivationPolicy.verdict(&ctx),
            "condition_gated_na",
            0.0,
        );
    }
}
