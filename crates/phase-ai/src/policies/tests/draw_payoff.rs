//! Unit tests for `policies::draw_payoff` — CR 121.1 "whenever you draw" payoff
//! policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! Direct-`verdict` tests cover each branch; a registry-routed regression
//! exercises the production seam (registration + `CastSpell` routing).

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastVariantPaid, Effect, QuantityExpr, StaticDefinition,
    TargetFilter, TriggerCondition, TriggerConstraint, TriggerDefinition,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    CastPaymentMode, GameState, TargetSelectionConstraint, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::{ProhibitionScope, StaticMode};
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
    let mut st = GameState::new(FormatConfig::standard(), 2, 42);
    // Deliverable draws by default: seed the AI a non-empty library so a draw
    // actually puts a card into hand (CR 121.1). Empty-library behavior is
    // exercised explicitly by clearing this in the dedicated test.
    seed_library(&mut st, AI, 3);
    st
}

/// Puts `n` cards into `player`'s library so draws are deliverable.
fn seed_library(state: &mut GameState, player: PlayerId, n: usize) {
    for _ in 0..n {
        let card_id = CardId(state.next_object_id);
        create_object(
            state,
            card_id,
            player,
            "Library Card".to_string(),
            Zone::Library,
        );
    }
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

/// The Locust God shape: a no-target on-draw payoff (here, gain life) — always
/// resolves to an effect, so target legality never blocks it.
fn drawn_engine_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Drawn).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    ))
}

/// A Wizard-Class shape: a "whenever you draw, deal 3 damage to TARGET creature"
/// payoff whose value depends on a legal target existing (CR 603.3d).
fn drawn_targeted_engine_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Drawn).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 3 },
            target: TargetFilter::Typed(
                engine::types::ability::TypedFilter::default()
                    .with_type(engine::types::ability::TypeFilter::Creature),
            ),
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

/// An enchantment engine whose "whenever you draw" trigger targets a creature —
/// value depends on a legal target existing (CR 603.3d). Deliberately NOT a
/// creature itself, so with an empty board the trigger has no legal target.
fn targeted_engine(state: &mut GameState) -> ObjectId {
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
    obj.trigger_definitions
        .push(drawn_targeted_engine_trigger());
    id
}

/// Puts an opponent creature on the battlefield — a legal target for a
/// "target creature" trigger.
fn add_opponent_creature(state: &mut GameState) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        PlayerId(1),
        "Grizzly Bears".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
}

/// CR 603.3d: a mandatory-target "whenever you draw" engine with no legal target
/// on the board cannot resolve to an effect, so it is not a live payoff — the
/// engine's `hypothetical_trigger_fireable` target-legality preflight rejects it.
#[test]
fn targeted_engine_with_no_legal_target_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    targeted_engine(&mut st); // enchantment, empty board → no creature to hit
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// Control: once a legal creature target exists, the same targeted engine is live
/// and the draw is rewarded.
#[test]
fn targeted_engine_with_a_legal_target_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    targeted_engine(&mut st);
    add_opponent_creature(&mut st); // now the "target creature" trigger can resolve
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// A `MaxTimesPerTurn { max }` engine that has fired fewer than `max` times this
/// turn can still fire, so the draw is rewarded — the engine authority reads the
/// live `trigger_fire_counts_this_turn` ledger.
#[test]
fn max_times_per_turn_below_cap_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    let engine_id = engine_with_constraint(&mut st, TriggerConstraint::MaxTimesPerTurn { max: 2 });
    let key = {
        let obj = st.objects.get(&engine_id).unwrap();
        let entry = obj.trigger_definitions.iter_unchecked().next().unwrap();
        obj.trigger_definition_ref(entry)
    };
    st.trigger_fire_counts_this_turn.insert(key, 1); // 1 < 2 → can still fire

    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// Control: the same engine that has already fired `max` times this turn cannot
/// fire again, so the draw earns nothing.
#[test]
fn max_times_per_turn_at_cap_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    let engine_id = engine_with_constraint(&mut st, TriggerConstraint::MaxTimesPerTurn { max: 2 });
    let key = {
        let obj = st.objects.get(&engine_id).unwrap();
        let entry = obj.trigger_definitions.iter_unchecked().next().unwrap();
        obj.trigger_definition_ref(entry)
    };
    st.trigger_fire_counts_this_turn.insert(key, 2); // 2 == max → exhausted

    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// An `OnlyDuringYourMainPhase` engine is live during BOTH main phases — the
