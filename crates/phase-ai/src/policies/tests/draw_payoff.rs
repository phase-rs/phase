//! Unit tests for `policies::draw_payoff` — CR 121.1 "whenever you draw" payoff
//! policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! Direct-`verdict` tests cover each branch; a registry-routed regression
//! exercises the production seam (registration + `CastSpell` routing).

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, CastVariantPaid, DrawReplacementScope, Effect, ModalChoice,
    QuantityExpr, QuantityModification, QuantityRef, ReplacementCondition, ReplacementDefinition,
    ReplacementMode, StaticDefinition, TargetFilter, TriggerCondition, TriggerConstraint,
    TriggerDefinition,
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
use engine::types::replacements::ReplacementEvent;
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

/// Puts a permanent named `name` under `controller` on the battlefield carrying a
/// `ReplacementEvent::Draw` definition that `customize` shapes, and returns it so
/// a `runtime_execute` substitute can bind it as its source.
///
/// `controller` is the replacement's source player: with the default
/// `valid_player` scope (CR 614.1a) the replacement applies only to THAT player's
/// draws, which is what makes source-scope discriminating.
///
/// The single Draw-definition producer in this file — every replacement shape
/// below is a `customize` parameterization of it, so
/// `scripts/draw_replacement_census.py` freezes one row rather than one per
/// shape.
fn add_draw_replacement(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    customize: impl FnOnce(&mut ReplacementDefinition),
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    let mut repl = ReplacementDefinition::new(ReplacementEvent::Draw)
        .draw_scope(DrawReplacementScope::IndividualDraw);
    customize(&mut repl);
    obj.replacement_definitions.push(repl);
    id
}

/// Living Conundrum shape: "if you would draw a card, skip that draw instead" —
/// a mandatory `Prevent` quantity modification on `controller`'s draws.
fn add_prevent_draw_replacement(
    state: &mut GameState,
    controller: PlayerId,
    customize: impl FnOnce(&mut ReplacementDefinition),
) {
    add_draw_replacement(state, controller, "Living Conundrum", |repl| {
        repl.quantity_modification = Some(QuantityModification::Prevent);
        customize(repl);
    });
}

