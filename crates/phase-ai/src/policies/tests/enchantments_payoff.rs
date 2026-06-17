//! Tests for `EnchantmentsPayoffPolicy`. Live in a sibling test module (declared
//! from `policies/tests/mod.rs`) so `policies/enchantments_payoff.rs` stays
//! implementation-only and SOURCE-classified.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::enchantments::EnchantmentsFeature;
use crate::features::DeckFeatures;
use crate::session::AiSession;

use super::super::context::PolicyContext;
use super::super::enchantments_payoff::EnchantmentsPayoffPolicy;
use super::super::registry::{DecisionKind, PolicyId, PolicyVerdict, TacticalPolicy};

const AI: PlayerId = PlayerId(0);

fn features(commitment: f32, payoff_count: u32) -> DeckFeatures {
    DeckFeatures {
        enchantments: EnchantmentsFeature {
            enchantment_count: 12,
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
    assert_eq!(EnchantmentsPayoffPolicy.id(), PolicyId::EnchantmentsPayoff);
    assert!(EnchantmentsPayoffPolicy
        .decision_kinds()
        .contains(&DecisionKind::CastSpell));
}

// ─── activation gate ─────────────────────────────────────────────────────────

#[test]
fn opts_out_with_no_payoff_even_at_high_commitment() {
    let features = features(0.9, 0);
    let state = GameState::new_two_player(7);
    assert!(EnchantmentsPayoffPolicy
        .activation(&features, &state, AI)
        .is_none());
}

#[test]
fn opts_out_below_commitment_floor() {
    let features = features(0.1, 4);
    let state = GameState::new_two_player(7);
    assert!(EnchantmentsPayoffPolicy
        .activation(&features, &state, AI)
        .is_none());
}

#[test]
fn opts_in_with_payoff_above_floor() {
    let features = features(0.6, 4);
    let state = GameState::new_two_player(7);
    assert_eq!(
        EnchantmentsPayoffPolicy.activation(&features, &state, AI),
        Some(0.6)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn enchantment_cast_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 1, vec![CoreType::Enchantment]);

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

    let (delta, kind) = delta_of(EnchantmentsPayoffPolicy.verdict(&ctx));
    assert_eq!(kind, "enchantment_cast_for_payoff");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
}

#[test]
fn non_enchantment_spell_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Sorcery]);

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

    let (delta, kind) = delta_of(EnchantmentsPayoffPolicy.verdict(&ctx));
    assert_eq!(kind, "enchantments_payoff_inert");
    assert_eq!(delta, 0.0);
}
