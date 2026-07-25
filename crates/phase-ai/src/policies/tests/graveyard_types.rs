//! Unit tests for `policies::graveyard_types` — the delirium/descend progress
//! policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! The `verdict` path runs against a real `PolicyContext` built over a
//! two-player `GameState`, mirroring the `energy_payoff` policy-test shape, so
//! the authoritative cast/activated-ability lookup and the graveyard scan are
//! exercised end to end rather than in isolation.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, TargetFilter, TriggerDefinition,
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
use crate::features::graveyard_types::{GraveyardTypesFeature, GRAVEYARD_TYPES_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::graveyard_types::*;
use crate::policies::registry::{PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);

// ─── fixtures ───────────────────────────────────────────────────────────────

fn config() -> AiConfig {
    AiConfig::default()
}

/// An `AiContext` whose cached graveyard feature carries the given threshold and
/// scaling posture, so `verdict` reads them the way it would in a real game.
fn context_with(
    config: &AiConfig,
    highest_threshold: Option<u32>,
    scaling_payoff_count: u32,
) -> AiContext {
    let features = DeckFeatures {
        graveyard_types: GraveyardTypesFeature {
            threshold_payoff_count: highest_threshold.map_or(0, |_| 8),
            scaling_payoff_count,
            enabler_count: 8,
            highest_threshold,
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

fn self_mill_effect() -> Effect {
    Effect::Mill {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
        destination: Zone::Graveyard,
    }
}

fn draw_effect() -> Effect {
    Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }
}

/// Put `count` objects with distinct core types into the AI's graveyard so
/// `distinct_graveyard_types` reports exactly `count`.
fn seed_graveyard_types(state: &mut GameState, count: usize) {
    const TYPES: [CoreType; 6] = [
        CoreType::Creature,
        CoreType::Instant,
        CoreType::Sorcery,
        CoreType::Artifact,
        CoreType::Enchantment,
        CoreType::Land,
    ];
    for (i, core) in TYPES.iter().take(count).enumerate() {
        let oid = create_object(
            state,
            CardId(1000 + i as u64),
            AI,
            format!("GY {i}"),
            Zone::Graveyard,
        );
        state.objects.get_mut(&oid).unwrap().card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![*core],
            subtypes: Vec::new(),
        };
    }
}

/// A hand object whose SPELL resolution mills the controller (Thought Scour
/// shape) — casting it fills the graveyard. `card_id` is aligned to the object
/// id so `cast_candidate` resolves it through `cast_facts`.
fn mill_spell(state: &mut GameState, idx: u64) -> ObjectId {
    let oid = create_object(
        state,
        CardId(idx),
        AI,
        format!("Mill Spell {idx}"),
        Zone::Hand,
    );
    let object = state.objects.get_mut(&oid).unwrap();
    object.card_id = CardId(oid.0);
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Instant],
        subtypes: Vec::new(),
    };
    *Arc::make_mut(&mut object.abilities) = vec![AbilityDefinition::new(
        AbilityKind::Spell,
        self_mill_effect(),
    )];
    oid
}

/// A hand object that is a CREATURE with an ACTIVATED self-mill ability and no
/// spell-resolution mill — casting it fills no graveyard; activating it does.
fn creature_with_activated_mill(state: &mut GameState, idx: u64) -> ObjectId {
    let oid = create_object(
        state,
        CardId(idx),
        AI,
        format!("Filler Body {idx}"),
        Zone::Hand,
    );
    let object = state.objects.get_mut(&oid).unwrap();
    object.card_id = CardId(oid.0);
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Creature],
        subtypes: Vec::new(),
    };
    *Arc::make_mut(&mut object.abilities) = vec![AbilityDefinition::new(
        AbilityKind::Activated,
        self_mill_effect(),
    )];
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

fn activate_candidate(source_id: ObjectId, ability_index: usize) -> CandidateAction {
    CandidateAction {
        action: GameAction::ActivateAbility {
            source_id,
            ability_index,
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
    }
}

fn score_of(verdict: PolicyVerdict) -> (f64, PolicyReason) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (delta, reason),
        PolicyVerdict::Reject { reason } => panic!("unexpected Reject: {reason:?}"),
    }
}