/// Scores a cast-a-draw-spell candidate with the payoff engine already out.
fn draw_spell_verdict(st: &mut GameState) -> (f64, PolicyReason) {
    let config = AiConfig::default();
    let (oid, cid) = draw_spell(st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    score_of(DrawPayoffPolicy.verdict(&ctx(st, &candidate, &decision, &context, &config)))
}

/// CR 614.6: a mandatory `Prevent` draw replacement whose source scopes it to
/// the drawing player suppresses the draw entirely — the replaced event never
/// happens, so no `CardDrawn` fires and the engine never triggers. The delivery
/// preflight withholds the bonus.
#[test]
fn mandatory_prevent_draw_replacement_is_a_no_op() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_prevent_draw_replacement(&mut st, AI, |_| {});
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// CR 614.1a: the same definition on an OPPONENT's permanent replaces that
/// player's draws, not the AI's. Control for scanning `active_replacements` by
/// event alone — the AI's draw is still deliverable, so the payoff still pays.
#[test]
fn opponent_scoped_prevent_draw_replacement_still_rewards() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_prevent_draw_replacement(&mut st, PlayerId(1), |_| {});
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// CR 614.1d: a conditional replacement whose condition does not hold is not
/// applicable, so it cannot suppress the draw. `UnlessPlayerLifeAtMost { 20 }`
/// is false at starting life totals.
#[test]
fn false_conditional_prevent_draw_replacement_still_rewards() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_prevent_draw_replacement(&mut st, AI, |repl| {
        repl.condition = Some(ReplacementCondition::UnlessPlayerLifeAtMost { amount: 20 });
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// An optional replacement is offered as an accept/decline choice, so it never
/// obligatorily suppresses the draw — the preflight must not assume it applies.
#[test]
fn optional_prevent_draw_replacement_still_rewards() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_prevent_draw_replacement(&mut st, AI, |repl| {
        repl.mode = ReplacementMode::Optional { decline: None };
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// `ReplacementEvent::DrawCards` is a recognized-but-stub registry entry, not a
/// runtime draw handler — it replaces nothing at resolution, so it must not
/// suppress the payoff either.
#[test]
fn draw_cards_stub_prevent_replacement_still_rewards() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_prevent_draw_replacement(&mut st, AI, |repl| {
        repl.event = ReplacementEvent::DrawCards;
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

// ─── candidate draw quantity (CR 121.1 + CR 107.1b) ──────────────────────────
//
// A draw instruction only fires the engine if it actually delivers a card. The
// resolver resolves the effect's own quantity (`resolve_quantity_with_targets(..)
// .max(0)`), so a zero count emits no `CardDrawn` no matter how healthy the
// library is. These pin that the candidate's OWN count is required positive,
// distinct from the player-level "can this player draw at all" delivery gate.

/// Routes a cast candidate through `PolicyRegistry` and returns its verdict.
fn registry_cast_verdict(st: &GameState, oid: ObjectId, cid: CardId) -> (f64, PolicyReason) {
    let config = AiConfig::default();
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    PolicyRegistry::default()
        .verdicts(&ctx(st, &candidate, &decision, &context, &config))
        .into_iter()
        .find(|(id, _)| *id == PolicyId::DrawPayoff)
        .map(|(_, v)| score_of(v))
        .expect("the cast must reach the policy through the registry")
}

/// CR 107.1b: a fixed zero-count draw resolves to no cards, so no `CardDrawn`
/// fires and the engine never triggers — the payoff must be withheld even with a
/// live engine and a full library. Registry-routed, so the production seam is
/// what is asserted.
#[test]
fn registry_fixed_zero_count_draw_is_not_rewarded() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    let (oid, cid) = spell(
        &mut st,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 0 },
            target: TargetFilter::Controller,
        },
    );
    let (delta, reason) = registry_cast_verdict(&st, oid, cid);
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// Discriminating control for the case above: identical shape, positive count.
/// Without this pair the zero-count assertion is satisfiable by a policy that
/// stopped rewarding casts altogether.
#[test]
fn registry_positive_count_draw_is_rewarded() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    let (oid, cid) = spell(
        &mut st,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let (delta, reason) = registry_cast_verdict(&st, oid, cid);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// Builds a "draw X cards" spell and binds `X` on the source via `cost_x_paid`,
/// the slot `QuantityRef::Variable { "X" }` reads (CR 601.2b).
fn draw_x_spell(state: &mut GameState, x: Option<u32>) -> (ObjectId, CardId) {
    let (oid, cid) = spell(
        state,
        Effect::Draw {
            count: QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
            target: TargetFilter::Controller,
        },
    );
    state.objects.get_mut(&oid).unwrap().cost_x_paid = x;
    (oid, cid)
}

/// CR 601.2b: a "draw X cards" candidate is scored BEFORE X is announced, so its
/// count is not knowable at this seam and the policy stays neutral rather than
/// crediting a draw it cannot confirm — the same conservative direction as the
/// trigger-eligibility gate.
///
/// Asserted for both an unset and a set `cost_x_paid` because the engine's
/// `resolve_quantity` reads X from the RESOLVING ability's `chosen_x`, which only
/// `resolve_quantity_with_targets` supplies from a `ResolvedAbility` — a spell
/// still being announced has none. `cost_x_paid` on the object is therefore not
/// consulted here, and a stale value from an earlier activation must not be
/// mistaken for this candidate's X. Both cases resolve to zero, so both are
/// neutral; this pins that equivalence so a future X-binding change has to come
/// with a deliberate decision about which value the policy trusts.
#[test]
fn registry_x_draw_is_conservatively_neutral_before_announcement() {
    for cost_x_paid in [None, Some(2)] {
        let mut st = state();
        engine_on_battlefield(&mut st);
        let (oid, cid) = draw_x_spell(&mut st, cost_x_paid);
        let (delta, reason) = registry_cast_verdict(&st, oid, cid);
        assert_eq!(
            reason.kind, "draw_payoff_na",
            "X is unbound at the candidate seam (cost_x_paid={cost_x_paid:?})"
        );
        assert_eq!(delta, 0.0);
    }
}

// ─── bounded score (MAX_REWARDED_ENGINES) ────────────────────────────────────

/// Reads the `engines` observability fact off a verdict reason.
fn engines_fact(reason: &PolicyReason) -> Option<i64> {
    reason
        .facts
        .iter()
        .find(|(key, _)| *key == "engines")
        .map(|(_, value)| *value)
}

/// The per-draw reward scales with the number of live engines but is capped at
/// `MAX_REWARDED_ENGINES`, so a stacked board can't push a single draw into the
/// critical band. With one engine PAST the cap the delta must not grow.
///
/// The `engines` fact deliberately reports the TRUE uncapped count — that is an
/// observability contract (it explains the board to a log reader), distinct from
/// the bounded score. Both halves are asserted so neither can drift.
#[test]
fn reward_is_capped_at_max_rewarded_engines() {
    let bonus = AiConfig::default().policy_penalties.draw_payoff_bonus;

    let mut at_cap = state();
    for _ in 0..MAX_REWARDED_ENGINES {
        engine_on_battlefield(&mut at_cap);
    }
    let (delta_at_cap, reason_at_cap) = draw_spell_verdict(&mut at_cap);

    let mut over_cap = state();
    for _ in 0..MAX_REWARDED_ENGINES + 1 {
        engine_on_battlefield(&mut over_cap);
    }
    let (delta_over_cap, reason_over_cap) = draw_spell_verdict(&mut over_cap);

    assert_eq!(reason_at_cap.kind, "draw_payoff_engine_active");
    assert_eq!(reason_over_cap.kind, "draw_payoff_engine_active");
    assert_eq!(
        delta_at_cap,
        bonus * MAX_REWARDED_ENGINES as f64,
        "at the cap the reward is one bonus per live engine"
    );
    assert_eq!(
        delta_over_cap, delta_at_cap,
        "an engine past MAX_REWARDED_ENGINES must not increase the reward — \
         without the cap this would scale without bound"
    );
    assert_eq!(
        engines_fact(&reason_over_cap),
        Some(MAX_REWARDED_ENGINES as i64 + 1),
        "the `engines` fact reports the true uncapped count for observability"
    );
}

/// Below the cap the reward still scales, so the test above is pinning a CAP and
/// not merely a constant score.
#[test]
fn reward_scales_below_the_cap() {
    let bonus = AiConfig::default().policy_penalties.draw_payoff_bonus;
    let mut st = state();
    engine_on_battlefield(&mut st);
    engine_on_battlefield(&mut st);
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert_eq!(delta, bonus * 2.0);
    assert_eq!(engines_fact(&reason), Some(2));
}

// ─── replacement substitution and rescaling (CR 614.11) ──────────────────────
//
// A `Prevent` quantity modification is only ONE of the three ways the pipeline
// removes a draw. It can also be substituted away by a non-Draw chain, or
// rescaled to zero. All three are classified by the shared engine authority
// `replacement::proposed_draw_survives_replacement`, whose substitution leg is
// the very function `apply_single_replacement` uses to pre-zero the live count —
// these cases pin that the preflight and the pipeline stay in agreement.

/// A non-Draw substitute chain: "instead, you gain 5 life" — the body of Words
/// of Worship, "{1}: The next time you would draw a card this turn, you gain 5
/// life instead."
///
/// The classifier keys on "not a `Draw`, not a pure event modifier", so this
/// stands in for the whole substitute class: Chains of Mephistopheles' "that
/// player discards a card instead", Jace, Wielder of Mysteries' "you win the
/// game instead", Abundance's reveal-until. What varies between those cards is
/// which slot carries the substitute and whether it is mandatory — the axes the
/// cases below vary — not the substitute effect itself.
fn gain_life_substitute() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 5 },
            player: TargetFilter::Controller,
        },
    )
}

/// A draw-count substitute: "draw that many cards plus one instead"
/// (Alhammarret's Archive / Teferi's Ageless Insight, CR 614.11a). Still a draw.
fn draw_count_substitute(value: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value },
            target: TargetFilter::Controller,
        },
    )
}

