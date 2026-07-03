//! Tests for the reanimator-payoff spec
//! (`PayoffPolicy::new(&REANIMATOR_PAYOFF)`). Live in a sibling test module
//! (declared from `policies/tests/mod.rs`) so the generic `policies/payoff.rs`
//! stays implementation-only and SOURCE-classified.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, ControllerRef,
    DiscardSelfScope, Effect, QuantityExpr, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::reanimator::ReanimatorFeature;
use crate::features::DeckFeatures;
use crate::session::AiSession;

use super::super::context::PolicyContext;
use super::super::payoff::{PayoffPolicy, REANIMATOR_PAYOFF};
use super::super::registry::{
    DecisionKind, PolicyId, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};

const AI: PlayerId = PlayerId(0);

fn policy() -> PayoffPolicy {
    PayoffPolicy::new(&REANIMATOR_PAYOFF)
}

fn features(commitment: f32, reanimation_count: u32, target_count: u32) -> DeckFeatures {
    DeckFeatures {
        reanimator: ReanimatorFeature {
            reanimation_count,
            enabler_count: 6,
            target_count,
            commitment,
        },
        ..DeckFeatures::default()
    }
}

fn ai_context(commitment: f32, reanimation_count: u32, target_count: u32) -> (AiContext, AiConfig) {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session
        .features
        .insert(AI, features(commitment, reanimation_count, target_count));
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

fn reanimation_effect(target: TargetFilter) -> Effect {
    Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Battlefield,
        target,
        owner_library: false,
        enter_transformed: false,
        enters_under: Some(ControllerRef::You),
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: Vec::new(),
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

fn push_ability(state: &mut GameState, oid: ObjectId, ability: AbilityDefinition) {
    Arc::make_mut(&mut state.objects.get_mut(&oid).unwrap().abilities).push(ability);
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
    assert_eq!(policy().id(), PolicyId::ReanimatorPayoff);
    assert!(policy().decision_kinds().contains(&DecisionKind::CastSpell));
    // Registry-membership guard: a dropped `Box::new(PayoffPolicy::new(&REANIMATOR_PAYOFF))`
    // registration line would otherwise be invisible to these direct-construction tests.
    assert!(PolicyRegistry::default().has_policy(PolicyId::ReanimatorPayoff));
}

// ─── activation gate ─────────────────────────────────────────────────────────

#[test]
fn opts_out_with_no_reanimation_even_at_high_commitment() {
    let features = features(0.9, 0, 6);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_out_with_no_target() {
    let features = features(0.9, 6, 0);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_out_below_commitment_floor() {
    let features = features(0.1, 6, 6);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_in_with_payoff_and_target_above_floor() {
    let features = features(0.6, 6, 6);
    let state = GameState::new_two_player(7);
    assert_eq!(policy().activation(&features, &state, AI), Some(0.6));
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn reanimation_spell_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 1, vec![CoreType::Sorcery]);
    push_ability(
        &mut state,
        oid,
        AbilityDefinition::new(
            AbilityKind::Spell,
            reanimation_effect(TargetFilter::Typed(TypedFilter::creature())),
        ),
    );

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 6, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "reanimation_cast_for_payoff");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
    // Value-identity: exact ported `reanimation_cast_bonus` (tier 1).
    assert!(
        (delta - AiConfig::default().policy_penalties.reanimation_cast_bonus).abs() < 1e-9,
        "delta must equal the exact ported reanimation_cast_bonus; got {delta}"
    );
}

#[test]
fn trigger_borne_reanimation_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Creature]);
    state
        .objects
        .get_mut(&oid)
        .unwrap()
        .trigger_definitions
        .push(
            TriggerDefinition::new(TriggerMode::Attacks).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                reanimation_effect(TargetFilter::Typed(TypedFilter::new(TypeFilter::Subtype(
                    "Vehicle".to_string(),
                )))),
            )),
        );

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 6, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "reanimation_cast_for_payoff");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
}

#[test]
fn self_mill_enabler_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 3, vec![CoreType::Sorcery]);
    push_ability(
        &mut state,
        oid,
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Mill {
                count: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Controller,
                destination: Zone::Graveyard,
            },
        ),
    );

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 6, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "graveyard_enabler_for_reanimation");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
    // Value-identity: exact ported `graveyard_enabler_bonus` (tier 2).
    assert!(
        (delta - AiConfig::default().policy_penalties.graveyard_enabler_bonus).abs() < 1e-9,
        "delta must equal the exact ported graveyard_enabler_bonus; got {delta}"
    );
}

#[test]
fn discard_outlet_enabler_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 4, vec![CoreType::Creature]);
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    ability.cost = Some(AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: 1 },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    });
    push_ability(&mut state, oid, ability);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 6, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "graveyard_enabler_for_reanimation");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
}

#[test]
fn non_reanimator_spell_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 5, vec![CoreType::Sorcery]);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 6, 6);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "reanimator_payoff_inert");
    assert_eq!(delta, 0.0);
}
