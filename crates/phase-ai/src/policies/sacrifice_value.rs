use engine::game::filter::{matches_target_filter, FilterContext};
use engine::types::ability::{Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::{CostResume, GameState, PayCostKind, WaitingFor};
use engine::types::player::PlayerId;

use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::sacrifice_cost;

pub struct SacrificeValuePolicy;

/// Policy scores are card-equivalent units, so drawing one card is worth 1.0.
const SINGLE_CARD_VALUE: f64 = 1.0;

impl SacrificeValuePolicy {
    fn optional_single_card_draw_sacrifices_only_source(ctx: &PolicyContext<'_>) -> Option<f64> {
        let GameAction::DecideOptionalEffect { accept: true } = &ctx.candidate.action else {
            return None;
        };
        let WaitingFor::OptionalEffectChoice { source_id, .. } = &ctx.decision.waiting_for else {
            return None;
        };
        let ability = ctx.state.active_optional_effect_frame()?.ability.as_ref();
        if ability.source_id != *source_id || ability.controller != ctx.ai_player {
            return None;
        }

        let Effect::Sacrifice {
            target,
            count: QuantityExpr::Fixed { value: 1 },
            ..
        } = &ability.effect
        else {
            return None;
        };
        // Explicit SelfRef sacrifices are designed to consume their source. This
        // guard is for filtered outlets whose source only happens to be the last
        // matching permanent, such as a Zombie engine with no other Zombies.
        if matches!(target, TargetFilter::SelfRef)
            || ability.else_ability.is_some()
            || ability.repeat_for.is_some()
            || ability.player_scope.is_some()
            || !single_card_draw_is_only_payoff(ability.sub_ability.as_deref())
        {
            return None;
        }

        let filter_ctx = FilterContext::from_ability(ability);
        let mut eligible = ctx.state.battlefield.iter().copied().filter(|id| {
            ctx.state.objects.get(id).is_some_and(|object| {
                object.controller == ability.controller
                    && !object.is_emblem
                    && matches_target_filter(ctx.state, *id, target, &filter_ctx)
            })
        });
        if eligible.next()? != *source_id || eligible.next().is_some() {
            return None;
        }

        let cost = sacrifice_cost(ctx.state, *source_id, ctx.penalties());
        (cost > SINGLE_CARD_VALUE).then_some(cost)
    }

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

fn single_card_draw_is_only_payoff(
    ability: Option<&engine::types::ability::ResolvedAbility>,
) -> bool {
    let Some(ability) = ability else {
        return false;
    };
    matches!(
        &ability.effect,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        }
    ) && ability.sub_ability.is_none()
        && ability.else_ability.is_none()
        && ability.repeat_for.is_none()
        && ability.player_scope.is_none()
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
        // VeryEasy and Easy do not search optional-effect continuations, so they
        // need a hard guard against accepting this immediately losing exchange.
        // Search-enabled difficulties must remain free to discover death-trigger,
        // recursion, and other continuation value that outweighs the source.
        let protected_source_cost = if ctx.config.search.enabled {
            None
        } else {
            Self::optional_single_card_draw_sacrifices_only_source(ctx)
        };
        if let Some(cost) = protected_source_cost {
            return PolicyVerdict::reject(
                PolicyReason::new("optional_sacrifice_only_source_for_single_card")
                    .with_fact("cost_milli", (cost * 1000.0) as i64),
            );
        }

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
    use crate::config::{create_config, AiConfig, AiDifficulty, Platform};
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TypedFilter};
    use engine::types::card_type::CoreType;
    use engine::types::game_state::{GameState, PendingCast};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

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

    fn optional_sacrifice_for_card_state() -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sacrifice Engine".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&source).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(3);
        object.toughness = Some(3);

        // Keep the accept branch engine-legal: drawing the payoff card must not
        // turn this policy regression into a draw-from-empty-library test.
        create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Drawn Card".to_string(),
            Zone::Library,
        );

        let mut sacrifice = ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Typed(TypedFilter::creature()),
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
            Vec::new(),
            source,
            PlayerId(0),
        );
        sacrifice.optional = true;
        sacrifice.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            source,
            PlayerId(0),
        )));
        state.push_optional_effect_frame(engine::types::OptionalEffectFrame {
            ability: Box::new(sacrifice),
            trigger_event: None,
            trigger_match_count: None,
        });
        state.waiting_for = WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: source,
            description: Some("You may sacrifice a creature. If you do, draw a card.".to_string()),
            may_trigger_key: None,
        };
        (state, source)
    }

    fn optional_verdict(state: &GameState, accept: bool, config: &AiConfig) -> PolicyVerdict {
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::DecideOptionalEffect { accept },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Utility),
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        SacrificeValuePolicy.verdict(&ctx)
    }

    #[test]
    fn easy_ai_declines_single_card_draw_when_only_filtered_sacrifice_is_source() {
        let (state, _) = optional_sacrifice_for_card_state();

        for difficulty in [AiDifficulty::VeryEasy, AiDifficulty::Easy] {
            let config = create_config(difficulty, Platform::Native).into_measurement(42);
            let mut rng = ChaCha20Rng::seed_from_u64(42);
            let action = crate::search::choose_action(&state, PlayerId(0), &config, &mut rng)
                .expect("the optional effect prompt has a legal decline action");
            assert_eq!(
                action,
                GameAction::DecideOptionalEffect { accept: false },
                "{difficulty:?} must preserve the filtered sacrifice source"
            );
        }
    }

    #[test]
    fn optional_source_preservation_guard_stands_down_with_another_sacrifice() {
        let (mut state, _) = optional_sacrifice_for_card_state();
        let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
        let fodder = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Fodder".to_string(),
            Zone::Battlefield,
        );
        let object = state.objects.get_mut(&fodder).unwrap();
        object.card_types.core_types.push(CoreType::Creature);
        object.power = Some(1);
        object.toughness = Some(1);

        assert!(matches!(
            optional_verdict(&state, true, &config),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn optional_source_preservation_guard_does_not_block_explicit_self_sacrifice() {
        let (mut state, _) = optional_sacrifice_for_card_state();
        let config = create_config(AiDifficulty::VeryEasy, Platform::Native);
        let ability = &mut state
            .active_optional_effect_frame_mut()
            .expect("fixture parks an optional-effect frame")
            .ability;
        let Effect::Sacrifice { target, .. } = &mut ability.effect else {
            panic!("fixture must contain a sacrifice effect");
        };
        *target = TargetFilter::SelfRef;

        assert!(matches!(
            optional_verdict(&state, true, &config),
            PolicyVerdict::Score { .. }
        ));
    }

    #[test]
    fn search_enabled_ai_can_evaluate_single_source_sacrifice() {
        let (state, _) = optional_sacrifice_for_card_state();

        for difficulty in [
            AiDifficulty::Medium,
            AiDifficulty::Hard,
            AiDifficulty::VeryHard,
            AiDifficulty::CEDH,
        ] {
            let config = create_config(difficulty, Platform::Native);
            assert!(
                config.search.enabled,
                "test premise: {difficulty:?} must search continuations"
            );
            assert!(
                matches!(
                    optional_verdict(&state, true, &config),
                    PolicyVerdict::Score { .. }
                ),
                "{difficulty:?} must not hard-veto the sacrifice before search"
            );
        }
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
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let creature_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &creature_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let creature_score = SacrificeValuePolicy.score(&creature_ctx);

        // Score sacrificing the token
        let token_candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![token] },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let token_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &token_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
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
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
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
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let score = SacrificeValuePolicy.score(&ctx);
        assert!(
            score.abs() < 0.01,
            "No score outside sacrifice, got {score}"
        );
    }
}
