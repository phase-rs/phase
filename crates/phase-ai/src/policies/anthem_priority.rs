//! Anthem priority tactical policy.
//!
//! Scores `CastSpell` candidates to reward deploying anthem effects when the
//! AI already has creatures on the board, and penalizes premature anthem
//! casting when the board is small.
//!
//! CR 613.4c: layer 7c power/toughness modifications — anthem static effects.
//! CR 604.3: static ability definition.

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::tokens_wide::{is_anthem_parts, ANTHEM_TIMELY_BOARD_FLOOR, COMMITMENT_FLOOR};
use crate::features::DeckFeatures;

pub struct AnthemPriorityPolicy;

impl TacticalPolicy for AnthemPriorityPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::AnthemPriority
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    /// Opt out below `COMMITMENT_FLOOR` — anthems are irrelevant outside
    /// a tokens-wide or wide-board strategy.
    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        let commitment = features.tokens_wide.commitment;
        if commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("anthem_priority_na"),
            };
        };

        let Some(obj) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("anthem_priority_na"),
            };
        };

        // Only anthem cards get scored by this policy. CR 613.4c.
        if !is_anthem_parts(obj.static_definitions.as_slice()) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("anthem_priority_na"),
            };
        }

        // Count the AI's own creatures on the battlefield. CR 613.4c.
        let own_creatures = count_own_creatures(ctx.state, ctx.ai_player);

        if own_creatures >= ANTHEM_TIMELY_BOARD_FLOOR {
            // Timely deployment — anthem amplifies an already-wide board.
            PolicyVerdict::Score {
                delta: 2.0,
                reason: PolicyReason::new("anthem_timely")
                    .with_fact("own_creatures", own_creatures as i64),
            }
        } else {
            // Premature — anthem with no tokens is wasted tempo.
            PolicyVerdict::Score {
                delta: -0.5,
                reason: PolicyReason::new("anthem_premature")
                    .with_fact("own_creatures", own_creatures as i64),
            }
        }
    }
}

/// Count the number of creatures the AI controls on the battlefield.
/// CR 302: creature permanent type. CR 613.4c: anthem affects creatures.
fn count_own_creatures(state: &GameState, player: PlayerId) -> u32 {
    state
        .battlefield
        .iter()
        .filter(|&&id| {
            state.objects.get(&id).is_some_and(|obj| {
                obj.controller == player && obj.card_types.core_types.contains(&CoreType::Creature)
            })
        })
        .count() as u32
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::tokens_wide::TokensWideFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        ContinuousModification, ControllerRef, StaticDefinition, TargetFilter, TypeFilter,
        TypedFilter,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn features_with_commitment(commitment: f32) -> DeckFeatures {
        DeckFeatures {
            tokens_wide: TokensWideFeature {
                commitment,
                anthem_count: 4,
                token_generator_count: 8,
                mass_token_generator_count: 4,
                mass_pump_count: 2,
                wide_payoff_count: 4,
                payoff_names: Vec::new(),
                anthem_names: Vec::new(),
            },
            ..DeckFeatures::default()
        }
    }

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
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

    fn add_anthem(state: &mut GameState, id: u64) -> ObjectId {
        let card_id = CardId(id);
        let oid = create_object(
            state,
            card_id,
            AI,
            format!("Glorious Anthem {id}"),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Enchantment],
            subtypes: Vec::new(),
        };
        let creature_filter = TargetFilter::Typed(TypedFilter {
            controller: Some(ControllerRef::You),
            type_filters: vec![TypeFilter::Creature],
            ..TypedFilter::default()
        });
        obj.static_definitions.push(
            StaticDefinition::continuous()
                .affected(creature_filter)
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );
        oid
    }

    fn add_creature_to_battlefield(state: &mut GameState, id: u64) -> ObjectId {
        let card_id = CardId(id);
        let oid = create_object(state, card_id, AI, format!("Token {id}"), Zone::Battlefield);
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        };
        obj.power = Some(1);
        obj.toughness = Some(1);
        state.battlefield.push_back(oid);
        oid
    }

    // ── Tests ─────────────────────────────────────────────────────────────

    #[test]
    fn timely_at_three_creatures() {
        let mut state = GameState::new_two_player(42);
        // 3 creatures on battlefield = ANTHEM_TIMELY_BOARD_FLOOR.
        for i in 0..3 {
            add_creature_to_battlefield(&mut state, 100 + i);
        }
        let oid = add_anthem(&mut state, 1);
        let candidate = cast_candidate(oid, CardId(1));
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = AnthemPriorityPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(
                    delta > 0.0,
                    "timely anthem should score positive, got {delta}"
                );
                assert_eq!(reason.kind, "anthem_timely");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn premature_below_floor() {
        let mut state = GameState::new_two_player(42);
        // Only 1 creature — below ANTHEM_TIMELY_BOARD_FLOOR (3).
        add_creature_to_battlefield(&mut state, 200);
        let oid = add_anthem(&mut state, 2);
        let candidate = cast_candidate(oid, CardId(2));
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = AnthemPriorityPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert!(delta < 0.0, "premature anthem should penalize, got {delta}");
                assert_eq!(reason.kind, "anthem_premature");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn non_anthem_neutral() {
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(3);
        let oid = create_object(&mut state, card_id, AI, "Bear".to_string(), Zone::Hand);
        {
            let obj = state.objects.get_mut(&oid).unwrap();
            obj.card_types = CardType {
                supertypes: Vec::new(),
                core_types: vec![CoreType::Creature],
                subtypes: Vec::new(),
            };
        }
        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(features_with_commitment(0.6));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };
        let verdict = AnthemPriorityPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(delta, 0.0, "non-anthem should be neutral");
                assert_eq!(reason.kind, "anthem_priority_na");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn opts_out_below_commitment_floor() {
        let features = features_with_commitment(0.1); // below COMMITMENT_FLOOR (0.30)
        let state = GameState::new_two_player(42);
        assert!(AnthemPriorityPolicy
            .activation(&features, &state, AI)
            .is_none());
    }
}
