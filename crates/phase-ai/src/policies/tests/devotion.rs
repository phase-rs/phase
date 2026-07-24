//! Unit tests for `policies::devotion` — the CR 700.5 pip-density policy. No
//! `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! The `verdict` path runs against a real `PolicyContext` built over a
//! two-player `GameState`, mirroring the `graveyard_types` policy-test shape:
//! current devotion comes from real battlefield permanents (the `count_devotion`
//! authority) and the cast's pips from a real hand object's mana cost.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::devotion::{DevotionFeature, DEVOTION_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::devotion::*;
use crate::policies::registry::{PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);

fn config() -> AiConfig {
    AiConfig::default()
}

fn state() -> GameState {
    GameState::new(FormatConfig::standard(), 2, 42)
}

/// An `AiContext` whose cached devotion feature carries the given primary color
/// and god threshold, so `verdict` reads them the way it would in a real game.
fn context_with(
    config: &AiConfig,
    primary_color: Option<ManaColor>,
    thresholds: Vec<u32>,
) -> AiContext {
    let features = DeckFeatures {
        devotion: DevotionFeature {
            payoff_count: 8,
            primary_color,
            pip_count: 30,
            thresholds,
            commitment: 0.9,
            payoff_names: Vec::new(),
        },
        ..Default::default()
    };
    let mut session = AiSession::empty();
    session.features.insert(AI, features);
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;
    context
}

fn priority_decision() -> AiDecisionContext {
    AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: Vec::new(),
    }
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
        search_depth: SearchDepth::Root,
    }
}

/// Put a permanent on the AI's battlefield carrying `pips` colored symbols, so
/// `count_devotion` sees them.
fn battlefield_permanent(state: &mut GameState, idx: u64, pips: &[ManaCostShard]) {
    let oid = create_object(
        state,
        CardId(2000 + idx),
        AI,
        format!("Devout {idx}"),
        Zone::Battlefield,
    );
    let object = state.objects.get_mut(&oid).unwrap();
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Creature],
        subtypes: Vec::new(),
    };
    object.mana_cost = ManaCost::Cost {
        shards: pips.to_vec(),
        generic: 1,
    };
}

/// A hand object of `core` type with `pips` colored symbols — the cast candidate.
fn hand_card(state: &mut GameState, idx: u64, core: CoreType, pips: &[ManaCostShard]) -> ObjectId {
    let oid = create_object(state, CardId(idx), AI, format!("Cast {idx}"), Zone::Hand);
    let object = state.objects.get_mut(&oid).unwrap();
    object.card_id = CardId(oid.0);
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![core],
        subtypes: Vec::new(),
    };
    object.mana_cost = ManaCost::Cost {
        shards: pips.to_vec(),
        generic: 1,
    };
    oid
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

fn score_of(verdict: PolicyVerdict) -> (f64, PolicyReason) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (delta, reason),
        PolicyVerdict::Reject { reason } => panic!("unexpected Reject: {reason:?}"),
    }
}

const B: ManaCostShard = ManaCostShard::Black;
const R: ManaCostShard = ManaCostShard::Red;

// ─── activation ──────────────────────────────────────────────────────────────

