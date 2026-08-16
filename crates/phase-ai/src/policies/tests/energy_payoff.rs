//! Tests for `EnergyPayoffPolicy` — activation gate + verdict branches
//! including reserve-momentum scaling. No `#[cfg(test)]` in SOURCE files;
//! tests live here.
//!
//! The verdict path is exercised against a real `PolicyContext` built from a
//! two-player `GameState` with a castable energy object and a controlled
//! casting-player reserve, mirroring the `mill_payoff` policy-test shape.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter,
    TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::energy::{EnergyFeature, COMMITMENT_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::PolicyContext;
use crate::policies::payoff::{PayoffPolicy, ENERGY_PAYOFF};
use crate::policies::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);

fn policy() -> PayoffPolicy {
    PayoffPolicy::new(&ENERGY_PAYOFF)
}

// ─── fixtures ───────────────────────────────────────────────────────────────

fn features(commitment: f32) -> DeckFeatures {
    DeckFeatures {
        energy: EnergyFeature {
            producer_count: 20,
            sink_count: 12,
            payoff_count: 8,
            commitment,
        },
        ..DeckFeatures::default()
    }
}

fn ai_context(commitment: f32) -> (AiContext, AiConfig) {
    let config = AiConfig::default();
    let mut session = AiSession::empty();
    session.features.insert(AI, features(commitment));
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

/// A non-cast (PlayLand) candidate — drives the `energy_payoff_na` branch,
/// which short-circuits any candidate that is not a `CastSpell`.
fn land_candidate(object_id: ObjectId) -> CandidateAction {
    CandidateAction {
        action: GameAction::PlayLand {
            object_id,
            card_id: CardId(object_id.0),
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Land),
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

/// An Attune with Aether shape producer — grants energy. `is_producer = true`.
fn producer_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainEnergy {
            amount: QuantityExpr::Fixed { value: 1 },
        },
    )
}

/// A Bristling Hydra activation shape sink — pays energy for a non-energy
/// effect. `is_sink = true`, `is_producer = false`.
fn sink_ability() -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    ability.cost = Some(AbilityCost::PayEnergy {
        amount: QuantityExpr::Fixed { value: 1 },
    });
    ability
}

fn trigger_energy_sink() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Attacks).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PayCost {
            cost: AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            scale: None,
            payer: TargetFilter::Controller,
        },
    ))
}

/// A Divination shape draw spell — no energy interaction. Drives `energy_payoff_inert`.
fn draw_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
    )
}

/// Set the casting player's banked energy reserve — the momentum knob.
fn set_reserve(state: &mut GameState, reserve: u32) {
    state.players[AI.0 as usize].energy = reserve;
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

/// Unwrap a `Score` verdict into `(delta, reason)`; fail on `Reject`.
fn score_of(verdict: PolicyVerdict) -> (f64, PolicyReason) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (delta, reason),
        PolicyVerdict::Reject { reason } => panic!("unexpected Reject: {reason:?}"),
    }
}

/// Look up a typed fact by key, panicking if it is absent.
fn fact(reason: &PolicyReason, key: &str) -> i64 {
    reason
        .facts
        .iter()
        .find(|(k, _)| *k == key)
        .map(|(_, v)| *v)
        .unwrap_or_else(|| panic!("missing fact {key:?}; facts = {:?}", reason.facts))
}

// ─── identity ───────────────────────────────────────────────────────────────

#[test]
fn identity_is_energy_payoff() {
    assert_eq!(policy().id(), PolicyId::EnergyPayoff);
    assert!(policy().decision_kinds().contains(&DecisionKind::CastSpell));
    assert!(!policy().decision_kinds().contains(&DecisionKind::PlayLand));
    // Registry-membership guard: a dropped `Box::new(PayoffPolicy::new(&ENERGY_PAYOFF))`
    // registration line would otherwise be invisible to these direct-construction tests.
    assert!(PolicyRegistry::default().has_policy(PolicyId::EnergyPayoff));
}

// ─── activation gate ────────────────────────────────────────────────────────

#[test]
fn activation_below_floor_returns_none() {
    let mut features = DeckFeatures::default();
    features.energy.commitment = COMMITMENT_FLOOR - 0.01;
    let state = GameState::new_two_player(7);
    assert!(
        policy().activation(&features, &state, AI).is_none(),
        "commitment below floor must return None"
    );
}

#[test]
fn activation_at_floor_returns_some() {
    let mut features = DeckFeatures::default();
    features.energy.commitment = COMMITMENT_FLOOR;
    let state = GameState::new_two_player(7);
    let v = policy()
        .activation(&features, &state, AI)
        .expect("commitment at floor must activate");
    assert!(
        (v - COMMITMENT_FLOOR).abs() < 1e-6,
        "activation should equal commitment; got {v}"
    );
}

#[test]
fn activation_above_floor_returns_commitment() {
    let mut features = DeckFeatures::default();
    features.energy.commitment = 0.9;
    let state = GameState::new_two_player(7);
    let v = policy()
        .activation(&features, &state, AI)
        .expect("commitment 0.9 must activate");
    assert!(
        (v - 0.9).abs() < 1e-6,
        "activation should equal commitment 0.9; got {v}"
    );
}

// ─── verdict: non-cast → na ─────────────────────────────────────────────────

