//! `SeparatePilesTimingPolicy` — prevent the AI from casting "separate into
//! piles" spells (e.g. Make an Example) when opponents control no creatures.
//!
//! These spells ask each opponent to partition the creatures they control into
//! two piles; the caster then chooses one pile per opponent and those creatures
//! are sacrificed. When every opponent controls zero creatures both piles are
//! empty and the spell resolves with no effect — a wasted card and four mana.
//!
//! Per CR 700.3: partitioning is a resolution-time set computation, not a
//! targeting step, so the spell is technically legal to cast regardless of
//! board state. This policy adds strategic intelligence the rules do not
//! enforce.
//!
//! Verdict table (total opponent creatures across all opponents):
//!   0   → Reject  (spell does nothing)
//!   1–2 → strong penalty  (lopsided piles, caster picks the 0-creature pile)
//!   3–4 → small penalty   (marginal; opponent always keeps the better half)
//!   5+  → small bonus     (meaningful selection pressure)

use engine::game::players;
use engine::types::ability::{Effect, PlayerScope, VoterScope};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::DeckFeatures;

/// Minimum opponent-creature count at which the spell provides real selection
/// pressure (opponent must sacrifice at least 2+ creatures to give you a pile
/// with anything in it).
const MEANINGFUL_THRESHOLD: u32 = 5;
/// Creature count below which the spell is nearly useless.
const MARGINAL_THRESHOLD: u32 = 3;
/// Creature count at which the spell is pointless (1–2 means opponent can
/// always offer a 0-creature pile for you to pick, keeping all their creatures).
const WEAK_THRESHOLD: u32 = 1;

const DELTA_MEANINGFUL: f64 = 0.3;
const DELTA_MARGINAL: f64 = -1.0;
const DELTA_WEAK: f64 = -2.5;

pub struct SeparatePilesTimingPolicy;

impl TacticalPolicy for SeparatePilesTimingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SeparatePilesTiming
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // activation-constant: 1.0 — always fires for CastSpell; verdict filters non-pile spells to neutral.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("separate_piles_na"));
        };
        let Some(obj) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("separate_piles_na"));
        };

        // Only applies to spells where the AI is the pile-chooser and each
        // opponent partitions their own creatures. Neutral on all other spells.
        let has_separate_piles = obj
            .abilities
            .iter()
            .any(ability_is_opponent_separate_piles_ai_chooses);
        if !has_separate_piles {
            return PolicyVerdict::neutral(PolicyReason::new("separate_piles_na"));
        }

        let opponent_creatures = count_opponent_creatures(ctx.state, ctx.ai_player);

        if opponent_creatures == 0 {
            // CR 700.3: piles can be empty, but if ALL creatures are already
            // zero the spell resolves with no effect whatsoever — hard reject.
            return PolicyVerdict::reject(
                PolicyReason::new("separate_piles_no_targets").with_fact("opponent_creatures", 0),
            );
        }

        if opponent_creatures < WEAK_THRESHOLD + 1 {
            // 1 creature: opponent puts it in pile A, leaves pile B empty;
            // caster must pick one pile — picking B nets 0 sacrifices,
            // picking A nets 1 but the opponent got to arrange optimally.
            // Effectively a 4-mana "maybe kill one creature."
            return PolicyVerdict::strong(
                DELTA_WEAK,
                PolicyReason::new("separate_piles_weak")
                    .with_fact("opponent_creatures", opponent_creatures as i64),
            );
        }

        if opponent_creatures < MARGINAL_THRESHOLD {
            // 2 creatures: opponent splits 1/1; we pick one, they lose one.
            // Marginally useful but expensive at 4 mana for a sorcery-speed
            // single removal.
            return PolicyVerdict::preference(
                DELTA_MARGINAL,
                PolicyReason::new("separate_piles_marginal")
                    .with_fact("opponent_creatures", opponent_creatures as i64),
            );
        }

        if opponent_creatures < MEANINGFUL_THRESHOLD {
            // 3–4 creatures: opponent still controls the split, but we force
            // them to sacrifice at least one meaningful pile.
            return PolicyVerdict::preference(
                DELTA_MARGINAL / 2.0,
                PolicyReason::new("separate_piles_below_threshold")
                    .with_fact("opponent_creatures", opponent_creatures as i64),
            );
        }

        // 5+ creatures: genuine selection pressure; opponent must sacrifice
        // a real portion of their board regardless of how they split.
        PolicyVerdict::nudge(
            DELTA_MEANINGFUL,
            PolicyReason::new("separate_piles_timely")
                .with_fact("opponent_creatures", opponent_creatures as i64),
        )
    }
}

