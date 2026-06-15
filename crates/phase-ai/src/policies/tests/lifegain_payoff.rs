//! Tests for `LifegainPayoffPolicy`. Live in a sibling test module (declared
//! from `policies/tests/mod.rs`) so `policies/lifegain_payoff.rs` stays
//! implementation-only and SOURCE-classified.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::lifegain::LifegainFeature;
use crate::features::DeckFeatures;
use crate::session::AiSession;

use super::super::context::PolicyContext;
use super::super::lifegain_payoff::LifegainPayoffPolicy;
use super::super::registry::{DecisionKind, PolicyId, PolicyVerdict, TacticalPolicy};

const AI: PlayerId = PlayerId(0);

fn features(commitment: f32, payoff_count: u32) -> DeckFeatures {
    DeckFeatures {
        lifegain: LifegainFeature {
            source_count: 6,
            payoff_count,
            commitment,
        },
        ..DeckFeatures::default()
    }
}

fn ai_context(commitment: f32, payoff_count: u32) -> (AiContext, AiConfig) {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session
        .features
        .insert(AI, features(commitment, payoff_count));
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;
    (context, config)
}

fn decision() -> AiDecisionContext {
    AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: Vec::new(),
    }
}

fn cast_candidate(object_id: ObjectId) -> CandidateAction {
    CandidateAction {
        action: GameAction::CastSpell {
            object_id,
            card_id: CardId(object_id.0),
            targets: Vec::new(),
            payment_mode: CastPaymentMode::default(),
        },
        metadata: ActionMetadata {
            actor: Some(AI),
            tactical_class: TacticalClass::Spell,
        },
    }
}

fn spell_object(state: &mut GameState, idx: u64, core: Vec<CoreType>) -> ObjectId {
    let oid = create_object(state, CardId(idx), AI, format!("Spell {idx}"), Zone::Stack);
    state.objects.get_mut(&oid).unwrap().card_types = CardType {
        supertypes: Vec::new(),
        core_types: core,
        subtypes: Vec::new(),
    };
    oid
}

fn delta_of(verdict: PolicyVerdict) -> (f64, String) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (delta, reason.kind.to_string()),
        PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
    }
}

// ─── identity ────────────────────────────────────────────────────────────────

#[test]
fn policy_identity() {
    assert_eq!(LifegainPayoffPolicy.id(), PolicyId::LifegainPayoff);
    assert!(LifegainPayoffPolicy
        .decision_kinds()
        .contains(&DecisionKind::CastSpell));
}

// ─── activation gate ─────────────────────────────────────────────────────────

#[test]
fn opts_out_with_no_payoff_even_at_high_commitment() {
    // Payoff-gated: no payoff → inert even if commitment is high.
    let features = features(0.9, 0);
    let state = GameState::new_two_player(7);
    assert!(LifegainPayoffPolicy
        .activation(&features, &state, AI)
        .is_none());
}

#[test]
fn opts_out_below_commitment_floor() {
    let features = features(0.1, 4);
    let state = GameState::new_two_player(7);
    assert!(LifegainPayoffPolicy
        .activation(&features, &state, AI)
        .is_none());
}

#[test]
fn opts_in_with_payoff_above_floor() {
    let features = features(0.6, 4);
    let state = GameState::new_two_player(7);
    assert_eq!(
        LifegainPayoffPolicy.activation(&features, &state, AI),
        Some(0.6)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn lifelink_source_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 1, vec![CoreType::Creature]);
    state
        .objects
        .get_mut(&oid)
        .unwrap()
        .keywords
        .push(Keyword::Lifelink);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 4);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
    };

    let (delta, kind) = delta_of(LifegainPayoffPolicy.verdict(&ctx));
    assert_eq!(kind, "lifegain_source_for_payoff");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
}

#[test]
fn gain_life_spell_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Instant]);
    Arc::make_mut(&mut state.objects.get_mut(&oid).unwrap().abilities).push(
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        ),
    );

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 4);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
    };

    let (delta, kind) = delta_of(LifegainPayoffPolicy.verdict(&ctx));
    assert_eq!(kind, "lifegain_source_for_payoff");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
}

#[test]
fn trigger_borne_lifegain_source_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 4, vec![CoreType::Creature]);
    state
        .objects
        .get_mut(&oid)
        .unwrap()
        .trigger_definitions
        .push(
            TriggerDefinition::new(TriggerMode::ChangesZone).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )),
        );

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 4);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
    };

    let (delta, kind) = delta_of(LifegainPayoffPolicy.verdict(&ctx));
    assert_eq!(kind, "lifegain_source_for_payoff");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
}

#[test]
fn non_source_spell_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 3, vec![CoreType::Sorcery]);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 4);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
    };

    let (delta, kind) = delta_of(LifegainPayoffPolicy.verdict(&ctx));
    assert_eq!(kind, "lifegain_payoff_inert");
    assert_eq!(delta, 0.0);
}
