//! Tribal lord priority policy.
//!
//! Scores `CastSpell` candidates based on their tribal role relative to the
//! deck's dominant tribe. Lords are strongly preferred; on-tribe members are
//! modestly preferred; off-tribe creatures are penalized.
//!
//! CR 205.3: subtype-based tribal membership.
//! CR 613.4c: lord P/T anthems apply in layer 7c.

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::tribal::{statics_are_lord_for, LORD_PRIORITY_FLOOR};
use crate::features::DeckFeatures;
use engine::parser::oracle_util::canonicalize_subtype_name;

/// Bonus for casting an on-tribe lord.
/// CR 613.4c: lords grant layer 7c P/T modifications to other tribe members.
const DELTA_LORD: f64 = 2.0;
/// Bonus for casting an on-tribe non-lord creature.
const DELTA_ON_TRIBE: f64 = 0.5;
/// Penalty for casting an off-tribe creature in a tribal deck.
const DELTA_OFF_TRIBE: f64 = -0.3;

pub struct TribalLordPriorityPolicy;

impl TacticalPolicy for TribalLordPriorityPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::TribalLordPriority
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
        // CR 205.3: tribal commitment must exceed the floor before lord-prioritization
        // makes sense — incidental subtypes don't warrant re-ordering casts.
        if features.tribal.commitment < LORD_PRIORITY_FLOOR {
            None
        } else {
            Some(features.tribal.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("tribal_lord_na"),
            };
        };

        let features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();

        let Some(dominant_tribe) = features.tribal.dominant_tribe.as_deref() else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("tribal_lord_na"),
            };
        };

        let Some(obj) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("tribal_lord_na"),
            };
        };

        // CR 205.3: subtype-based tribal membership.
        let on_tribe = obj
            .card_types
            .subtypes
            .iter()
            .any(|s| canonicalize_subtype_name(s) == dominant_tribe);

        // Count on-board tribe members for observability.
        let on_board = count_tribe_on_board(ctx.state, ctx.ai_player, dominant_tribe);

        if on_tribe {
            // CR 613.4c: lords grant P/T (or ability) modifications to other tribe members
            // in layer 7c. Detect via the object's runtime static_abilities.
            if statics_are_lord_for(&obj.static_definitions, dominant_tribe) {
                return PolicyVerdict::Score {
                    delta: DELTA_LORD,
                    reason: PolicyReason::new("tribal_lord_prioritized")
                        .with_fact("on_board_tribe_members", on_board as i64),
                };
            }

            return PolicyVerdict::Score {
                delta: DELTA_ON_TRIBE,
                reason: PolicyReason::new("tribal_member_deploy")
                    .with_fact("on_board_tribe_members", on_board as i64),
            };
        }

        // Off-tribe creature: penalize only when the object is a creature type.
        let is_creature = obj
            .card_types
            .core_types
            .iter()
            .any(|t| matches!(t, CoreType::Creature));

        if is_creature {
            return PolicyVerdict::Score {
                delta: DELTA_OFF_TRIBE,
                reason: PolicyReason::new("off_tribe_creature"),
            };
        }

        PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("tribal_lord_na"),
        }
    }
}

fn count_tribe_on_board(state: &GameState, player: PlayerId, tribe: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| obj.controller == player && obj.zone == Zone::Battlefield)
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| canonicalize_subtype_name(s) == tribe)
        })
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::tribal::TribalFeature;
    use crate::features::DeckFeatures;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

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

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    fn tribal_features(commitment: f32, dominant: &str) -> DeckFeatures {
        DeckFeatures {
            tribal: TribalFeature {
                dominant_tribe: Some(dominant.to_string()),
                commitment,
                tribes: Vec::new(),
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

    #[test]
    fn activation_opts_out_below_floor() {
        let features = tribal_features(0.1, "Elf");
        let state = GameState::new_two_player(42);
        assert!(TribalLordPriorityPolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    #[test]
    fn on_tribe_creature_scores_positive() {
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(1);
        let oid = create_object(
            &mut state,
            card_id,
            AI,
            "Elf Warrior".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Elf".to_string()],
        };

        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(tribal_features(0.8, "Elf"));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = TribalLordPriorityPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "tribal_member_deploy");
                assert!(delta > 0.0, "expected positive delta for on-tribe creature");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn off_tribe_creature_scores_negative() {
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(2);
        let oid = create_object(&mut state, card_id, AI, "Zombie".to_string(), Zone::Hand);
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Zombie".to_string()],
        };

        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(tribal_features(0.8, "Elf"));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = TribalLordPriorityPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "off_tribe_creature");
                assert!(
                    delta < 0.0,
                    "expected negative delta for off-tribe creature"
                );
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn non_creature_spell_is_na() {
        let mut state = GameState::new_two_player(42);
        let card_id = CardId(3);
        let oid = create_object(
            &mut state,
            card_id,
            AI,
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        };

        let candidate = cast_candidate(oid, card_id);
        let (context, config) = context_with_features(tribal_features(0.8, "Elf"));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision(),
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = TribalLordPriorityPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "tribal_lord_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
