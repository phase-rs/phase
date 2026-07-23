use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::config::PolicyPenalties;
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use super::strategy_helpers::sacrifice_cost;

pub struct BlightValuePolicy;

/// Cost of placing a -1/-1 counter on `obj_id`.
///
/// CR 121.3: A -1/-1 counter reduces both power and toughness by 1.
/// CR 704.5f: A creature with toughness 0 or less is put into its owner's
/// graveyard as a state-based action.
///
/// If the creature's toughness is 1 or less, the counter kills it outright —
/// cost equals its full sacrifice value. Otherwise, the creature loses
/// roughly `1 / toughness` of its stat line, so cost scales inversely with
/// toughness.
fn blight_cost(state: &GameState, obj_id: ObjectId, penalties: &PolicyPenalties) -> f64 {
    let Some(obj) = state.objects.get(&obj_id) else {
        return 0.0;
    };
    let base = sacrifice_cost(state, obj_id, penalties);
    let toughness = obj.toughness.unwrap_or(0);
    if toughness <= 1 {
        // Creature dies to SBA when it receives a -1/-1 counter.
        return base;
    }
    base / (toughness as f64)
}

impl BlightValuePolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        // Guard: only score SelectCards during blight decisions.
        let GameAction::SelectCards { cards } = &ctx.candidate.action else {
            return 0.0;
        };
        if !matches!(ctx.decision.waiting_for, WaitingFor::BlightChoice { .. }) {
            return 0.0;
        }

        // Score inversely to cost: cheap blight targets produce less negative scores.
        let total_cost: f64 = cards
            .iter()
            .map(|&obj_id| blight_cost(ctx.state, obj_id, ctx.penalties()))
            .sum();
        -total_cost
    }
}

impl TacticalPolicy for BlightValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::BlightValue
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
        // The cost of placing a -1/-1 counter is intrinsic to the creature being
        // blighted — a 12/1 dies just as dead on turn 2 as on turn 9 — so it must
        // not scale with game phase. Mirrors SacrificeValuePolicy /
        // PaymentSelectionPolicy. A turn-phase multiplier (>1.0) over the
        // unbounded blight score could push a high-power, low-toughness creature
        // past the registry's CRITICAL_MAX ceiling (see issue #4282).
        // activation-constant: phase-independent blight resource valuation.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Route through the band contract helper rather than hand-building a raw
        // `Score`: `self.score()` is an unbounded sum of per-creature blight
        // costs, and `PolicyVerdict::score` clamps its magnitude into the
        // declared bands (|delta| <= CRITICAL_MAX). With activation pinned to
        // 1.0 above, the scaled delta can never exceed the critical ceiling.
        PolicyVerdict::score(self.score(ctx), PolicyReason::new("blight_value_score"))
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
    fn prefers_blighting_high_toughness_over_low_toughness() {
        let mut state = GameState::new_two_player(42);

        let small = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Small".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&small).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(1);

        let big_card = CardId(state.next_object_id);
        let big = create_object(
            &mut state,
            big_card,
            PlayerId(0),
            "Big".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&big).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(3);
        obj.toughness = Some(5);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::BlightChoice {
                player: PlayerId(0),
                counters: 1,
                creatures: vec![small, big],
                pending_cast: dummy_pending(),
            },
            candidates: Vec::new(),
        };

        // Score blighting the 3/1 — dies to the -1/-1 counter.
        let small_candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![small] },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let small_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &small_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let small_score = BlightValuePolicy.score(&small_ctx);

        // Score blighting the 3/5 — survives, loses ~1/5 of its value.
        let big_candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![big] },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Selection),
        };
        let big_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &big_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let big_score = BlightValuePolicy.score(&big_ctx);

        assert!(
            big_score > small_score,
            "Should prefer blighting the 3/5 ({big_score}) over the 3/1 ({small_score}) — \
             the 3/1 dies to its -1/-1 counter"
        );
    }

    /// Regression for #4282 (sibling of SacrificeValuePolicy): blighting a
    /// high-power, low-toughness creature must not produce a scaled delta beyond
    /// the critical band ceiling. A toughness-1 creature dies to its -1/-1
    /// counter, so `blight_cost` returns its full (unbounded) sacrifice value;
    /// before the fix, `verdict` returned that raw score and `activation` scaled
    /// it by `turn_phase_mult` (up to 1.3), tripping the registry's
    /// `debug_assert!(scaled_delta.abs() <= CRITICAL_MAX)`.
    #[test]
    fn large_blight_stays_within_critical_band() {
        use super::super::registry::CRITICAL_MAX;

        let mut state = GameState::new_two_player(42);

        // 12/1 => dies to the -1/-1 counter, so blight_cost = evaluate_creature
        // = 12*1.5 + 1 = 19.0, comfortably over the critical ceiling of 15.
        let glass_cannon = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Glass Cannon".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&glass_cannon).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(12);
        obj.toughness = Some(1);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::BlightChoice {
                player: PlayerId(0),
                counters: 1,
                creatures: vec![glass_cannon],
                pending_cast: dummy_pending(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::SelectCards {
                cards: vec![glass_cannon],
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

        // The raw score must exceed the ceiling, proving the clamp is exercised.
        assert!(
            BlightValuePolicy.score(&ctx).abs() > CRITICAL_MAX,
            "test premise: raw blight score should exceed the critical ceiling"
        );

        let PolicyVerdict::Score { delta, .. } = BlightValuePolicy.verdict(&ctx) else {
            panic!("blight value policy must return a Score verdict");
        };
        assert!(
            delta.abs() <= CRITICAL_MAX,
            "verdict delta {delta} must be clamped to the critical band ceiling {CRITICAL_MAX}"
        );

        let activation = BlightValuePolicy
            .activation(&DeckFeatures::default(), &state, PlayerId(0))
            .expect("blight value policy always activates");
        assert_eq!(activation, 1.0, "blight valuation must not scale by phase");
        assert!((delta * f64::from(activation)).abs() <= CRITICAL_MAX);
    }

    #[test]
    fn no_score_outside_blight_context() {
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

        let score = BlightValuePolicy.score(&ctx);
        assert!(score.abs() < 0.01, "No score outside blight, got {score}");
    }
}
