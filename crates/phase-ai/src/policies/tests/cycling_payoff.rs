//! Unit tests for `policies::cycling_payoff` — CR 702.29c/d "cycling matters"
//! payoff policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! Direct-`verdict` tests cover each branch; a registry-routed regression
//! exercises the production seam (registration + `ActivateAbility` routing),
//! following the poison policy's pattern.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::{CyclingCost, Keyword};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::cycling::{CyclingFeature, CYCLING_PAYOFF_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::cycling_payoff::*;
use crate::policies::registry::{
    PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);
const ENGINE_NAME: &str = "Astral Drift";

fn state() -> GameState {
    GameState::new(FormatConfig::standard(), 2, 42)
}

/// A cyclable card in hand whose synthesized Cycling ability is the activation.
fn cycler(state: &mut GameState) -> ObjectId {
    let kw = Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(2)));
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Cyc".to_string(), Zone::Hand);
    let ability = engine::database::synthesis::cycling_ability_for_keyword(&kw)
        .expect("cycling keyword must synthesize an activated ability");
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    Arc::make_mut(&mut obj.abilities).push(ability);
    id
}

/// A permanent the AI controls, named `ENGINE_NAME`, that carries `trigger`
/// live `trigger_definitions` (or none when `trigger` is `None` — the name-only
/// impostor case).
fn permanent_with_trigger(state: &mut GameState, trigger: Option<TriggerDefinition>) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        ENGINE_NAME.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    if let Some(trigger) = trigger {
        obj.trigger_definitions.push(trigger);
    }
}

/// Astral Drift shape: a live "whenever you cycle or discard a card" engine.
fn engine_on_battlefield(state: &mut GameState) {
    permanent_with_trigger(state, Some(cycle_trigger(TargetFilter::Any)));
}

/// A controller-scoped `CycledOrDiscarded` trigger whose effect targets
/// `target` (use `TargetFilter::Controller` for a no-target payoff shape).
fn cycle_trigger(target: TargetFilter) -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::CycledOrDiscarded).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target,
        },
    ))
}

fn session(commitment: f32) -> AiSession {
    let features = DeckFeatures {
        cycling: CyclingFeature {
            source_count: 18,
            payoff_count: 4,
            commitment,
        },
        ..Default::default()
    };
    let mut session = AiSession::empty();
    session.features.insert(AI, features);
    session
}

fn context(config: &AiConfig, session: AiSession) -> AiContext {
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;
    context
}

fn activate(source_id: ObjectId) -> CandidateAction {
    CandidateAction {
        action: GameAction::ActivateAbility {
            source_id,
            ability_index: 0,
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
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

fn score_of(verdict: PolicyVerdict) -> (f64, PolicyReason) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (delta, reason),
        PolicyVerdict::Reject { reason } => panic!("unexpected Reject: {reason:?}"),
    }
}

// ─── activation ──────────────────────────────────────────────────────────────

#[test]
fn activation_opts_out_below_floor() {
    let mut features = DeckFeatures::default();
    features.cycling.commitment = CYCLING_PAYOFF_FLOOR - 0.01;
    assert!(CyclingPayoffPolicy
        .activation(&features, &state(), AI)
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let mut features = DeckFeatures::default();
    features.cycling.commitment = 0.9;
    assert_eq!(
        CyclingPayoffPolicy.activation(&features, &state(), AI),
        Some(0.9)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn rewards_cycling_with_an_active_engine() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let source = cycler(&mut st);
    let context = context(&config, session(0.9));
    let candidate = activate(source);
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: vec![candidate.clone()],
    };
    let (delta, reason) =
        score_of(CyclingPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "cycling_payoff_engine_active");
    assert!(
        delta > 0.0,
        "cycling into an engine must be rewarded, got {delta}"
    );
}

#[test]
fn neutral_without_an_engine_on_board() {
    let config = AiConfig::default();
    let mut st = state();
    // Engine known to the deck, but none is on the battlefield.
    let source = cycler(&mut st);
    let context = context(&config, session(0.9));
    let candidate = activate(source);
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: vec![candidate.clone()],
    };
    let (delta, reason) =
        score_of(CyclingPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "cycling_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

#[test]
fn neutral_for_a_non_cycling_action() {
    let config = AiConfig::default();
    let st = state();
    let context = context(&config, session(0.9));
    // A cast candidate is not an activated ability at all.
    let candidate = CandidateAction {
        action: GameAction::ActivateAbility {
            source_id: ObjectId(999),
            ability_index: 0,
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
    };
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: vec![candidate.clone()],
    };
    let (delta, reason) =
        score_of(CyclingPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "cycling_payoff_na");
    assert_eq!(delta, 0.0);
}

/// [MED review] A permanent that merely SHARES the engine's name but carries no
/// live cycle trigger must not be rewarded — detection is structural over
/// `trigger_definitions`, not name-based.
#[test]
fn name_only_impostor_without_a_live_trigger_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(&mut st, None); // named "Astral Drift", no trigger
    let source = cycler(&mut st);
    let context = context(&config, session(0.9));
    let candidate = activate(source);
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: vec![candidate.clone()],
    };
    let (delta, reason) =
        score_of(CyclingPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "cycling_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// A no-target payoff (Drannith Stinger shape — its on-cycle effect hits each
/// opponent, choosing nothing) must still be rewarded; the policy checks the
/// live trigger, not target legality.
#[test]
fn no_target_payoff_still_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(&mut st, Some(cycle_trigger(TargetFilter::Controller)));
    let source = cycler(&mut st);
    let context = context(&config, session(0.9));
    let candidate = activate(source);
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: vec![candidate.clone()],
    };
    let (delta, reason) =
        score_of(CyclingPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "cycling_payoff_engine_active");
    assert!(
        delta > 0.0,
        "no-target payoff must still reward, got {delta}"
    );
}

// ─── production seam (registry routing) ─────────────────────────────────────

#[test]
fn registry_registers_the_policy() {
    assert!(PolicyRegistry::default().has_policy(PolicyId::CyclingPayoff));
}

/// End-to-end: an `ActivateAbility` on a cycler classifies to
/// `DecisionKind::ActivateAbility`, the policy declares that kind and clears its
/// activation floor, and the engine-active reward comes out of the registry.
#[test]
fn registry_routes_cycling_activation_to_the_policy() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let source = cycler(&mut st);
    let context = context(&config, session(0.9));
    let candidate = activate(source);
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: vec![candidate.clone()],
    };
    let (delta, reason) = PolicyRegistry::default()
        .verdicts(&ctx(&st, &candidate, &decision, &context, &config))
        .into_iter()
        .find(|(id, _)| *id == PolicyId::CyclingPayoff)
        .map(|(_, v)| score_of(v))
        .expect("the cycling activation must reach the policy through the registry");
    assert_eq!(reason.kind, "cycling_payoff_engine_active");
    assert!(delta > 0.0, "routed reward must be positive, got {delta}");
}