/// True when `ability`'s full effect chain contains a `SeparateIntoPiles`
/// where each opponent partitions (`EachOpponent`) and the AI is the
/// pile-chooser (`Controller`). Uses `collect_chain_effects` so the check
/// covers top-level and sub-ability placement equally.
///
/// The `chooser == Controller` guard is load-bearing: the "opponent keeps
/// the better half" value model only holds when the AI makes the pile
/// selection. If a future card gives the opponent the choice, the value
/// model flips and this policy must bail to neutral.
fn ability_is_opponent_separate_piles_ai_chooses(
    ability: &engine::types::ability::AbilityDefinition,
) -> bool {
    collect_chain_effects(ability).into_iter().any(|effect| {
        matches!(
            effect,
            Effect::SeparateIntoPiles {
                partition_subject: VoterScope::EachOpponent,
                chooser: PlayerScope::Controller,
                ..
            }
        )
    })
}

/// Count the total number of creature permanents controlled by any opponent of
/// `player` across the battlefield.
/// CR 608.2h: the board state queried here mirrors what the engine sees at
/// resolution time (pre-resolution policy check; actual resolution is
/// deterministic on the same state).
fn count_opponent_creatures(state: &GameState, player: PlayerId) -> u32 {
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
    use crate::features::DeckFeatures;
    use crate::policies::context::PolicyContext;
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PlayerScope, QuantityExpr, TargetFilter,
        TypeFilter, TypedFilter, VoterScope,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);
    const OPP: PlayerId = PlayerId(1);

    fn priority_decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

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

    fn make_context(_state: &GameState) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let session = AiSession::empty();
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
    }

    /// Create a spell with `Effect::SeparateIntoPiles { partition_subject: EachOpponent, ... }`
    /// mimicking Make an Example's parsed output.
    fn make_separate_piles_spell(state: &mut GameState, idx: u64) -> (ObjectId, CardId) {
        let card_id = CardId(9000 + idx);
        let oid = create_object(
            state,
            card_id,
            AI,
            format!("Make an Example {idx}"),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        let sacrifice_effect = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: TargetFilter::SelfRef,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        );
        let mut main_ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::SeparateIntoPiles {
                partition_subject: VoterScope::EachOpponent,
                object_filter: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: Vec::new(),
                }),
                chooser: PlayerScope::Controller,
                chosen_pile_effect: Box::new(sacrifice_effect),
                pile_source: engine::types::ability::PileSource::Battlefield,
                unchosen_pile_effect: None,
            },
        );
        main_ability.sub_ability = None;
        Arc::make_mut(&mut obj.abilities).push(main_ability);
        (oid, card_id)
    }

    /// Create a non-pile spell (draw 2).
    fn make_draw_spell(state: &mut GameState, idx: u64) -> (ObjectId, CardId) {
        let card_id = CardId(8000 + idx);
        let oid = create_object(state, card_id, AI, format!("Divination {idx}"), Zone::Hand);
        let obj = state.objects.get_mut(&oid).unwrap();
        obj.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Sorcery],
            subtypes: Vec::new(),
        };
        Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
        ));
        (oid, card_id)
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

    fn policy_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        candidate: &'a CandidateAction,
        config: &'a AiConfig,
        context: &'a AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: AI,
            config,
            context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        }
    }

    #[test]
    fn rejects_cast_when_opponent_has_no_creatures() {
        // The reported bug: AI casts Make an Example when opponent has 0 creatures.
        let mut state = GameState::new_two_player(42);
        let (oid, cid) = make_separate_piles_spell(&mut state, 0);
        let candidate = cast_candidate(oid, cid);
        let decision = priority_decision();
        let (context, config) = make_context(&state);
        let ctx = policy_ctx(&state, &decision, &candidate, &config, &context);

        let verdict = SeparatePilesTimingPolicy.verdict(&ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Reject { .. }),
            "expected Reject when opponent has 0 creatures, got {verdict:?}"
        );
    }

    #[test]
    fn rejects_returns_correct_reason_kind() {
        let mut state = GameState::new_two_player(42);
        let (oid, cid) = make_separate_piles_spell(&mut state, 0);
        let candidate = cast_candidate(oid, cid);
        let decision = priority_decision();
        let (context, config) = make_context(&state);
        let ctx = policy_ctx(&state, &decision, &candidate, &config, &context);

        let verdict = SeparatePilesTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Reject { reason } => {
                assert_eq!(reason.kind, "separate_piles_no_targets");
                assert_eq!(reason.facts[0], ("opponent_creatures", 0));
            }
            other => panic!("expected Reject, got {other:?}"),
        }
    }

    #[test]
    fn strong_penalty_with_one_opponent_creature() {
        // 1 creature: opponent offers [creature] + [empty]; we must pick one.
        // At best we remove 1 creature for 4 mana at sorcery speed — very weak.
        let mut state = GameState::new_two_player(42);
        make_opponent_creature(&mut state, 0);
        let (oid, cid) = make_separate_piles_spell(&mut state, 0);
        let candidate = cast_candidate(oid, cid);
        let decision = priority_decision();
        let (context, config) = make_context(&state);
        let ctx = policy_ctx(&state, &decision, &candidate, &config, &context);

        let verdict = SeparatePilesTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "separate_piles_weak");
                assert!(delta < -1.5, "expected strong negative delta, got {delta}");
            }
            other => panic!("expected Score, got {other:?}"),
        }
    }

    #[test]
    fn marginal_penalty_with_two_opponent_creatures() {
        // 2 creatures: opponent splits 1/1; we pick one, opponent loses one.
        // Marginal value; expensive for a sorcery.
        let mut state = GameState::new_two_player(42);
        make_opponent_creature(&mut state, 0);
        make_opponent_creature(&mut state, 1);
        let (oid, cid) = make_separate_piles_spell(&mut state, 0);
        let candidate = cast_candidate(oid, cid);
        let decision = priority_decision();
        let (context, config) = make_context(&state);
        let ctx = policy_ctx(&state, &decision, &candidate, &config, &context);

        let verdict = SeparatePilesTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "separate_piles_marginal");
                assert!(delta < 0.0, "expected negative delta, got {delta}");
            }
            other => panic!("expected Score, got {other:?}"),
        }
    }

    #[test]
    fn below_threshold_penalty_with_three_opponent_creatures() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            make_opponent_creature(&mut state, i);
        }
        let (oid, cid) = make_separate_piles_spell(&mut state, 0);
        let candidate = cast_candidate(oid, cid);
        let decision = priority_decision();
        let (context, config) = make_context(&state);
        let ctx = policy_ctx(&state, &decision, &candidate, &config, &context);

        let verdict = SeparatePilesTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "separate_piles_below_threshold");
                assert!(
                    delta < 0.0,
                    "expected negative delta for 3 creatures, got {delta}"
                );
            }
            other => panic!("expected Score, got {other:?}"),
        }
    }

    #[test]
    fn bonus_with_five_or_more_opponent_creatures() {
        // 5+ creatures → timely; opponent must sacrifice a real portion.
        let mut state = GameState::new_two_player(42);
        for i in 0..5 {
            make_opponent_creature(&mut state, i);
        }
        let (oid, cid) = make_separate_piles_spell(&mut state, 0);
        let candidate = cast_candidate(oid, cid);
        let decision = priority_decision();
        let (context, config) = make_context(&state);
        let ctx = policy_ctx(&state, &decision, &candidate, &config, &context);

        let verdict = SeparatePilesTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "separate_piles_timely");
                assert!(
                    delta > 0.0,
                    "expected positive delta for 5 creatures, got {delta}"
                );
            }
            other => panic!("expected Score, got {other:?}"),
        }
    }

    #[test]
    fn non_pile_spell_is_neutral() {
        // Draw spells should get a neutral verdict regardless of board state.
        let mut state = GameState::new_two_player(42);
        for i in 0..10 {
            make_opponent_creature(&mut state, i);
        }
        let (oid, cid) = make_draw_spell(&mut state, 0);
        let candidate = cast_candidate(oid, cid);
        let decision = priority_decision();
        let (context, config) = make_context(&state);
        let ctx = policy_ctx(&state, &decision, &candidate, &config, &context);

        let verdict = SeparatePilesTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "separate_piles_na");
                assert_eq!(delta, 0.0);
            }
            other => panic!("expected neutral Score, got {other:?}"),
        }
    }

    #[test]
    fn activation_always_returns_some() {
        let state = GameState::new_two_player(42);
        let features = DeckFeatures::default();
        assert!(
            SeparatePilesTimingPolicy
                .activation(&features, &state, AI)
                .is_some(),
            "SeparatePilesTimingPolicy must always activate for CastSpell decisions"
        );
    }

    #[test]
    fn id_is_separate_piles_timing() {
        assert_eq!(
            SeparatePilesTimingPolicy.id(),
            PolicyId::SeparatePilesTiming
        );
    }
}