#[test]
fn verdict_non_cast_action_is_na() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 1, vec![CoreType::Land]);
    push_ability(&mut state, oid, producer_ability());

    // An energy-committed deck keeps the policy live, but a PlayLand candidate
    // is not a cast — the verdict short-circuits to `energy_payoff_na` before
    // any structural energy classification runs.
    let candidate = land_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, reason) = score_of(policy().verdict(&ctx));
    assert_eq!(reason.kind, "energy_payoff_na");
    assert!(
        delta.abs() < 1e-9,
        "na must be neutral (delta 0), got {delta}"
    );
}

// ─── verdict: non-energy spell → inert ──────────────────────────────────────

#[test]
fn verdict_non_energy_spell_is_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Sorcery]);
    push_ability(&mut state, oid, draw_ability());

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, reason) = score_of(policy().verdict(&ctx));
    assert_eq!(reason.kind, "energy_payoff_inert");
    assert!(
        delta.abs() < 1e-9,
        "inert must be neutral (delta 0), got {delta}"
    );
}

// ─── verdict: reserve-momentum scaling (×1 / ×2 / ×3) ───────────────────────

/// Cast a producer spell against a controlled reserve, returning the scored
/// `(delta, reason)`. Commitment is pinned at 1.0 so the policy is active.
fn scored_producer_verdict(reserve: u32) -> (f64, PolicyReason) {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 3, vec![CoreType::Sorcery]);
    push_ability(&mut state, oid, producer_ability());
    set_reserve(&mut state, reserve);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);
    score_of(policy().verdict(&ctx))
}

#[test]
fn verdict_building_reserve_is_x1() {
    // 0 reserve (< MID threshold) → ×1.0.
    let (delta, reason) = scored_producer_verdict(0);
    let expected = AiConfig::default().policy_penalties.energy_cast_bonus * 1.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "building momentum (0 reserve) must scale ×1 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "energy_cast");
    assert_eq!(fact(&reason, "energy_reserve"), 0);
    assert_eq!(fact(&reason, "urgency_x10"), 10);
    assert_eq!(fact(&reason, "is_producer"), 1);
    assert_eq!(fact(&reason, "is_sink"), 0);
}

#[test]
fn verdict_online_reserve_is_x2() {
    // 3 reserve (≥ MID, < HIGH) → ×2.0.
    let (delta, reason) = scored_producer_verdict(3);
    let expected = AiConfig::default().policy_penalties.energy_cast_bonus * 2.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "online momentum (3 reserve) must scale ×2 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "energy_cast");
    assert_eq!(fact(&reason, "energy_reserve"), 3);
    assert_eq!(fact(&reason, "urgency_x10"), 20);
}

#[test]
fn verdict_humming_reserve_is_x3() {
    // 6 reserve (≥ HIGH) → ×3.0.
    let (delta, reason) = scored_producer_verdict(6);
    let expected = AiConfig::default().policy_penalties.energy_cast_bonus * 3.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "humming momentum (6 reserve) must scale ×3 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "energy_cast");
    assert_eq!(fact(&reason, "energy_reserve"), 6);
    assert_eq!(fact(&reason, "urgency_x10"), 30);
}

// ─── verdict: sink body ─────────────────────────────────────────────────────

#[test]
fn verdict_sink_body_is_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 4, vec![CoreType::Creature]);
    push_ability(&mut state, oid, sink_ability());
    set_reserve(&mut state, 4);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, reason) = score_of(policy().verdict(&ctx));
    // 4 reserve → ×2.0.
    let expected = AiConfig::default().policy_penalties.energy_cast_bonus * 2.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "sink body at 4 reserve must scale ×2 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "energy_cast");
    assert_eq!(fact(&reason, "is_producer"), 0);
    assert_eq!(fact(&reason, "is_sink"), 1);
}

#[test]
fn verdict_trigger_energy_sink_body_is_scored() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 5, vec![CoreType::Creature]);
    push_trigger(&mut state, oid, trigger_energy_sink());
    set_reserve(&mut state, 4);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, reason) = score_of(policy().verdict(&ctx));
    let expected = AiConfig::default().policy_penalties.energy_cast_bonus * 2.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "trigger energy sink at 4 reserve must scale ×2 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "energy_cast");
    assert_eq!(fact(&reason, "is_producer"), 0);
    assert_eq!(fact(&reason, "is_sink"), 1);
}

// ─── momentum constants ─────────────────────────────────────────────────────

/// Compile-time guard: the reserve boundaries must be strictly ordered so the
/// three tiers (building / online / humming) partition the reserve space, and
/// the scales must be strictly increasing. Inverting any constant is a build
/// error. Surfaced as a `#[test]` for discoverability.
#[test]
fn momentum_constants_are_ordered() {
    use crate::policies::payoff::{
        MOMENTUM_SCALE_HIGH, MOMENTUM_SCALE_MID, MOMENTUM_SCALE_NORMAL, RESERVE_THRESHOLD_HIGH,
        RESERVE_THRESHOLD_MID,
    };
    const {
        assert!(
            RESERVE_THRESHOLD_MID < RESERVE_THRESHOLD_HIGH,
            "MID threshold must be strictly below HIGH"
        );
        assert!(
            MOMENTUM_SCALE_HIGH > MOMENTUM_SCALE_MID,
            "MOMENTUM_SCALE_HIGH must exceed MOMENTUM_SCALE_MID"
        );
        assert!(
            MOMENTUM_SCALE_MID > MOMENTUM_SCALE_NORMAL,
            "MOMENTUM_SCALE_MID must exceed MOMENTUM_SCALE_NORMAL"
        );
    }
}