/// pre-combat and the post-combat main — so a draw in either is rewarded.
#[test]
fn only_during_your_main_phase_rewards_in_both_main_phases() {
    for phase in [Phase::PreCombatMain, Phase::PostCombatMain] {
        let config = AiConfig::default();
        let mut st = state();
        st.active_player = AI;
        st.phase = phase;
        engine_with_constraint(&mut st, TriggerConstraint::OnlyDuringYourMainPhase);
        let (oid, cid) = draw_spell(&mut st);
        let context = context(&config, session(0.9));
        let candidate = cast(oid, cid);
        let decision = priority_decision(&candidate);
        let (delta, reason) =
            score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
        assert_eq!(
            reason.kind, "draw_payoff_engine_active",
            "main phase {phase:?} should be live"
        );
        assert!(delta > 0.0, "main phase {phase:?} should reward");
    }
}

/// An `OnlyDuringOpponentsTurn` engine (a punish-on-their-draw payoff) is live
/// only while it is NOT your turn — a draw during the opponent's turn is
/// rewarded.
#[test]
fn only_during_opponents_turn_rewards_off_turn() {
    let config = AiConfig::default();
    let mut st = state();
    st.active_player = PlayerId(1); // the opponent's turn
    engine_with_constraint(&mut st, TriggerConstraint::OnlyDuringOpponentsTurn);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// Control: the same `OnlyDuringOpponentsTurn` engine on YOUR turn cannot fire,
/// so a draw earns nothing.
#[test]
fn only_during_opponents_turn_is_neutral_on_your_turn() {
    let config = AiConfig::default();
    let mut st = state();
    st.active_player = AI;
    engine_with_constraint(&mut st, TriggerConstraint::OnlyDuringOpponentsTurn);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// Negative for the main-phase timing: an `OnlyDuringYourMainPhase` engine during
/// a non-main phase (here, upkeep) cannot fire (CR 505.1), so an instant-speed
/// draw in that step earns nothing.
#[test]
fn only_during_your_main_phase_off_phase_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    st.active_player = AI;
    st.phase = Phase::Upkeep; // your turn, but not a main phase
    engine_with_constraint(&mut st, TriggerConstraint::OnlyDuringYourMainPhase);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// A permanent-spell creature whose self-ETB trigger draws you a card
/// (Elvish Visionary / Latchkey Faerie), with an optional intervening-if
/// `condition` — `qualifies_immediate_etb` picks it up as a `CastFacts`
/// immediate ETB.
fn etb_draw_spell(
    state: &mut GameState,
    condition: Option<TriggerCondition>,
) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        "Elvish Visionary".to_string(),
        Zone::Hand,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    let mut etb = TriggerDefinition::new(TriggerMode::ChangesZone).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ));
    etb.destination = Some(Zone::Battlefield);
    etb.valid_card = Some(TargetFilter::SelfRef);
    etb.condition = condition;
    obj.trigger_definitions.push(etb);
    (id, card_id)
}

/// CR 603.4: Latchkey Faerie's "if its prowl cost was paid, draw a card" ETB is
/// an intervening-if the AI cannot confirm at decision time, so its draw is NOT
/// credited — the cast is treated as a non-draw and earns nothing even with an
/// engine out.
#[test]
fn conditional_etb_draw_is_not_credited() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st); // a live engine is present…
    let (oid, cid) = etb_draw_spell(
        &mut st,
        Some(TriggerCondition::CastVariantPaid {
            variant: CastVariantPaid::Prowl,
        }),
    );
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    // …but the conditional ETB is not a confirmed draw, so no engine reward.
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// Control: Elvish Visionary's unconditional "when this enters, draw a card" ETB
/// IS a confirmed draw, so with an engine out the cast is rewarded.
#[test]
fn unconditional_etb_draw_is_credited() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let (oid, cid) = etb_draw_spell(&mut st, None);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// A battlefield permanent whose activated ability at index 0 runs `effect`, plus
/// its id for an `ActivateAbility` candidate.
fn activated_permanent(state: &mut GameState, effect: Effect) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        "Draw Engine".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(AbilityKind::Activated, effect));
    id
}

