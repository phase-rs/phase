//! `SweeperTimingPolicy` — bias toward casting board wipes when they are
//! impactful (≥3 opposing creatures) and against casting them prematurely.
//!
//! CR 608.2: sweepers resolve and apply their effect to all matching permanents
//! at the time of resolution. CR 701.8: destroy moves permanents to the
//! graveyard. CR 701.13: exile removes them from the game entirely.

use engine::game::players;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::control::{is_sweeper_parts, COMMITMENT_FLOOR};
use crate::features::DeckFeatures;

/// Opponent creature threshold — at this count a sweeper is "timely".
const SWEEPER_TIMELY_THRESHOLD: u32 = 3;
/// Score bonus when the sweeper clears a meaningful board.
const DELTA_TIMELY: f64 = 1.5;
/// Score penalty when the sweeper would hit fewer than the threshold.
const DELTA_PREMATURE: f64 = -2.0;

pub struct SweeperTimingPolicy;

impl TacticalPolicy for SweeperTimingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SweeperTiming
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.control.sweeper_count == 0 {
            return None;
        }
        if features.control.commitment < COMMITMENT_FLOOR {
            return None;
        }
        Some(features.control.commitment)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Only applies to sweeper spells. Non-sweepers get a neutral verdict.
        if !candidate_is_sweeper(ctx) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("sweeper_timing_na"),
            };
        }

        let opposing_creatures = count_opposing_creatures(ctx.state, ctx.ai_player);

        if opposing_creatures >= SWEEPER_TIMELY_THRESHOLD {
            PolicyVerdict::Score {
                delta: DELTA_TIMELY,
                reason: PolicyReason::new("sweeper_timely")
                    .with_fact("opposing_creatures", opposing_creatures as i64),
            }
        } else {
            PolicyVerdict::Score {
                delta: DELTA_PREMATURE,
                reason: PolicyReason::new("sweeper_premature")
                    .with_fact("opposing_creatures", opposing_creatures as i64),
            }
        }
    }
}

/// True when the candidate action is casting a spell with sweeper-shaped effects.
/// Delegates to the feature module's `is_sweeper_parts` — single structural
/// source of truth shared between detection and runtime policy scoring.
fn candidate_is_sweeper(ctx: &PolicyContext<'_>) -> bool {
    let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
        return false;
    };
    let Some(obj) = ctx.state.objects.get(object_id) else {
        return false;
    };
    is_sweeper_parts(&obj.abilities)
}

/// Count the number of creature permanents controlled by opponents of `player`.
/// CR 608.2h: resolution checks the board state at the time of effect application.
fn count_opposing_creatures(state: &GameState, player: PlayerId) -> u32 {
    let opponents = players::opponents(state, player);
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            opponents.contains(&obj.controller)
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::control::ControlFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, TargetFilter};
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn control_features_with_sweepers(sweeper_count: u32, commitment: f32) -> DeckFeatures {
        DeckFeatures {
            control: ControlFeature {
                sweeper_count,
                commitment,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn context_with_features(features: DeckFeatures) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
    }

    fn cast_candidate(object_id: ObjectId, card_id: CardId) -> CandidateAction {
        CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Spell,
            },
        }
    }

    fn priority_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    fn make_sweeper_spell(state: &mut GameState, idx: u64) -> ObjectId {
        let oid = create_object(
            state,
            CardId(6000 + idx),
            AI,
            format!("Wrath {idx}"),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DestroyAll {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        ));
        oid
    }

    fn make_opponent_creature(state: &mut GameState, idx: u64) -> ObjectId {
        let oid = create_object(
            state,
            CardId(7000 + idx),
            OPP,
            format!("Opp Creature {idx}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.power = Some(2);
        obj.toughness = Some(2);
        oid
    }

    fn make_non_sweeper_spell(state: &mut GameState, idx: u64) -> ObjectId {
        let oid = create_object(
            state,
            CardId(8000 + idx),
            AI,
            format!("Non-sweeper {idx}"),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: engine::types::ability::QuantityExpr::Fixed { value: 2 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        ));
        oid
    }

    #[test]
    fn activation_opts_out_with_zero_sweepers() {
        let features = control_features_with_sweepers(0, 0.8);
        let state = GameState::new_two_player(42);
        assert!(SweeperTimingPolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    #[test]
    fn activation_opts_out_with_low_commitment() {
        let features = control_features_with_sweepers(3, 0.1);
        let state = GameState::new_two_player(42);
        assert!(SweeperTimingPolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    #[test]
    fn activation_active_with_sweepers_and_commitment() {
        let features = control_features_with_sweepers(3, 0.6);
        let state = GameState::new_two_player(42);
        assert!(SweeperTimingPolicy
            .activation(&features, &state, AI)
            .is_some());
    }

    #[test]
    fn premature_penalty_under_threshold() {
        // 2 opposing creatures < 3 → premature.
        let mut state = GameState::new_two_player(42);
        make_opponent_creature(&mut state, 0);
        make_opponent_creature(&mut state, 1);

        let sweeper_id = make_sweeper_spell(&mut state, 0);
        let candidate = cast_candidate(sweeper_id, CardId(6000));
        let decision = priority_decision();
        let (context, config) = context_with_features(control_features_with_sweepers(3, 0.8));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = SweeperTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "sweeper_premature");
                assert!(delta < 0.0, "expected negative delta, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn timely_bonus_at_threshold() {
        // 3 opposing creatures ≥ 3 → timely.
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            make_opponent_creature(&mut state, i);
        }

        let sweeper_id = make_sweeper_spell(&mut state, 0);
        let candidate = cast_candidate(sweeper_id, CardId(6000));
        let decision = priority_decision();
        let (context, config) = context_with_features(control_features_with_sweepers(3, 0.8));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = SweeperTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "sweeper_timely");
                assert!(delta > 0.0, "expected positive delta, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn non_sweeper_spell_is_neutral() {
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            make_opponent_creature(&mut state, i);
        }

        let spell_id = make_non_sweeper_spell(&mut state, 0);
        let candidate = cast_candidate(spell_id, CardId(8000));
        let decision = priority_decision();
        let (context, config) = context_with_features(control_features_with_sweepers(3, 0.8));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = SweeperTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "sweeper_timing_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
