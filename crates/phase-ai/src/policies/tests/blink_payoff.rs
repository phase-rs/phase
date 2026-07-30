//! Tests for the blink-payoff spec (`PayoffPolicy::new(&BLINK_PAYOFF)`). Live in
//! a sibling test module (declared from `policies/tests/mod.rs`) so the generic
//! `policies/payoff.rs` stays implementation-only and SOURCE-classified.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId, TrackedSetId};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::blink::BlinkFeature;
use crate::features::DeckFeatures;
use crate::session::AiSession;

use super::super::context::PolicyContext;
use super::super::payoff::{PayoffPolicy, BLINK_PAYOFF};
use super::super::registry::{
    DecisionKind, PolicyId, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};

const AI: PlayerId = PlayerId(0);

fn policy() -> PayoffPolicy {
    PayoffPolicy::new(&BLINK_PAYOFF)
}

fn features(commitment: f32, flicker_count: u32, etb_payoff_count: u32) -> DeckFeatures {
    DeckFeatures {
        blink: BlinkFeature {
            flicker_count,
            etb_payoff_count,
            commitment,
        },
        ..DeckFeatures::default()
    }
}

fn ai_context(commitment: f32, flicker_count: u32, etb_payoff_count: u32) -> (AiContext, AiConfig) {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session
        .features
        .insert(AI, features(commitment, flicker_count, etb_payoff_count));
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

fn spell_object(state: &mut GameState, idx: u64, core: Vec<CoreType>) -> ObjectId {
    let oid = create_object(state, CardId(idx), AI, format!("Spell {idx}"), Zone::Stack);
    state.objects.get_mut(&oid).unwrap().card_types = CardType {
        supertypes: Vec::new(),
        core_types: core,
        subtypes: Vec::new(),
    };
    oid
}

fn push_ability(state: &mut GameState, oid: ObjectId, ability: AbilityDefinition) {
    Arc::make_mut(&mut state.objects.get_mut(&oid).unwrap().abilities).push(ability);
}

fn push_trigger(state: &mut GameState, oid: ObjectId, trigger: TriggerDefinition) {
    state
        .objects
        .get_mut(&oid)
        .unwrap()
        .trigger_definitions
        .push(trigger);
}

fn change_zone(destination: Zone, target: TargetFilter) -> Effect {
    Effect::ChangeZone {
        origin: None,
        destination,
        target,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

/// An Ephemerate-shape flicker: exile a friendly creature, then return it.
fn flicker_ability() -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Spell,
        change_zone(
            Zone::Exile,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        ),
    );
    ability.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        change_zone(
            Zone::Battlefield,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
        ),
    )));
    ability
}

/// A Mulldrifter-shape self-ETB value trigger.
fn value_etb_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .destination(Zone::Battlefield)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
        ))
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
    assert_eq!(policy().id(), PolicyId::BlinkPayoff);
    assert!(policy().decision_kinds().contains(&DecisionKind::CastSpell));
    // Registry-membership guard: a dropped `Box::new(PayoffPolicy::new(&BLINK_PAYOFF))`
    // registration line would otherwise be invisible to these direct-construction tests.
    assert!(PolicyRegistry::default().has_policy(PolicyId::BlinkPayoff));
}

// ─── activation gate ─────────────────────────────────────────────────────────

#[test]
fn opts_out_with_no_flicker() {
    let features = features(0.9, 0, 14);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_out_with_no_payoff() {
    let features = features(0.9, 8, 0);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_out_below_commitment_floor() {
    let features = features(0.1, 8, 14);
    let state = GameState::new_two_player(7);
    assert!(policy().activation(&features, &state, AI).is_none());
}

#[test]
fn opts_in_with_flicker_and_payoff_above_floor() {
    let features = features(0.6, 8, 14);
    let state = GameState::new_two_player(7);
    assert_eq!(policy().activation(&features, &state, AI), Some(0.6));
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn deploy_flicker_engine_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 1, vec![CoreType::Instant]);
    push_ability(&mut state, oid, flicker_ability());

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 8, 14);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "deploy_flicker_engine");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
    // Value-identity: exact ported `deploy_flicker_engine_bonus` (tier 1).
    assert!(
        (delta
            - AiConfig::default()
                .policy_penalties
                .deploy_flicker_engine_bonus)
            .abs()
            < 1e-9,
        "delta must equal the exact ported deploy_flicker_engine_bonus; got {delta}"
    );
}

#[test]
fn etb_payoff_cast_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Creature]);
    push_trigger(&mut state, oid, value_etb_trigger());

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 8, 14);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "etb_payoff_cast");
    assert!(delta > 0.0, "expected a positive delta, got {delta}");
    // Value-identity: exact ported `etb_payoff_cast_bonus` (tier 2).
    assert!(
        (delta - AiConfig::default().policy_penalties.etb_payoff_cast_bonus).abs() < 1e-9,
        "delta must equal the exact ported etb_payoff_cast_bonus; got {delta}"
    );
}

#[test]
fn non_blink_spell_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 3, vec![CoreType::Sorcery]);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(0.8, 8, 14);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, kind) = delta_of(policy().verdict(&ctx));
    assert_eq!(kind, "blink_payoff_inert");
    assert_eq!(delta, 0.0);
}