fn activate(source_id: ObjectId, ability_index: usize) -> CandidateAction {
    CandidateAction {
        action: GameAction::ActivateAbility {
            source_id,
            ability_index,
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
    }
}

/// An activated ability that draws you a card ("{T}: Draw a card") is a draw
/// action, so with an engine out it is rewarded — covering the policy's second
/// `DecisionKind::ActivateAbility` seam.
#[test]
fn activated_draw_ability_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let source_id = activated_permanent(
        &mut st,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let context = context(&config, session(0.9));
    let candidate = activate(source_id, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// Control: a non-draw activated ability (gain life) is not a draw action, so it
/// earns nothing regardless of the engine.
#[test]
fn activated_non_draw_ability_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    let source_id = activated_permanent(
        &mut st,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 2 },
            player: TargetFilter::Controller,
        },
    );
    let context = context(&config, session(0.9));
    let candidate = activate(source_id, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

// ─── source-sensitive constraint: AtClassLevel (CR 716) ──────────────────────

/// A Class-enchantment engine at `class_level` whose level-gated
/// "whenever you draw" payoff fires only while the Class is at `required_level`
/// (CR 716). The engine authority reads the level from the source context.
fn class_engine(state: &mut GameState, class_level: u8, required_level: u8) -> ObjectId {
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
    obj.class_level = Some(class_level);
    obj.trigger_definitions
        .push(
            drawn_engine_trigger().constraint(TriggerConstraint::AtClassLevel {
                level: required_level,
            }),
        );
    id
}

/// CR 716: an `AtClassLevel` payoff at the required level is live — the shared
/// hypothetical authority passes the source context, so the class level is read
/// correctly rather than treated as absent.
#[test]
fn at_class_level_engine_at_required_level_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    class_engine(&mut st, 2, 2); // at level 2, needs level 2
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// Control: the same Class engine at a DIFFERENT level cannot fire its
/// level-gated payoff, so the draw earns nothing.
#[test]
fn at_class_level_engine_at_wrong_level_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    class_engine(&mut st, 1, 2); // at level 1, but the payoff needs level 2
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

// ─── draw-delivery gate (CR 121.1 / CR 704.5b) ───────────────────────────────

/// Puts a permanent carrying a static that restricts drawing (Spirit of the
/// Labyrinth / Narset shape) on the battlefield, scoped to `who`.
fn add_draw_restricting_static(state: &mut GameState, mode: StaticMode) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        PlayerId(1),
        "Draw Hoser".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.static_definitions.push(StaticDefinition::new(mode));
}

fn set_cards_drawn_this_turn(state: &mut GameState, player: PlayerId, n: u32) {
    state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .cards_drawn_this_turn = n;
}

/// CR 121.1: under a `CantDraw` static the draw produces no `CardDrawn` event, so
/// the "whenever you draw" engine never fires — the delivery gate makes it a
/// no-op and the bonus is withheld even with the engine on the battlefield.
#[test]
fn cant_draw_static_makes_the_draw_a_no_op() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_restricting_static(
        &mut st,
        StaticMode::CantDraw {
            who: ProhibitionScope::AllPlayers,
        },
    );
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// CR 101.2: with a `PerTurnDrawLimit` already exhausted this turn, the extra
/// draw draws nothing, so no engine fires and the bonus is withheld.
#[test]
fn exhausted_per_turn_draw_limit_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_restricting_static(
        &mut st,
        StaticMode::PerTurnDrawLimit {
            who: ProhibitionScope::AllPlayers,
            max: 1,
        },
    );
    set_cards_drawn_this_turn(&mut st, AI, 1); // already at the cap
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// Control: the same per-turn limit with headroom left still lets a draw through,
/// so the engine is rewarded.
#[test]
fn per_turn_draw_limit_with_headroom_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_restricting_static(
        &mut st,
        StaticMode::PerTurnDrawLimit {
            who: ProhibitionScope::AllPlayers,
            max: 1,
        },
    );
    set_cards_drawn_this_turn(&mut st, AI, 0); // one draw still allowed
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// CR 704.5b: with an empty library, a "draw a card" only records an attempted
/// draw (a state-based loss) and puts no card into hand — no `CardDrawn` event,
/// so the engine never fires. The delivery preflight withholds the bonus.
#[test]
fn empty_library_draw_is_a_no_op() {
    let config = AiConfig::default();
    let mut st = state();
    st.players
        .iter_mut()
        .find(|p| p.id == AI)
        .unwrap()
        .library
        .clear(); // empty deck
    engine_on_battlefield(&mut st);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// Control: with cards left in the library the draw is deliverable (CR 121.1),
/// so the engine is rewarded. (The default `state()` seeds a non-empty library.)
#[test]
fn nonempty_library_draw_rewards() {
    let config = AiConfig::default();
    let mut st = state(); // seeded library
    engine_on_battlefield(&mut st);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

// ─── multi-target engine legality (CR 603.3d) ────────────────────────────────

/// A creature `TargetFilter`.
fn creature_filter() -> TargetFilter {
    TargetFilter::Typed(
        engine::types::ability::TypedFilter::default()
            .with_type(engine::types::ability::TypeFilter::Creature),
    )
}

/// A two-target payoff: "whenever you draw, exchange control of two target
/// permanents". A multi-target mandatory execute the cheap single-slot check
/// can't decide, so the engine authority must consult the full legal-assignment
/// solver (CR 603.3d). Enchantment engine, so an empty board has nothing to hit.
fn two_target_engine(state: &mut GameState) -> ObjectId {
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
    obj.trigger_definitions
        .push(
            TriggerDefinition::new(TriggerMode::Drawn).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::ExchangeControl {
                    target_a: creature_filter(),
                    target_b: creature_filter(),
                },
            )),
        );
    id
}

/// CR 603.3d: a mandatory MULTI-target engine with no legal target assignment is
/// removed rather than producing an effect — the preflight's cheap single-slot
/// check returns "undecided" here, so it falls through to the full solver, which
/// finds no assignment and reports the engine not-live.
#[test]
fn multi_target_engine_with_no_legal_assignment_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    two_target_engine(&mut st); // empty board → no two permanents to exchange
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// Control: once two exchangeable permanents (one per player) exist, the full
/// solver finds a legal assignment and the multi-target engine is rewarded.
#[test]
fn multi_target_engine_with_a_legal_assignment_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    two_target_engine(&mut st);
    add_opponent_creature(&mut st); // opponent permanent
                                    // an AI-controlled creature so the exchange has two sides
    let card_id = CardId(st.next_object_id);
    let mine = create_object(&mut st, card_id, AI, "Bear".to_string(), Zone::Battlefield);
    st.objects
        .get_mut(&mine)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// Adds an AI-controlled creature to the battlefield.
fn add_ai_creature(state: &mut GameState) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Bear".to_string(), Zone::Battlefield);
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
}