// Object ids for hand fixtures must match `cast_candidate`'s `card_id ==
// object_id.0`, so keep them clear of the 1000+ graveyard-seed ids.
fn state() -> GameState {
    GameState::new_two_player(42)
}

// ─── activation + helpers ───────────────────────────────────────────────────

#[test]
fn activation_opts_out_below_floor() {
    let mut features = DeckFeatures::default();
    features.graveyard_types.commitment = GRAVEYARD_TYPES_FLOOR - 0.01;
    assert!(GraveyardTypesPolicy
        .activation(&features, &state(), AI)
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let mut features = DeckFeatures::default();
    features.graveyard_types.commitment = 0.9;
    assert_eq!(
        GraveyardTypesPolicy.activation(&features, &state(), AI),
        Some(0.9)
    );
}

/// CR 404.1: an empty graveyard has zero distinct card types.
#[test]
fn empty_graveyard_counts_zero_types() {
    assert_eq!(distinct_graveyard_types(&state(), AI), 0);
}

#[test]
fn distinct_graveyard_types_counts_each_core_type_once() {
    let mut state = state();
    seed_graveyard_types(&mut state, 3);
    assert_eq!(distinct_graveyard_types(&state, AI), 3);
}

// ─── verdict: threshold race (CR 205.2a) ────────────────────────────────────

/// One type short of the threshold, casting a self-mill is the strongest play —
/// it can switch every delirium payoff on.
#[test]
fn verdict_rewards_the_last_missing_type_strongly() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 3); // deficit == 1
    let spell = mill_spell(&mut state, 1);
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(spell),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_progress");
    assert!(
        delta > crate::policies::registry::PREFERENCE_MAX,
        "the last missing type is a strong play, got {delta}"
    );
}

/// Farther from the threshold, the same self-mill is worth less per point.
#[test]
fn verdict_scales_progress_by_deficit() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let decision = priority_decision();

    let score_at = |types: usize| {
        let mut state = state();
        seed_graveyard_types(&mut state, types);
        let spell = mill_spell(&mut state, 1);
        score_of(GraveyardTypesPolicy.verdict(&ctx(
            &state,
            &cast_candidate(spell),
            &decision,
            &context,
            &config,
        )))
        .0
    };

    // deficit 3 (1 type) < deficit 1 (3 types): closer to the threshold scores
    // higher.
    assert!(score_at(3) > score_at(1));
    assert!(score_at(1) > 0.0);
}

/// At or over the threshold with no scaling payoff, delirium is on and more
/// diversity buys nothing.
#[test]
fn verdict_is_neutral_once_threshold_met_without_scaling() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 4);
    let spell = mill_spell(&mut state, 1);
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(spell),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_threshold_met");
    assert_eq!(delta, 0.0);
}

// ─── verdict: scaling-only continuation (the item-2 fix) ────────────────────

/// A scaling-only deck (no threshold) must keep receiving a progress signal
/// ABOVE four types — the payoff continues to scale, so the old invented
/// four-type ceiling was wrong.
#[test]
fn verdict_still_rewards_scaling_only_deck_above_four_types() {
    let config = config();
    let context = context_with(&config, None, 4); // no threshold, has scaling
    let mut state = state();
    seed_graveyard_types(&mut state, 5); // above the old ceiling of 4
    let spell = mill_spell(&mut state, 1);
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(spell),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_scaling");
    assert!(
        delta > 0.0,
        "a scaling payoff still wants more types, got {delta}"
    );
}

/// The scaling reward diminishes as the graveyard grows — the sixth type is
/// worth less than the second.
#[test]
fn verdict_scaling_reward_diminishes() {
    let config = config();
    let context = context_with(&config, None, 4);
    let decision = priority_decision();

    let score_at = |types: usize| {
        let mut state = state();
        seed_graveyard_types(&mut state, types);
        let spell = mill_spell(&mut state, 1);
        score_of(GraveyardTypesPolicy.verdict(&ctx(
            &state,
            &cast_candidate(spell),
            &decision,
            &context,
            &config,
        )))
        .0
    };
    assert!(score_at(1) > score_at(5));
    assert!(score_at(5) > 0.0);
}

