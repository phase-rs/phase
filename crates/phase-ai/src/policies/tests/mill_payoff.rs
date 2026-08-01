//! Tests for `MillPayoffPolicy` — activation gate + verdict branches including
//! library-size urgency scaling. No `#[cfg(test)]` in SOURCE files; tests
//! live here.
//!
//! The verdict path is exercised against a real `PolicyContext` built from a
//! two-player `GameState` with a castable mill object and a controlled
//! opponent library size, mirroring the `blink_payoff` policy-test shape.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::mill::{MillFeature, COMMITMENT_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::PolicyContext;
use crate::policies::payoff::{PayoffPolicy, MILL_PAYOFF};
use crate::policies::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);
const OPPONENT: PlayerId = PlayerId(1);

fn policy() -> PayoffPolicy {
    PayoffPolicy::new(&MILL_PAYOFF)
}

// ─── fixtures ───────────────────────────────────────────────────────────────

fn features(commitment: f32) -> DeckFeatures {
    DeckFeatures {
        mill: MillFeature {
            mill_count: 20,
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

/// A non-cast (PlayLand) candidate — drives the `mill_payoff_na` branch,
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

/// A Tome Scour-shape opponent-mill spell — classifies as opponent-mill under
/// per-chain structural detection (`target: Player` ≠ `Controller | Any`).
fn opponent_mill_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Mill {
            count: QuantityExpr::Fixed { value: 5 },
            target: TargetFilter::Player,
            destination: Zone::Graveyard,
        },
    )
}

/// A Divination-shape draw spell — no mill effect, so a cast is `mill_payoff_inert`.
fn draw_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
    )
}

/// Set the opponent's library to `n` cards. Library size selects the urgency
/// tier in `MillPayoffPolicy::verdict` (CR 104.3c).
fn set_opponent_library(state: &mut GameState, n: usize) {
    state.players[OPPONENT.0 as usize].library = (0..n).map(|i| ObjectId(i as u64)).collect();
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
fn identity_is_mill_payoff() {
    assert_eq!(policy().id(), PolicyId::MillPayoff);
    assert!(policy().decision_kinds().contains(&DecisionKind::CastSpell));
    assert!(!policy().decision_kinds().contains(&DecisionKind::PlayLand));
    // Registry-membership guard: a dropped `Box::new(PayoffPolicy::new(&MILL_PAYOFF))`
    // registration line would otherwise be invisible to these direct-construction tests.
    assert!(PolicyRegistry::default().has_policy(PolicyId::MillPayoff));
}

// ─── activation gate ────────────────────────────────────────────────────────

#[test]
fn activation_below_floor_returns_none() {
    let mut features = DeckFeatures::default();
    features.mill.commitment = COMMITMENT_FLOOR - 0.01;
    let state = GameState::new_two_player(7);
    assert!(
        policy().activation(&features, &state, AI).is_none(),
        "commitment below floor must return None"
    );
}

#[test]
fn activation_at_floor_returns_some() {
    let mut features = DeckFeatures::default();
    features.mill.commitment = COMMITMENT_FLOOR;
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
    features.mill.commitment = 0.9;
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
    push_ability(&mut state, oid, opponent_mill_ability());

    // A mill-committed deck keeps the policy live, but a PlayLand candidate is
    // not a cast — the verdict short-circuits to `mill_payoff_na` before any
    // structural mill classification runs.
    let candidate = land_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, reason) = score_of(policy().verdict(&ctx));
    assert_eq!(reason.kind, "mill_payoff_na");
    assert!(
        delta.abs() < 1e-9,
        "na must be neutral (delta 0), got {delta}"
    );
}

// ─── verdict: non-mill spell → inert ────────────────────────────────────────

#[test]
fn verdict_non_mill_spell_is_inert() {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 2, vec![CoreType::Sorcery]);
    push_ability(&mut state, oid, draw_ability());

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);

    let (delta, reason) = score_of(policy().verdict(&ctx));
    assert_eq!(reason.kind, "mill_payoff_inert");
    assert!(
        delta.abs() < 1e-9,
        "inert must be neutral (delta 0), got {delta}"
    );
}