#[test]
fn activation_opts_out_below_floor() {
    let mut features = DeckFeatures::default();
    features.devotion.commitment = DEVOTION_FLOOR - 0.01;
    assert!(DevotionPolicy.activation(&features, &state(), AI).is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let mut features = DeckFeatures::default();
    features.devotion.commitment = 0.9;
    assert_eq!(
        DevotionPolicy.activation(&features, &state(), AI),
        Some(0.9)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn verdict_scores_pips_added_when_no_threshold() {
    let config = config();
    let context = context_with(&config, Some(ManaColor::Black), Vec::new());
    let mut state = state();
    let oid = hand_card(&mut state, 1, CoreType::Creature, &[B, B]);
    let decision = priority_decision();
    let candidate = cast_candidate(oid);
    let (delta, reason) =
        score_of(DevotionPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "devotion_pip_progress");
    // Two black pips at the default 0.35/pip scalar.
    assert!((delta - 0.7).abs() < 1e-6, "expected 0.7, got {delta}");
}

#[test]
fn verdict_god_activation_when_cast_crosses_threshold() {
    let config = config();
    let context = context_with(&config, Some(ManaColor::Black), vec![5]);
    let mut state = state();
    // Four black pips already on board → devotion 4, one below the threshold.
    battlefield_permanent(&mut state, 1, &[B, B]);
    battlefield_permanent(&mut state, 2, &[B, B]);
    let oid = hand_card(&mut state, 1, CoreType::Enchantment, &[B]);
    let decision = priority_decision();
    let candidate = cast_candidate(oid);
    let (delta, reason) =
        score_of(DevotionPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "devotion_god_activation");
    // Crossing spike (2.5) + one pip (0.35).
    assert!(
        delta > 2.5,
        "crossing must exceed the god-activation floor, got {delta}"
    );
}

#[test]
fn verdict_below_threshold_without_crossing_is_pip_progress() {
    let config = config();
    let context = context_with(&config, Some(ManaColor::Black), vec![5]);
    let mut state = state();
    // Devotion 1; casting one more pip reaches 2, still short of 5.
    battlefield_permanent(&mut state, 1, &[B]);
    let oid = hand_card(&mut state, 1, CoreType::Creature, &[B]);
    let decision = priority_decision();
    let candidate = cast_candidate(oid);
    let (_, reason) =
        score_of(DevotionPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "devotion_pip_progress");
}

#[test]
fn verdict_off_color_cast_is_neutral() {
    let config = config();
    let context = context_with(&config, Some(ManaColor::Black), Vec::new());
    let mut state = state();
    let oid = hand_card(&mut state, 1, CoreType::Creature, &[R, R]);
    let decision = priority_decision();
    let candidate = cast_candidate(oid);
    let (delta, reason) =
        score_of(DevotionPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "devotion_off_color");
    assert_eq!(delta, 0.0);
}

/// CR 700.5: an instant contributes no devotion even with colored pips.
#[test]
fn verdict_instant_is_neutral() {
    let config = config();
    let context = context_with(&config, Some(ManaColor::Black), Vec::new());
    let mut state = state();
    let oid = hand_card(&mut state, 1, CoreType::Instant, &[B, B]);
    let decision = priority_decision();
    let candidate = cast_candidate(oid);
    let (delta, reason) =
        score_of(DevotionPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "devotion_na");
    assert_eq!(delta, 0.0);
}

/// [MED] The bug the review caught: with gods at 3 and 5 and devotion at 2, a
/// cast that reaches 3 turns on the smaller god. The old max-only logic scored
/// this as mere pip progress; it must now be a god activation.
#[test]
fn verdict_crossing_lower_threshold_activates_that_god() {
    let config = config();
    let context = context_with(&config, Some(ManaColor::Black), vec![3, 5]);
    let mut state = state();
    // Devotion 2 (one below the lower gate).
    battlefield_permanent(&mut state, 1, &[B, B]);
    let oid = hand_card(&mut state, 1, CoreType::Enchantment, &[B]);
    let decision = priority_decision();
    let candidate = cast_candidate(oid);
    let (_, reason) =
        score_of(DevotionPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "devotion_god_activation");
    assert!(
        reason
            .facts
            .iter()
            .any(|(k, v)| *k == "gods_activated" && *v == 1),
        "exactly one god crossed, got {:?}",
        reason.facts
    );
}

/// One cast can flip two gods at once when it clears both gates.
#[test]
fn verdict_crossing_two_thresholds_activates_both() {
    let config = config();
    let context = context_with(&config, Some(ManaColor::Black), vec![3, 5]);
    let mut state = state();
    // Devotion 2; a {B}{B}{B} cast reaches 5, clearing both the 3 and the 5 gate.
    battlefield_permanent(&mut state, 1, &[B, B]);
    let oid = hand_card(&mut state, 1, CoreType::Enchantment, &[B, B, B]);
    let decision = priority_decision();
    let candidate = cast_candidate(oid);
    let (_, reason) =
        score_of(DevotionPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "devotion_god_activation");
    assert!(
        reason
            .facts
            .iter()
            .any(|(k, v)| *k == "gods_activated" && *v == 2),
        "both gods crossed, got {:?}",
        reason.facts
    );
}
