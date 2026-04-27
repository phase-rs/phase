use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::config::PolicyPenalties;
use crate::features::DeckFeatures;

use super::activation::turn_only;
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
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("blight_value_score"),
        }
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
                count: 1,
                creatures: vec![small, big],
                pending_cast: dummy_pending(),
            },
            candidates: Vec::new(),
        };

        // Score blighting the 3/1 — dies to the -1/-1 counter.
        let small_candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![small] },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };
        let small_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &small_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let small_score = BlightValuePolicy.score(&small_ctx);

        // Score blighting the 3/5 — survives, loses ~1/5 of its value.
        let big_candidate = CandidateAction {
            action: GameAction::SelectCards { cards: vec![big] },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };
        let big_ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &big_candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };
        let big_score = BlightValuePolicy.score(&big_ctx);

        assert!(
            big_score > small_score,
            "Should prefer blighting the 3/5 ({big_score}) over the 3/1 ({small_score}) — \
             the 3/1 dies to its -1/-1 counter"
        );
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

        let score = BlightValuePolicy.score(&ctx);
        assert!(score.abs() < 0.01, "No score outside blight, got {score}");
    }
}