/// A two-target "exchange control of two target permanents controlled by
/// DIFFERENT players" engine — the execute carries a
/// `DifferentObjectControllers` cross-target constraint (CR 115.1). The preflight
/// must honor that constraint, not just the per-slot filters.
fn constrained_two_target_engine(state: &mut GameState) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        ENGINE_NAME.to_string(),
        Zone::Battlefield,
    );
    let mut execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ExchangeControl {
            target_a: creature_filter(),
            target_b: creature_filter(),
        },
    );
    execute.target_constraints = vec![TargetSelectionConstraint::DifferentObjectControllers];
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.trigger_definitions
        .push(TriggerDefinition::new(TriggerMode::Drawn).execute(execute));
    id
}

/// CR 115.1 + CR 603.3d: two permanents controlled by the SAME player cannot
/// satisfy the engine's `DifferentObjectControllers` constraint, so the trigger
/// has no legal assignment and is not a live payoff.
#[test]
fn constrained_two_target_engine_same_controller_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    constrained_two_target_engine(&mut st);
    add_ai_creature(&mut st);
    add_ai_creature(&mut st); // both mine → different-controllers can't be met
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// Control: one permanent per player satisfies `DifferentObjectControllers`, so
/// the constrained engine is live and the draw is rewarded.
#[test]
fn constrained_two_target_engine_different_controllers_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    constrained_two_target_engine(&mut st);
    add_ai_creature(&mut st);
    add_opponent_creature(&mut st); // one each → constraint satisfiable
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// A "whenever you draw" trigger with NO execute resolves to a `TriggerNoExecute`
/// no-op — no payoff — so it is not a live engine.
#[test]
fn no_execute_engine_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(&mut st, Some(TriggerDefinition::new(TriggerMode::Drawn)));
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// A "whenever you draw" trigger whose execute is an unsupported
/// (`Effect::Unimplemented`) gap node produces no payoff, so it is not credited.
#[test]
fn unsupported_execute_engine_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(
        &mut st,
        Some(
            TriggerDefinition::new(TriggerMode::Drawn).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::unimplemented("draw_payoff_test_gap", "unsupported payoff"),
            )),
        ),
    );
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
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
