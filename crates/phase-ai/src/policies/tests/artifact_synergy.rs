//! Tests for `ArtifactSynergyPolicy`. Live in a sibling test module (declared
//! from `policies/tests/mod.rs`) so `policies/artifact_synergy.rs` stays
//! implementation-only and SOURCE-classified.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{TypeFilter, TypedFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::artifacts::ArtifactsFeature;
use crate::features::DeckFeatures;
use crate::session::AiSession;

use super::super::artifact_synergy::ArtifactSynergyPolicy;
use super::super::context::PolicyContext;
use super::super::registry::{DecisionKind, PolicyId, PolicyVerdict, TacticalPolicy};

const AI: PlayerId = PlayerId(0);

fn features_with_commitment(commitment: f32) -> DeckFeatures {
    DeckFeatures {
        artifacts: ArtifactsFeature {
            artifact_count: 20,
            payoff_count: 6,
            enabler_count: 4,
            commitment,
        },
        ..DeckFeatures::default()
    }
}

fn ai_context(commitment: f32) -> (AiContext, AiConfig) {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session
        .features
        .insert(AI, features_with_commitment(commitment));
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

/// Create a spell object on the stack with the given core types, returning its id.
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
    assert_eq!(
        ArtifactSynergyPolicy.id(),
        PolicyId::ArtifactSynergyTactical
    );
    assert!(ArtifactSynergyPolicy
        .decision_kinds()
        .contains(&DecisionKind::CastSpell));
}

// ─── activation gate ─────────────────────────────────────────────────────────

#[test]
fn opts_out_below_commitment_floor() {
    let features = DeckFeatures::default(); // commitment 0.0
    let state = GameState::new_two_player(7);
    assert!(ArtifactSynergyPolicy
        .activation(&features, &state, AI)
        .is_none());
}

#[test]
fn opts_in_above_floor() {
    let features = features_with_commitment(0.6);
    let state = GameState::new_two_player(7);
    assert_eq!(
        ArtifactSynergyPolicy.activation(&features, &state, AI),
        Some(0.6)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn affinity_for_artifacts_spell_preferred() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 1, vec![CoreType::Artifact, CoreType::Creature]);
    state
        .objects
        .get_mut(&oid)
        .unwrap()
        .keywords
        .push(Keyword::Affinity(TypedFilter::new(TypeFilter::Artifact)));

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
    };

    let (delta, kind) = delta_of(ArtifactSynergyPolicy.verdict(&ctx));
    assert_eq!(kind, "artifact_cost_payoff");
    assert!(delta > 0.3, "expected a preference-band delta, got {delta}");
}

#[test]
fn plain_artifact_nudged() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Artifact]);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
    };

    let (delta, kind) = delta_of(ArtifactSynergyPolicy.verdict(&ctx));
    assert_eq!(kind, "deploy_artifact_for_synergy");
    assert!(
        delta > 0.0 && delta <= 0.3,
        "expected a nudge-band delta, got {delta}"
    );
}

#[test]
fn non_artifact_spell_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 3, vec![CoreType::Sorcery]);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8);
    let ctx = PolicyContext {
        state: &state,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &context,
        cast_facts: None,
    };

    let (delta, kind) = delta_of(ArtifactSynergyPolicy.verdict(&ctx));
    assert_eq!(kind, "artifact_synergy_inert");
    assert_eq!(delta, 0.0);
}
