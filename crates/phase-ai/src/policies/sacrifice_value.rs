use engine::types::actions::GameAction;
use engine::types::game_state::{CostResume, GameState, PayCostKind, WaitingFor};
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::sacrifice_cost;

pub struct SacrificeValuePolicy;

impl SacrificeValuePolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        // Guard: only score SelectCards during sacrifice decisions
        let GameAction::SelectCards { cards } = &ctx.candidate.action else {
            return 0.0;
        };
        if !matches!(
            ctx.decision.waiting_for,
            WaitingFor::PayCost {
                kind: PayCostKind::Sacrifice,
                resume: CostResume::Spell { .. } | CostResume::SpellCost { .. },
                ..
            } | WaitingFor::WardSacrificeChoice { .. }
                | WaitingFor::EffectZoneChoice {
                    effect_kind: engine::types::ability::EffectKind::Sacrifice,
                    ..
                }
        ) {
            return 0.0;
        }

        // Score inversely to value: cheap sacrifices produce less negative scores
        let total_cost: f64 = cards
            .iter()
            .map(|&obj_id| sacrifice_cost(ctx.state, obj_id, ctx.penalties()))
            .sum();
        -total_cost
    }
}

impl TacticalPolicy for SacrificeValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SacrificeValue
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
        // Sacrifice resource valuation is intrinsic to the permanent being given
        // up — a 6/6 costs the same to sacrifice on turn 2 as on turn 9 — so it
        // must not scale with game phase. Mirrors the sibling
        // PaymentSelectionPolicy, which handles the same SelectCards / PayCost
        // decision with a constant 1.0 activation. A turn-phase multiplier (>1.0)
        // here could push a legitimate critical-band score past the registry's
        // CRITICAL_MAX ceiling (see issue #4282).
        // activation-constant: phase-independent sacrifice resource valuation.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Route through the band contract helper rather than hand-building a
        // raw `Score`: `self.score()` is an unbounded sum of per-card sacrifice
        // costs, and `PolicyVerdict::score` clamps its magnitude into the
        // declared bands (|delta| <= CRITICAL_MAX). With activation pinned to
        // 1.0 above, the scaled delta can never exceed the critical ceiling.
        PolicyVerdict::score(self.score(ctx), PolicyReason::new("sacrifice_value_score"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{GameState, PendingCast};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn dummy_pending() -> Box<PendingCast> {
        Box::new(PendingCast::new(
            ObjectId(100),
            CardId(100),
            ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 0 },
                    target: engine::types::ability::TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(100),
                PlayerId(0),
            ),
            ManaCost::zero(),
        ))
    }

    #[test]
    fn prefers_sacrificing_token_over_creature() {
        let mut state = GameState::new_two_player(42);

        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&creature).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(3);

        let token_card_id = CardId(state.next_object_id);
        let token = create_object(
            &mut state,
            token_card_id,
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&token).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.is_token = true;

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: vec![creature, token],
                count: 1,
                min_count: 1,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };

        // Score sacrificing the creature
        let creature_candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![creature],
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };
        let creature_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &creature_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let creature_score = SacrificeValuePolicy.score(&creature_ctx);

        // Score sacrificing the token
        let token_candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![token] },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };
        let token_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &token_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let token_score = SacrificeValuePolicy.score(&token_ctx);

        assert!(
            token_score > creature_score,
            "Should prefer sacrificing token ({token_score}) over creature ({creature_score})"
        );
    }

    /// Regression for #4282: sacrificing a high-value creature must not produce
    /// a scaled delta beyond the critical band ceiling. Before the fix, `verdict`
    /// returned the raw unbounded `-evaluate_creature` score and `activation`
    /// scaled it by `turn_phase_mult` (up to 1.3), so a single large creature
    /// tripped the registry's `debug_assert!(scaled_delta.abs() <= CRITICAL_MAX)`.
    #[test]
    fn large_sacrifice_stays_within_critical_band() {
        use super::super::registry::CRITICAL_MAX;

        let mut state = GameState::new_two_player(42);

        // 8/8 => evaluate_creature = 8*1.5 + 8 = 20.0, comfortably over the
        // critical ceiling of 15, so the band clamp must actually engage.
        let big = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Colossus".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&big).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(8);
        obj.toughness = Some(8);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: vec![big],
                count: 1,
                min_count: 1,
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![big] },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
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

        // The raw score must exceed the ceiling, proving the clamp is exercised.
        assert!(
            SacrificeValuePolicy.score(&ctx).abs() > CRITICAL_MAX,
            "test premise: raw sacrifice score should exceed the critical ceiling"
        );

        // The banded verdict must clamp magnitude into the critical band.
        let PolicyVerdict::Score { delta, .. } = SacrificeValuePolicy.verdict(&ctx) else {
            panic!("sacrifice value policy must return a Score verdict");
        };
        assert!(
            delta.abs() <= CRITICAL_MAX,
            "verdict delta {delta} must be clamped to the critical band ceiling {CRITICAL_MAX}"
        );

        // Activation is the constant 1.0, so the scaled delta the registry
        // asserts on equals the (already clamped) verdict delta — never above
        // the ceiling regardless of turn number.
        let activation = SacrificeValuePolicy
            .activation(&DeckFeatures::default(), &state, PlayerId(0))
            .expect("sacrifice value policy always activates");
        assert_eq!(
            activation, 1.0,
            "sacrifice valuation must not scale by phase"
        );
        assert!((delta * f64::from(activation)).abs() <= CRITICAL_MAX);
    }

    #[test]
    fn no_score_outside_sacrifice_context() {
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![ObjectId(1)],
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
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

        let score = SacrificeValuePolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No score outside sacrifice, got {score}"
        );
    }
}
