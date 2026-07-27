//! Unit tests for `policies::draw_payoff` — CR 121.1 "whenever you draw" payoff
//! policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! Direct-`verdict` tests cover each branch; a registry-routed regression
//! exercises the production seam (registration + `CastSpell` routing).

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerConstraint,
    TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::draw_matters::{DrawMattersFeature, DRAW_MATTERS_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::draw_payoff::*;
use crate::policies::registry::{
    PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);
const ENGINE_NAME: &str = "The Locust God";

fn state() -> GameState {
    GameState::new(FormatConfig::standard(), 2, 42)
}

/// A hand spell that draws YOU cards on resolution (an `AbilityKind::Spell`
/// Draw effect), plus its `(object_id, card_id)` for the cast candidate.
fn spell(state: &mut GameState, effect: Effect) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Spell".to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Sorcery);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(AbilityKind::Spell, effect));
    (id, card_id)
}

fn draw_spell(state: &mut GameState) -> (ObjectId, CardId) {
    spell(
        state,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
    )
}

/// A permanent the AI controls, named `ENGINE_NAME`, carrying `trigger` live
/// `trigger_definitions` (or none — the name-only impostor case).
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
    obj.card_types.core_types.push(CoreType::Creature);
    if let Some(trigger) = trigger {
        obj.trigger_definitions.push(trigger);
    }
}

fn drawn_engine_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Drawn).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Opponent,
            damage_source: None,
            excess: None,
        },
    ))
}

fn engine_on_battlefield(state: &mut GameState) {
    permanent_with_trigger(state, Some(drawn_engine_trigger()));
}

fn session(commitment: f32) -> AiSession {
    let features = DeckFeatures {
        draw_matters: DrawMattersFeature {
            source_count: 20,
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

fn cast(object_id: ObjectId, card_id: CardId) -> CandidateAction {
    CandidateAction {
        action: GameAction::CastSpell {
            object_id,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::default(),
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
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

fn priority_decision(candidate: &CandidateAction) -> AiDecisionContext {
    AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: vec![candidate.clone()],
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
    features.draw_matters.commitment = DRAW_MATTERS_FLOOR - 0.01;
    assert!(DrawPayoffPolicy
        .activation(&features, &state(), AI)
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let mut features = DeckFeatures::default();
    features.draw_matters.commitment = 0.9;
    assert_eq!(
        DrawPayoffPolicy.activation(&features, &state(), AI),
        Some(0.9)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn rewards_drawing_with_an_active_engine() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(
        delta > 0.0,
        "drawing into an engine must be rewarded, got {delta}"
    );
}

#[test]
fn neutral_without_an_engine_on_board() {
    let config = AiConfig::default();
    let mut st = state();
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

#[test]
fn neutral_for_a_non_draw_spell() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    // A burn spell draws nothing.
    let (oid, cid) = spell(
        &mut st,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    );
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// A permanent that merely shares the engine's name but carries no live draw
/// trigger must not be rewarded — detection is structural over
/// `trigger_definitions`, not name-based.
#[test]
fn name_only_impostor_without_a_live_trigger_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(&mut st, None);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// A once-per-turn "whenever you draw" engine (Chulane / Valiant-Rescuer shape).
fn once_per_turn_engine(state: &mut GameState) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        ENGINE_NAME.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.trigger_definitions
        .push(drawn_engine_trigger().constraint(TriggerConstraint::OncePerTurn));
    id
}

/// [MED review parity with #6683] A once-per-turn engine that has already fired
/// this turn cannot fire again (CR 603.4), so drawing again earns nothing — the
/// policy consults the fired-trigger ledger, not just the trigger shape.
#[test]
fn rate_limited_engine_already_fired_this_turn_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    let engine_id = once_per_turn_engine(&mut st);
    let key = {
        let obj = st.objects.get(&engine_id).unwrap();
        let entry = obj.trigger_definitions.iter_unchecked().next().unwrap();
        obj.trigger_definition_ref(entry)
    };
    st.triggers_fired_this_turn.insert(key);

    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// Control: the same once-per-turn engine that has NOT fired yet still rewards.
#[test]
fn rate_limited_engine_not_yet_fired_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    once_per_turn_engine(&mut st);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0, "an unfired once-per-turn engine still rewards");
}

/// [MED review] A modal "choose one — deal 3 damage; OR draw a card" spell (the
/// draw lives in the `else` branch) is scored before its mode is chosen, so the
/// runtime scan (Unconditional) must NOT credit it a draw.
fn modal_burn_or_draw_spell(state: &mut GameState) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Modal".to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Instant);
    let mut ability = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
    );
    ability.else_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )));
    Arc::make_mut(&mut obj.abilities).push(ability);
    (id, card_id)
}

#[test]
fn modal_draw_not_credited_before_mode_selected() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let (oid, cid) = modal_burn_or_draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// An engine trigger with a per-game constraint on the AI's own permanent.
fn engine_with_constraint(state: &mut GameState, constraint: TriggerConstraint) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        ENGINE_NAME.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.trigger_definitions
        .push(drawn_engine_trigger().constraint(constraint));
    id
}

#[test]
fn once_per_game_engine_already_fired_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    let engine_id = engine_with_constraint(&mut st, TriggerConstraint::OncePerGame);
    let key = {
        let obj = st.objects.get(&engine_id).unwrap();
        let entry = obj.trigger_definitions.iter_unchecked().next().unwrap();
        obj.trigger_definition_ref(entry)
    };
    st.triggers_fired_this_game.insert(key);

    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

#[test]
fn once_per_game_engine_unfired_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    engine_with_constraint(&mut st, TriggerConstraint::OncePerGame);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// [MED review] An `OnlyDuringYourTurn` engine on the opponent's turn cannot
/// fire, so an instant-speed draw during their turn earns nothing.
#[test]
fn only_during_your_turn_engine_is_neutral_off_turn() {
    let config = AiConfig::default();
    let mut st = state();
    st.active_player = PlayerId(1); // the opponent's turn
    engine_with_constraint(&mut st, TriggerConstraint::OnlyDuringYourTurn);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// Control: the same `OnlyDuringYourTurn` engine on YOUR turn still rewards.
#[test]
fn only_during_your_turn_engine_rewards_on_your_turn() {
    let config = AiConfig::default();
    let mut st = state();
    st.active_player = AI;
    engine_with_constraint(&mut st, TriggerConstraint::OnlyDuringYourTurn);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

// ─── production seam (registry routing) ─────────────────────────────────────

#[test]
fn registry_registers_the_policy() {
    assert!(PolicyRegistry::default().has_policy(PolicyId::DrawPayoff));
}

/// End-to-end: casting a draw spell classifies to `DecisionKind::CastSpell`, the
/// policy declares that kind and clears its activation floor, and the
/// engine-active reward comes out of the registry.
#[test]
fn registry_routes_draw_cast_to_the_policy() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) = PolicyRegistry::default()
        .verdicts(&ctx(&st, &candidate, &decision, &context, &config))
        .into_iter()
        .find(|(id, _)| *id == PolicyId::DrawPayoff)
        .map(|(_, v)| score_of(v))
        .expect("the draw cast must reach the policy through the registry");
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0, "routed reward must be positive, got {delta}");
}
