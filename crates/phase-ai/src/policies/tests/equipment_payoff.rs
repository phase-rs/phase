//! Tests for the equipment-payoff spec (`PayoffPolicy::new(&EQUIPMENT_PAYOFF)`).
//! Live in a sibling test module (declared from `policies/tests/mod.rs`) so the
//! generic `policies/payoff.rs` stays implementation-only and SOURCE-classified.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, SearchSelectionConstraint, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::equipment::EquipmentFeature;
use crate::features::DeckFeatures;
use crate::session::AiSession;

use super::super::context::PolicyContext;
use super::super::payoff::{PayoffPolicy, EQUIPMENT_PAYOFF};
use super::super::registry::{
    DecisionKind, PolicyId, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};

const AI: PlayerId = PlayerId(0);

fn policy() -> PayoffPolicy {
    PayoffPolicy::new(&EQUIPMENT_PAYOFF)
}

fn features(commitment: f32, equipment_count: u32, payoff_count: u32) -> DeckFeatures {
    DeckFeatures {
        equipment: EquipmentFeature {
            equipment_count,
            payoff_count,
            commitment,
        },
        ..DeckFeatures::default()
    }
}

fn ai_context(commitment: f32, equipment_count: u32, payoff_count: u32) -> (AiContext, AiConfig) {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session
        .features
        .insert(AI, features(commitment, equipment_count, payoff_count));
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
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
    }
}

fn spell_object(
    state: &mut GameState,
    idx: u64,
    core: Vec<CoreType>,
    subtypes: Vec<&str>,
) -> ObjectId {
    let oid = create_object(state, CardId(idx), AI, format!("Spell {idx}"), Zone::Stack);
    state.objects.get_mut(&oid).unwrap().card_types = CardType {
        supertypes: Vec::new(),
        core_types: core,
        subtypes: subtypes.into_iter().map(String::from).collect(),
    };
    oid
}

fn push_ability(state: &mut GameState, oid: ObjectId, ability: AbilityDefinition) {
    Arc::make_mut(&mut state.objects.get_mut(&oid).unwrap().abilities).push(ability);
}

fn search_equipment_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::SearchLibrary {
            source_zones: vec![Zone::Library],
            filter: TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                "Equipment".to_string(),
            ))),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: true,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
        },
    )
}

fn ctx<'a>(
    state: &'a GameState,
    candidate: &'a CandidateAction,
    decision: &'a AiDecisionContext,
    context: &'a AiContext,
    config: &'a AiConfig,
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

fn delta_of(verdict: PolicyVerdict) -> (f64, String) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (delta, reason.kind.to_string()),
        PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
    }
}

// ─── identity ────────────────────────────────────────────────────────────────

#[test]
fn policy_identity() {
    assert_eq!(policy().id(), PolicyId::EquipmentPayoff);
    assert!(policy().decision_kinds().contains(&DecisionKind::CastSpell));
    // Registry-membership guard: a dropped `Box::new(PayoffPolicy::new(&EQUIPMENT_PAYOFF))`
    // registration line would otherwise be invisible to these direct-construction tests.
    assert!(PolicyRegistry::default().has_policy(PolicyId::EquipmentPayoff));
}

// ─── activation gate ─────────────────────────────────────────────────────────

#[test]
fn opts_out_with_no_equipment() {
    let features = features(0.9, 0, 6);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_out_with_no_payoff() {
    let features = features(0.9, 12, 0);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_out_below_commitment_floor() {
    let features = features(0.1, 12, 6);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_in_with_equipment_and_payoff_above_floor() {
    let features = features(0.6, 12, 6);
    let state = GameState::new_two_player(7);
    assert_eq!(policy().activation(&features, &state, AI), Some(0.6));
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn deploy_equipment_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 1, vec![CoreType::Artifact], vec!["Equipment"]);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 12, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "deploy_equipment_for_payoff");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
    // Value-identity: exact ported `deploy_equipment_bonus` (tier 1).
    assert!(
        (delta - AiConfig::default().policy_penalties.deploy_equipment_bonus).abs() < 1e-9,
        "delta must equal the exact ported deploy_equipment_bonus; got {delta}"
    );
}

#[test]
fn equipment_payoff_cast_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Creature], vec![]);
    push_ability(&mut state, oid, search_equipment_ability());

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 12, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "equipment_payoff_cast");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
    // Value-identity: exact ported `equipment_payoff_cast_bonus` (tier 2).
    assert!(
        (delta
            - AiConfig::default()
                .policy_penalties
                .equipment_payoff_cast_bonus)
            .abs()
            < 1e-9,
        "delta must equal the exact ported equipment_payoff_cast_bonus; got {delta}"
    );
}

#[test]
fn non_equipment_spell_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 3, vec![CoreType::Sorcery], vec![]);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 12, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "equipment_payoff_inert");
    assert_eq!(delta, 0.0);
}
