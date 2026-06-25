use engine::types::actions::GameAction;
use engine::types::game_state::{CostResume, GameState, PayCostKind, WaitingFor};
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;

use super::activation::turn_only;
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
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // `score` sums every sacrificed permanent's value into one delta, which
        // is unbounded (a costly multi-permanent sacrifice can far exceed the
        // critical band). Route it through the shared `PolicyVerdict::score`
        // constructor so the raw delta is clamped into the critical band like
        // every other policy, instead of emitting an out-of-band value the
        // registry would otherwise have to defend against (issue #4282).
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

    /// Regression for #4282: a high-cost sacrifice (the summed value of many
    /// permanents) produces a raw `score` well past the critical band, but
    /// `verdict` must clamp it into the band so the registry never receives an
    /// out-of-band delta — previously this surfaced as a recovered engine panic
    /// ("policy SacrificeValue scaled delta … exceeds critical band ceiling 15").
    #[test]
    fn high_cost_sacrifice_verdict_stays_within_critical_band() {
        use crate::policies::registry::CRITICAL_MAX;

        let mut state = GameState::new_two_player(42);

        // Eight beefy creatures whose combined sacrifice value far exceeds the
        // critical band ceiling, so the raw score is unambiguously out of band.
        let mut choices = Vec::new();
        for i in 0..8 {
            let id = create_object(
                &mut state,
                CardId(1 + i),
                PlayerId(0),
                format!("Behemoth {i}"),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(8);
            obj.toughness = Some(8);
            choices.push(id);
        }

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::PayCost {
                player: PlayerId(0),
                kind: PayCostKind::Sacrifice,
                choices: choices.clone(),
                count: choices.len(),
                min_count: choices.len(),
                resume: CostResume::Spell {
                    spell: dummy_pending(),
                },
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: choices.clone(),
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

        // The raw score is genuinely out of band (otherwise the regression would
        // not exercise the clamp at all).
        let raw = SacrificeValuePolicy.score(&ctx);
        assert!(
            raw < -CRITICAL_MAX,
            "scenario must drive the raw score past the critical band (got {raw})"
        );

        // The verdict clamps it into the band: a single policy never emits a
        // delta whose magnitude exceeds the critical-band ceiling.
        match SacrificeValuePolicy.verdict(&ctx) {
            PolicyVerdict::Score { delta, .. } => {
                assert!(
                    delta.abs() <= CRITICAL_MAX,
                    "verdict delta {delta} must be clamped into the critical band (±{CRITICAL_MAX})"
                );
                assert!(
                    delta < 0.0,
                    "sacrificing value remains a negative incentive"
                );
            }
            other => panic!("expected a Score verdict, got {other:?}"),
        }
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