// ─── verdict: urgency scaling (×1 / ×2 / ×3) ────────────────────────────────

/// Cast a mill spell against a controlled opponent library size, returning the
/// scored `(delta, reason)`. Commitment is pinned at 1.0 so the policy is
/// active; the verdict's own scaling reads only the opponent's library.
fn scored_mill_verdict(opponent_library: usize) -> (f64, PolicyReason) {
    let mut state = GameState::new_two_player(7);
    let oid = spell_object(&mut state, 3, vec![CoreType::Sorcery]);
    push_ability(&mut state, oid, opponent_mill_ability());
    set_opponent_library(&mut state, opponent_library);

    let candidate = cast_candidate(oid);
    let decision = decision();
    let (context, config) = ai_context(1.0);
    let ctx = ctx(&state, &candidate, &decision, &context, &config);
    score_of(policy().verdict(&ctx))
}

#[test]
fn verdict_normal_library_is_x1() {
    // 20 cards (≥ ELEVATED threshold) → ×1.0.
    let (delta, reason) = scored_mill_verdict(20);
    let expected = AiConfig::default().policy_penalties.mill_cast_bonus * 1.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "normal urgency (20 cards) must scale ×1 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "mill_cast");
    assert_eq!(fact(&reason, "library_remaining"), 20);
    assert_eq!(fact(&reason, "urgency_x10"), 10);
}

#[test]
fn verdict_elevated_library_is_x2() {
    // 10 cards (< ELEVATED, ≥ URGENT) → ×2.0.
    let (delta, reason) = scored_mill_verdict(10);
    let expected = AiConfig::default().policy_penalties.mill_cast_bonus * 2.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "elevated urgency (10 cards) must scale ×2 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "mill_cast");
    assert_eq!(fact(&reason, "library_remaining"), 10);
    assert_eq!(fact(&reason, "urgency_x10"), 20);
}

#[test]
fn verdict_urgent_library_is_x3() {
    // 3 cards (< URGENT threshold) → ×3.0.
    let (delta, reason) = scored_mill_verdict(3);
    let expected = AiConfig::default().policy_penalties.mill_cast_bonus * 3.0;
    assert!(
        (delta - expected).abs() < 1e-9,
        "urgent urgency (3 cards) must scale ×3 (expected {expected}); got {delta}"
    );
    assert_eq!(reason.kind, "mill_cast");
    assert_eq!(fact(&reason, "library_remaining"), 3);
    assert_eq!(fact(&reason, "urgency_x10"), 30);
}

// ─── urgency constants ──────────────────────────────────────────────────────

/// Compile-time guard: the policy's library-size boundaries must be strictly
/// ordered so the three tiers (normal / elevated / urgent) partition the space
/// without overlap, and the scales must be strictly increasing. Each assert
/// lives in a `const {}` block, so inverting any constant is a build error
/// before the suite runs. Surfaced as a `#[test]` for discoverability.
#[test]
fn urgency_constants_are_ordered() {
    use crate::policies::payoff::{
        LIBRARY_THRESHOLD_ELEVATED, LIBRARY_THRESHOLD_URGENT, URGENCY_SCALE_HIGH,
        URGENCY_SCALE_MID, URGENCY_SCALE_NORMAL,
    };
    const {
        assert!(
            LIBRARY_THRESHOLD_URGENT < LIBRARY_THRESHOLD_ELEVATED,
            "URGENT threshold must be strictly below ELEVATED"
        );
        assert!(
            URGENCY_SCALE_HIGH > URGENCY_SCALE_MID,
            "URGENCY_SCALE_HIGH must exceed URGENCY_SCALE_MID"
        );
        assert!(
            URGENCY_SCALE_MID > URGENCY_SCALE_NORMAL,
            "URGENCY_SCALE_MID must exceed URGENCY_SCALE_NORMAL"
        );
    }
}
