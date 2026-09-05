//! Tribal lord priority policy.
//!
//! Scores `CastSpell` candidates based on their tribal role relative to the
//! deck's dominant tribe. Lords are strongly preferred; on-tribe members are
//! modestly preferred; off-tribe creatures are penalized.
//!
//! CR 205.3: subtype-based tribal membership.
//! CR 613.4c: lord P/T anthems apply in layer 7c.

use engine::game::game_object::GameObject;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::tribal::{statics_are_lord_for, LORD_PRIORITY_FLOOR};
use crate::features::DeckFeatures;
use engine::parser::oracle_util::canonicalize_subtype_name;
#[cfg(test)]
use engine::types::game_state::CastPaymentMode;

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
        let on_board = count_subtype_members(
            ctx.state,
            ctx.state.battlefield.iter().copied(),
            |obj| obj.controller == ctx.ai_player && obj.zone == Zone::Battlefield,
            dominant_tribe,
        );

        if on_tribe {
            // CR 613.4c: lords grant P/T (or ability) modifications to other tribe members
            // in layer 7c. Detect via the object's runtime static_abilities.
            if statics_are_lord_for(obj.static_definitions.as_slice(), dominant_tribe) {
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

/// Shared creature-type census. Counts the objects in `ids` that pass `filter`
/// and belong to the canonical creature type `canon`; each object contributes
/// at most 1. The zone/controller predicate is the caller's (`filter`), so the
/// same census serves a battlefield-controller sweep, a command-zone owner
/// sweep, and an unfiltered hand sweep.
///
/// CR 205.3m: creature types are the subtypes shared by creature and Kindred
/// cards, so subtype membership is the authoritative test.
/// CR 702.73a: Changeling means "this object is every creature type", so a
/// changeling is a member of `canon` whatever its printed subtypes. The `||`
/// cannot double count — each object is counted once regardless of which arm
/// matches — which also makes it exact for battlefield changelings, whose types
/// the layer system has already expanded into `subtypes`.
pub(super) fn count_subtype_members(
    state: &GameState,
    ids: impl Iterator<Item = ObjectId>,
    filter: impl Fn(&GameObject) -> bool,
    canon: &str,
) -> usize {
    ids.filter_map(|id| state.objects.get(&id))
        .filter(|obj| filter(obj))
        .filter(|obj| {
            obj.card_types
                .subtypes
                .iter()
                .any(|s| canonicalize_subtype_name(s) == canon)
                || obj.has_keyword(&Keyword::Changeling)
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

                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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
            search_depth: crate::policies::context::SearchDepth::Root,
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

    fn put_creature(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
        zone: Zone,
        subtypes: &[&str],
    ) -> ObjectId {
        let oid = create_object(state, card_id, owner, name.to_string(), zone);
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Creature],
            subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
        };
        oid
    }

    /// The census is the shared building block: the caller's `filter` owns the
    /// zone/controller predicate, and CR 702.73a membership is the helper's own
    /// rule rather than a per-policy quirk.
    #[test]
    fn count_subtype_members_filters_by_controller_and_counts_changelings() {
        const OPPONENT: PlayerId = PlayerId(1);
        let mut state = GameState::new_two_player(42);

        put_creature(
            &mut state,
            CardId(10),
            AI,
            "Llanowar Elves",
            Zone::Battlefield,
            &["Elf"],
        );
        // Excluded by the caller's controller predicate, not by the census.
        put_creature(
            &mut state,
            CardId(11),
            OPPONENT,
            "Elvish Mystic",
            Zone::Battlefield,
            &["Elf"],
        );
        // CR 702.73a: no printed Elf subtype, but Changeling makes it one.
        let changeling = put_creature(
            &mut state,
            CardId(12),
            AI,
            "Woodland Changeling",
            Zone::Battlefield,
            &[],
        );
        state
            .objects
            .get_mut(&changeling)
            .unwrap()
            .keywords
            .push(Keyword::Changeling);

        let count = count_subtype_members(
            &state,
            state.battlefield.iter().copied(),
            |obj| obj.controller == AI && obj.zone == Zone::Battlefield,
            "Elf",
        );
        assert_eq!(
            count, 2,
            "the AI's Elf and its changeling count; the opponent's Elf does not"
        );
    }

    #[test]
    fn on_board_tribe_members_fact_counts_battlefield_members() {
        let mut state = GameState::new_two_player(42);
        put_creature(
            &mut state,
            CardId(20),
            AI,
            "Llanowar Elves",
            Zone::Battlefield,
            &["Elf"],
        );
        let card_id = CardId(21);
        let oid = put_creature(&mut state, card_id, AI, "Elf Warrior", Zone::Hand, &["Elf"]);

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
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let verdict = TribalLordPriorityPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { reason, .. } => {
                assert_eq!(reason.kind, "tribal_member_deploy");
                assert!(
                    reason.facts.contains(&("on_board_tribe_members", 1)),
                    "expected the battlefield Elf in the fact, got {:?}",
                    reason.facts
                );
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }
}