/// CR 614.11: a mandatory `execute` substitute that is not a draw replaces the
/// draw event away — `apply_single_replacement` zeroes the proposed count, so no
/// `CardDrawn` is emitted and the "whenever you draw" engine never triggers. The
/// bonus must be withheld even though nothing here is a `Prevent`.
///
/// The printed-static half of the class: Chains of Mephistopheles ("that player
/// discards a card instead"), Jace, Wielder of Mysteries ("you win the game
/// instead"). Both carry the substitute in `execute`.
#[test]
fn mandatory_execute_substitution_is_a_no_op() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_replacement(&mut st, AI, "Chains of Mephistopheles", |repl| {
        repl.execute = Some(Box::new(gain_life_substitute()));
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// CR 614.11: a one-shot draw replacement created by a resolving ability carries
/// its substitute in `runtime_execute` while `execute` stays `None`. That slot
/// substitutes the draw away exactly as `execute` does, so the preflight must
/// inspect it too — the leg a definition-shaped scan of `execute` alone misses.
///
/// The activated-one-shot half of the class, and an exact fit: Words of Worship
/// is "{1}: The next time you would draw a card this turn, you gain 5 life
/// instead" (Words of Wilding substitutes a 2/2 Bear token the same way).
#[test]
fn mandatory_runtime_execute_substitution_is_a_no_op() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    let source = add_draw_replacement(&mut st, AI, "Words of Worship", |_| {});
    let runtime = engine::types::ability::ResolvedAbility::new(
        gain_life_substitute().effect.as_ref().clone(),
        Vec::new(),
        source,
        AI,
    );
    let obj = st.objects.get_mut(&source).unwrap();
    obj.replacement_definitions[0].runtime_execute = Some(Box::new(runtime));
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

/// CR 614.6: the same substitution offered as "you may" is an accept/decline
/// choice, so it cannot be assumed to apply — the draw is still deliverable and
/// the payoff still pays. Control that the substitution leg gates on mandatory
/// mode rather than on the presence of a non-Draw `execute`.
///
/// Abundance is the printed case: "If you would draw a card, you MAY instead
/// choose land or nonland and reveal cards from the top of your library…".
#[test]
fn optional_execute_substitution_still_rewards() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_replacement(&mut st, AI, "Abundance", |repl| {
        repl.execute = Some(Box::new(gain_life_substitute()));
        repl.mode = ReplacementMode::Optional { decline: None };
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// CR 614.1a: an opponent-sourced mandatory substitution scopes to THAT player's
/// draws, so the AI's draw survives. Control that the substitution leg inherits
/// the live applicability gate rather than scanning definitions by event alone.
#[test]
fn opponent_scoped_execute_substitution_still_rewards() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_replacement(&mut st, PlayerId(1), "Chains of Mephistopheles", |repl| {
        repl.execute = Some(Box::new(gain_life_substitute()));
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// CR 614.11a: a count-modifying replacement rescales the draw rather than
/// removing it — Alhammarret's Archive and Teferi's Ageless Insight both read
/// "…draw two cards instead" (each gated on "except the first one you draw in
/// each of your draw steps"; the gate is immaterial here, so the definition is
/// modeled ungated). A rescaled draw still emits `CardDrawn`, so the payoff must
/// be paid.
///
/// The discriminating positive control for
/// `mandatory_execute_substitution_is_a_no_op`: both carry a mandatory
/// `execute`, and only the non-Draw one suppresses.
#[test]
fn count_modifying_draw_replacement_still_rewards() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_replacement(&mut st, AI, "Alhammarret's Archive", |repl| {
        repl.execute = Some(Box::new(draw_count_substitute(2)));
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
}

/// CR 614.11a: a mandatory count modification that resolves to ZERO leaves no
/// card to draw — `draw_applier` yields `Modified { count: 0 }` and the delivery
/// loop emits no `CardDrawn`. Third suppression leg, distinct from both `Prevent`
/// and non-Draw substitution: the `execute` here IS a draw, so the substitution
/// classifier correctly declines it and only the resolved count discriminates.
///
/// A synthetic boundary rather than a printed card — the count-modifier surface
/// accepts any `QuantityExpr`, and zero is the value at which a rescaled draw
/// stops being a draw. Pinned so the leg cannot regress unnoticed.
#[test]
fn zero_count_draw_replacement_is_a_no_op() {
    let mut st = state();
    engine_on_battlefield(&mut st);
    add_draw_replacement(&mut st, AI, "Zero-Count Draw Rescaler", |repl| {
        repl.execute = Some(Box::new(draw_count_substitute(0)));
    });
    let (delta, reason) = draw_spell_verdict(&mut st);
    assert_eq!(reason.kind, "draw_payoff_na");
    assert_eq!(delta, 0.0);
}

// ─── multi-target engine legality (CR 603.3d) ────────────────────────────────

/// A creature `TargetFilter`.
fn creature_filter() -> TargetFilter {
    TargetFilter::Typed(
        engine::types::ability::TypedFilter::default()
            .with_type(engine::types::ability::TypeFilter::Creature),
    )
}

/// A required-modal payoff: "whenever you draw, choose one — deal 3 to target
/// creature; or deal 3 to target creature". The execute is a modal placeholder
/// with all target-required modes (the targets live in `mode_abilities`), so on
/// an empty board every mode is unavailable and the live trigger is dropped
/// (`DroppedNoLegalMode`, CR 603.3c).
fn modal_all_target_required_engine(state: &mut GameState) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        ENGINE_NAME.to_string(),
        Zone::Battlefield,
    );
    let mode = || {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: creature_filter(),
                damage_source: None,
                excess: None,
            },
        )
    };
    let mut execute = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::unimplemented("modal_placeholder", ""),
    );
    execute.modal = Some(ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        ..Default::default()
    });
    execute.mode_abilities = vec![mode(), mode()];
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Enchantment);
    obj.trigger_definitions
        .push(TriggerDefinition::new(TriggerMode::Drawn).execute(execute));
    id
}

