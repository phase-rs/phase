//! Unit tests for `policies::cycling_payoff` — CR 702.29c/d "cycling matters"
//! payoff policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! Direct-`verdict` tests cover each branch; a registry-routed regression
//! exercises the production seam (registration + `ActivateAbility` routing),
//! following the poison policy's pattern.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::ability_utils::{build_resolved_from_def, build_target_slots};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerConstraint,
    TriggerDefinition, TypedFilter,
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
fn permanent_with_trigger(state: &mut GameState, trigger: Option<TriggerDefinition>) -> ObjectId {
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
    id
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

/// A once-per-turn "whenever you cycle" engine (Valiant Rescuer shape).
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
        .push(cycle_trigger(TargetFilter::Any).constraint(TriggerConstraint::OncePerTurn));
    id
}

/// [MED review] A once-per-turn engine that has already fired this turn cannot
/// fire again (CR 603.4), so a second cycle earns nothing — the policy consults
/// the fired-trigger ledger, not just the structural trigger shape.
#[test]
fn rate_limited_engine_already_fired_this_turn_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    let engine_id = once_per_turn_engine(&mut st);
    // Mark its trigger as already fired this turn via the ledger authority.
    let key = {
        let obj = st.objects.get(&engine_id).unwrap();
        let entry = obj.trigger_definitions.iter_unchecked().next().unwrap();
        obj.trigger_definition_ref(entry)
    };
    st.triggers_fired_this_turn.insert(key);

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

/// Control: the same once-per-turn engine that has NOT fired yet still rewards.
#[test]
fn rate_limited_engine_not_yet_fired_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    once_per_turn_engine(&mut st);
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
    assert!(delta > 0.0, "an unfired once-per-turn engine still rewards");
}

/// An engine trigger with `constraint` on the AI's own permanent.
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
        .push(cycle_trigger(TargetFilter::Any).constraint(constraint));
    id
}

/// A once-per-game engine already fired this game earns nothing more.
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

/// An `OnlyDuringYourTurn` engine on the opponent's turn cannot fire, so cycling
/// at instant speed during their turn earns nothing.
#[test]
fn only_during_your_turn_engine_is_neutral_off_turn() {
    let config = AiConfig::default();
    let mut st = state();
    st.active_player = PlayerId(1);
    engine_with_constraint(&mut st, TriggerConstraint::OnlyDuringYourTurn);
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

/// "Deal 2 damage to target creature" — exactly one MANDATORY creature target.
fn damage_target_creature() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Typed(TypedFilter::creature()),
            damage_source: None,
            excess: None,
        },
    )
}

/// A "whenever you cycle, deal 2 damage to TARGET creature" payoff — value
/// depends on a legal creature target existing (CR 603.3d). Distinct from the
/// no-target `cycle_trigger` (a `Draw` needs no target slot).
fn targeted_cycle_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::CycledOrDiscarded).execute(damage_target_creature())
}

/// The same payoff with a sub-ability chain, so the execute carries TWO
/// mandatory creature target slots — the shape the cheap single-slot check
/// cannot decide, which must fall through to the engine's full
/// legal-assignment authority rather than being assumed satisfiable.
fn two_target_cycle_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::CycledOrDiscarded)
        .execute(damage_target_creature().sub_ability(damage_target_creature()))
}

/// How many target slots the engine builds for the payoff permanent's trigger
/// execute — used to assert a fixture's target SHAPE, so a slot-count change
/// surfaces as a precondition failure instead of a silently weakened test.
fn engine_execute_target_slot_count(state: &GameState, engine_id: ObjectId) -> usize {
    let obj = state
        .objects
        .get(&engine_id)
        .expect("payoff permanent must be on the battlefield");
    let entry = obj
        .trigger_definitions
        .iter_unchecked()
        .next()
        .expect("payoff permanent must carry a trigger");
    let execute = entry
        .definition
        .execute
        .as_deref()
        .expect("payoff trigger must have an execute");
    let resolved = build_resolved_from_def(execute, obj.id, obj.controller);
    build_target_slots(state, &resolved)
        .expect("target slots must build")
        .len()
}

/// Puts an opponent creature on the battlefield — a legal "target creature".
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

/// CR 603.3d: a mandatory-target cycling payoff with no legal target on the board
/// cannot resolve to an effect, so the engine's `hypothetical_trigger_fireable`
/// target-legality preflight rejects it and no bonus is awarded. `permanent_with_trigger`
/// builds an Enchantment, so an empty board truly has no creature to target.
#[test]
fn targeted_payoff_with_no_legal_target_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(&mut st, Some(targeted_cycle_trigger()));
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

/// Control: once a legal creature target exists, the same targeted payoff is live
/// and cycling is rewarded.
#[test]
fn targeted_payoff_with_a_legal_target_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(&mut st, Some(targeted_cycle_trigger()));
    add_opponent_creature(&mut st);
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
    assert!(delta > 0.0);
}

/// CR 603.3d, multi-slot shape: a payoff whose execute needs TWO mandatory
/// creature targets earns nothing on a creatureless board. This is the shape
/// the cheap single-slot check reports as UNDECIDABLE; answering that
/// optimistically — as an `unwrap_or(true)` would — credits a trigger that
/// cannot resolve to an effect. Legality is instead settled by the engine's own
/// target authority, which reports no legal targets for the mandatory slots.
#[test]
fn multi_target_payoff_with_no_legal_target_is_neutral() {
    let config = AiConfig::default();
    let mut st = state();
    permanent_with_trigger(&mut st, Some(two_target_cycle_trigger()));
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

/// Control for the pair above: with creatures available the identical two-target
/// payoff is live and rewarded — so the multi-slot path RESOLVES legality rather
/// than rejecting every shape it cannot cheaply decide. Also pins the fixture's
/// target shape, so a slot-count change surfaces here as a precondition failure
/// instead of silently degrading the negative case into a single-slot test.
#[test]
fn multi_target_payoff_with_legal_targets_rewards() {
    let config = AiConfig::default();
    let mut st = state();
    let engine_id = permanent_with_trigger(&mut st, Some(two_target_cycle_trigger()));
    add_opponent_creature(&mut st);
    add_opponent_creature(&mut st);
    assert_eq!(
        engine_execute_target_slot_count(&st, engine_id),
        2,
        "fixture must carry TWO mandatory target slots"
    );
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
    assert!(delta > 0.0);
}

/// A `MaxTimesPerTurn { max }` engine that has fired fewer than `max` times this
/// turn can still fire, so cycling is rewarded — the shared engine authority reads
/// the live `trigger_fire_counts_this_turn` ledger rather than rejecting the
/// constraint categorically.
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
    assert!(delta > 0.0);
}

/// Control: the same engine that has already fired `max` times this turn cannot
/// fire again, so cycling earns nothing.
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