/// A mixed deck past its threshold still rewards diversity for the scaling half.
#[test]
fn verdict_mixed_deck_keeps_scaling_past_threshold() {
    let config = config();
    let context = context_with(&config, Some(4), 2); // both threshold and scaling
    let mut state = state();
    seed_graveyard_types(&mut state, 5); // past the threshold
    let spell = mill_spell(&mut state, 1);
    let decision = priority_decision();

    let (_, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(spell),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_scaling");
}

// ─── verdict: authoritative cast/activate semantics (the item-5 fix) ─────────

/// CR 601.2: casting a permanent that merely HAS an activated self-mill ability
/// fills no graveyard — the cast's own resolution does nothing here.
#[test]
fn verdict_ignores_cast_of_a_body_with_an_activated_mill() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 3);
    let body = creature_with_activated_mill(&mut state, 1);
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(body),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_na");
    assert_eq!(delta, 0.0);
}

/// But ACTIVATING that same body's self-mill ability is credited, resolved
/// through the engine's enumerated ability index.
#[test]
fn verdict_credits_the_activated_mill_ability() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 3);
    let body = creature_with_activated_mill(&mut state, 1);
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &activate_candidate(body, 0),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_progress");
    assert!(delta > 0.0);
}

/// A spell whose OWN resolution mills is credited on cast.
#[test]
fn verdict_credits_a_spell_that_mills_on_resolution() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 3);
    let spell = mill_spell(&mut state, 1);
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(spell),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_progress");
    assert!(delta > 0.0);
}

/// A spell that does not touch the graveyard is neutral.
#[test]
fn verdict_ignores_a_non_filling_spell() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 3);
    let oid = create_object(
        &mut state,
        CardId(1),
        AI,
        "Draw Spell".to_string(),
        Zone::Hand,
    );
    {
        let object = state.objects.get_mut(&oid).unwrap();
        object.card_id = CardId(oid.0);
        object.card_types = CardType {
            supertypes: Vec::new(),
            core_types: vec![CoreType::Instant],
            subtypes: Vec::new(),
        };
        *Arc::make_mut(&mut object.abilities) =
            vec![AbilityDefinition::new(AbilityKind::Spell, draw_effect())];
    }
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(oid),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_na");
    assert_eq!(delta, 0.0);
}

// ─── ETB-trigger casts (the delirium play the gate used to drop) ─────────────

/// Stitcher's Supplier shape: a creature whose self-mill rides an ETB TRIGGER,
/// not a spell ability. `CastFacts` carries it as `immediate_etb_triggers`
/// precisely because it fires as a consequence of the cast.
fn etb_mill_creature(state: &mut GameState, idx: u64) -> ObjectId {
    let oid = create_object(
        state,
        CardId(idx),
        AI,
        format!("Stitcher {idx}"),
        Zone::Hand,
    );
    let object = state.objects.get_mut(&oid).unwrap();
    object.card_id = CardId(oid.0);
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Creature],
        subtypes: Vec::new(),
    };
    *Arc::make_mut(&mut object.abilities) = Vec::new();
    object.trigger_definitions.push(
        TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                self_mill_effect(),
            )),
    );
    oid
}

/// Casting the archetypal delirium enabler must score. Excluding ETB triggers
/// from the cast gate made this exact play return `graveyard_types_na`.
#[test]
fn verdict_credits_a_cast_whose_etb_trigger_mills() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 3);
    let stitcher = etb_mill_creature(&mut state, 1);
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(stitcher),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_progress");
    assert!(delta > 0.0);
}

/// Control: an ETB trigger that does NOT fill the graveyard stays neutral, so
/// the branch discriminates on the trigger body rather than on "has any ETB".
#[test]
fn verdict_ignores_a_cast_whose_etb_trigger_does_not_fill() {
    let config = config();
    let context = context_with(&config, Some(4), 0);
    let mut state = state();
    seed_graveyard_types(&mut state, 3);
    let oid = etb_mill_creature(&mut state, 1);
    state.objects.get_mut(&oid).unwrap().trigger_definitions = Default::default();
    state
        .objects
        .get_mut(&oid)
        .unwrap()
        .trigger_definitions
        .push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(AbilityKind::Spell, draw_effect())),
        );
    let decision = priority_decision();

    let (delta, reason) = score_of(GraveyardTypesPolicy.verdict(&ctx(
        &state,
        &cast_candidate(oid),
        &decision,
        &context,
        &config,
    )));
    assert_eq!(reason.kind, "graveyard_types_na");
    assert_eq!(delta, 0.0);
}