/// CR 603.3c: a required "choose one" payoff whose every mode needs a target and
/// none is available on an empty board has no legal mode, so the live trigger is
/// dropped — the modal-aware preflight reports it not-live.
#[test]
fn modal_engine_with_no_legal_mode_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    modal_all_target_required_engine(&mut st); // empty board → no legal mode
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

/// Control: once a legal creature target exists, at least one mode is choosable,
/// so the modal engine is live and the draw is rewarded.
#[test]
fn modal_engine_with_a_legal_mode_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    modal_all_target_required_engine(&mut st);
    add_opponent_creature(&mut st); // a legal target for a mode
    let (oid, cid) = draw_spell(&mut st);
    let context = context(&config, session(0.9));
    let candidate = cast(oid, cid);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DrawPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0);
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

/// End-to-end: an activated DRAW ability routes through `DecisionKind::ActivateAbility`
/// to the policy and is rewarded — covering the second decision kind the policy
/// registers, not just the direct-`verdict` path.
#[test]
fn registry_routes_activated_draw_to_the_policy() {
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
    let (delta, reason) = PolicyRegistry::default()
        .verdicts(&ctx(&st, &candidate, &decision, &context, &config))
        .into_iter()
        .find(|(id, _)| *id == PolicyId::DrawPayoff)
        .map(|(_, v)| score_of(v))
        .expect("the activated draw must reach the policy through the registry");
    assert_eq!(reason.kind, "draw_payoff_engine_active");
    assert!(delta > 0.0, "routed reward must be positive, got {delta}");
}

/// Control: an activated NON-draw ability routes to the policy but is not
/// rewarded — guards against the classifier crediting every activation.
#[test]
fn registry_activated_non_draw_is_not_rewarded() {
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
    let routed = PolicyRegistry::default()
        .verdicts(&ctx(&st, &candidate, &decision, &context, &config))
        .into_iter()
        .find(|(id, _)| *id == PolicyId::DrawPayoff)
        .map(|(_, v)| score_of(v));
    // Either the policy is absent for this action, or it returns a neutral verdict.
    if let Some((delta, reason)) = routed {
        assert_eq!(reason.kind, "draw_payoff_na");
        assert_eq!(delta, 0.0);
    }
}
