//! Unit tests for `policies::discard_payoff` — CR 701.9 "whenever you discard"
//! payoff policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! Direct-`verdict` tests cover each branch; a registry-routed regression
//! exercises the production seam (registration + routing).

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, CardSelectionMode, DiscardSelfScope, Effect,
    QuantityExpr, TargetFilter, TriggerDefinition,
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
use crate::features::discard_matters::{DiscardMattersFeature, DISCARD_MATTERS_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::discard_payoff::*;
use crate::policies::registry::{
    PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);

fn state() -> GameState {
    GameState::new(FormatConfig::standard(), 2, 42)
}

/// "Discard N cards" scoped to the caster (the rich `Effect::Discard` form).
fn discard_effect(count: i32, target: TargetFilter) -> Effect {
    Effect::Discard {
        count: QuantityExpr::Fixed { value: count },
        target,
        selection: CardSelectionMode::Chosen,
        unless_filter: None,
        filter: None,
    }
}

/// A hand spell whose resolution runs `effect`.
fn spell(state: &mut GameState, effect: Effect) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Pitch Spell".to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Sorcery);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(AbilityKind::Spell, effect));
    (id, card_id)
}

/// A battlefield permanent whose activated ability at index 0 runs `effect` —
/// the rummaging-outlet shape (Wild Mongrel / Anje).
fn activated_permanent(state: &mut GameState, effect: Effect) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Outlet".to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    Arc::make_mut(&mut obj.abilities).push(AbilityDefinition::new(AbilityKind::Activated, effect));
    id
}

/// The Archfiend of Ifnir shape: a no-target on-discard payoff, so target
/// legality never blocks it.
fn discarded_engine_trigger() -> TriggerDefinition {
    TriggerDefinition::new(TriggerMode::Discarded).execute(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    ))
}

fn engine_on_battlefield(state: &mut GameState, trigger: Option<TriggerDefinition>) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        AI,
        "Archfiend".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    if let Some(t) = trigger {
        obj.trigger_definitions.push(t);
    }
}

fn feature(commitment: f32) -> DiscardMattersFeature {
    DiscardMattersFeature {
        source_count: 10,
        payoff_count: 4,
        commitment,
    }
}

fn session(commitment: f32) -> AiSession {
    let features = DeckFeatures {
        discard_matters: feature(commitment),
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

/// The REAL Wild Mongrel / Anje shape: the discard is the ability's COST, not
/// its effect. This is the class the axis exists for, and the effect-only
/// classifier never reached it.
fn discard_cost(count: i32) -> AbilityCost {
    AbilityCost::Discard {
        count: QuantityExpr::Fixed { value: count },
        filter: None,
        selection: CardSelectionMode::Chosen,
        self_scope: DiscardSelfScope::FromHand,
    }
}

/// A battlefield permanent whose activated ability PAYS `cost` and does
/// something unrelated (pump) on resolution.
fn cost_paying_permanent(state: &mut GameState, cost: AbilityCost) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Mongrel".to_string(), Zone::Battlefield);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    let ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(cost);
    Arc::make_mut(&mut obj.abilities).push(ability);
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
    let features = DeckFeatures {
        discard_matters: feature(DISCARD_MATTERS_FLOOR - 0.01),
        ..Default::default()
    };
    assert!(DiscardPayoffPolicy
        .activation(&features, &state(), AI)
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let features = DeckFeatures {
        discard_matters: feature(0.8),
        ..Default::default()
    };
    assert_eq!(
        DiscardPayoffPolicy.activation(&features, &state(), AI),
        Some(0.8)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn discarding_with_an_engine_out_scores_positive() {
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let (obj, card) = spell(&mut st, discard_effect(2, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(
        delta > 0.0,
        "expected a positive payoff credit, got {delta}"
    );
}

#[test]
fn discarding_without_an_engine_is_neutral() {
    // Without a payoff the AI's instinct to avoid discarding is CORRECT, so this
    // policy must stay silent rather than push it to pitch cards.
    let mut st = state();
    let (obj, card) = spell(&mut st, discard_effect(2, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_no_engine");
    assert_eq!(delta, 0.0);
}

#[test]
fn opponent_discard_is_not_credited() {
    // `hand_disruption`'s subject — making THEM discard fires no engine of mine.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let (obj, card) = spell(&mut st, discard_effect(2, TargetFilter::Opponent));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn zero_count_discard_is_not_credited() {
    // CR 701.9 + CR 107.1b: a discard that moves no card emits no event.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let (obj, card) = spell(&mut st, discard_effect(0, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (_, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_na");
}

#[test]
fn non_discard_spell_is_not_applicable() {
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let (obj, card) = spell(
        &mut st,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    );

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (_, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_na");
}

#[test]
fn activated_rummaging_outlet_is_credited() {
    // The Wild Mongrel / Anje shape: the outlet is an activated ability, which
    // is where most real self-discard lives.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = activated_permanent(&mut st, discard_effect(1, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(delta > 0.0);
}

#[test]
fn simple_discard_card_variant_is_credited() {
    // Sibling-variant coverage at the LIVE seam: `Effect::DiscardCard` must be
    // classified as an enabler exactly like `Effect::Discard`.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = activated_permanent(
        &mut st,
        Effect::DiscardCard {
            count: 1,
            target: TargetFilter::Controller,
        },
    );

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(delta > 0.0);
}

#[test]
fn name_only_impostor_is_not_an_engine() {
    // A permanent with no live discard trigger must not be credited, even though
    // it sits on the battlefield under our control.
    let mut st = state();
    engine_on_battlefield(&mut st, None);
    let (obj, card) = spell(&mut st, discard_effect(1, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (_, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_no_engine");
}

#[test]
fn credit_is_bounded_by_the_engine_cap() {
    let mut st = state();
    for _ in 0..8 {
        engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    }
    let (obj, card) = spell(&mut st, discard_effect(1, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (delta, _) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    let ceiling = config.policy_penalties.discard_payoff_bonus * MAX_REWARDED_ENGINES as f64;
    assert!(
        delta <= ceiling + f64::EPSILON,
        "delta {delta} exceeded ceiling {ceiling}"
    );
}

// ─── production seam ─────────────────────────────────────────────────────────

#[test]
fn registry_routes_cast_spell_to_this_policy() {
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let (obj, card) = spell(&mut st, discard_effect(1, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let verdicts =
        PolicyRegistry::default().verdicts(&ctx(&st, &candidate, &decision, &context, &config));

    let found = verdicts
        .iter()
        .find(|(id, _)| *id == PolicyId::DiscardPayoff)
        .map(|(_, v)| v.clone())
        .expect("DiscardPayoffPolicy must be registered and routed for CastSpell");
    let (delta, reason) = score_of(found);
    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(delta > 0.0);
}

#[test]
fn registry_stays_silent_below_the_activation_floor() {
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let (obj, card) = spell(&mut st, discard_effect(1, TargetFilter::Controller));

    let config = AiConfig::default();
    let context = context(&config, session(DISCARD_MATTERS_FLOOR - 0.01));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let verdicts =
        PolicyRegistry::default().verdicts(&ctx(&st, &candidate, &decision, &context, &config));

    assert!(
        !verdicts
            .iter()
            .any(|(id, _)| *id == PolicyId::DiscardPayoff),
        "policy must not contribute below its activation floor"
    );
}

// ─── review #6786: the discard-as-COST path (the real rummaging class) ───────

#[test]
fn activated_discard_cost_outlet_is_credited() {
    // The blocker this PR was returned for: Wild Mongrel pays `AbilityCost::
    // Discard`, so the effect-only classifier scored it neutral even with a live
    // engine out. Fails without the cost-axis classification.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(&mut st, discard_cost(1));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(delta > 0.0, "a discard COST must be credited, got {delta}");
}

#[test]
fn composite_cost_containing_a_discard_is_credited() {
    // CR 601.2h: every component of a composite cost is paid, so the discard is
    // guaranteed — tap AND discard.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(
        &mut st,
        AbilityCost::Composite {
            costs: vec![AbilityCost::Tap, discard_cost(1)],
        },
    );

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(
        delta > 0.0,
        "a guaranteed composite discard must earn positive credit, got {delta}"
    );
}

#[test]
fn one_of_cost_is_not_credited_at_the_live_seam() {
    // CR 118.12a: only one branch is chosen. "Discard a card OR pay 2 life" is a
    // discard the DECK can plan around, but not one this candidate is committed
    // to — crediting it would score a discard the player may never make.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(
        &mut st,
        AbilityCost::OneOf {
            costs: vec![
                discard_cost(1),
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                },
            ],
        },
    );

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn one_of_cost_with_only_discard_branches_is_credited_at_the_live_seam() {
    // CR 118.12a: only one branch is paid, but this ability discards no matter
    // which branch is selected. It is therefore a guaranteed live discard,
    // unlike the mixed discard-or-life sibling above.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(
        &mut st,
        AbilityCost::OneOf {
            costs: vec![discard_cost(1), discard_cost(2)],
        },
    );

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(delta > 0.0, "expected positive credit, got {delta}");
}

#[test]
fn zero_count_discard_cost_is_not_credited() {
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(&mut st, discard_cost(0));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn discard_cost_without_an_engine_is_neutral() {
    let mut st = state();
    let outlet = cost_paying_permanent(&mut st, discard_cost(1));

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (_, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_no_engine");
}

#[test]
fn composite_wrapping_a_mixed_one_of_is_not_credited() {
    // CR 601.2h + CR 118.12a: the composite is fully paid, but its discard sits
    // inside a mixed `OneOf`, so the player can settle the cost without ever
    // discarding. Recursion must carry the "not guaranteed" answer up through
    // the composite rather than treating any nested discard as certain.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(
        &mut st,
        AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::OneOf {
                    costs: vec![
                        discard_cost(1),
                        AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 2 },
                        },
                    ],
                },
            ],
        },
    );

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn composite_wrapping_an_all_discard_one_of_is_credited() {
    // The positive twin: nesting must not lose a guaranteed discard either.
    // Every branch of the inner `OneOf` discards, so the composite does too.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(
        &mut st,
        AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::OneOf {
                    costs: vec![discard_cost(1), discard_cost(2)],
                },
            ],
        },
    );

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_engine_active");
    assert!(delta > 0.0);
}

#[test]
fn empty_one_of_cost_is_not_credited() {
    // Behavioral assertion, NOT a guard on `cost_discards`'s `!costs.is_empty()`
    // precondition: an empty `OneOf` never reaches that walk, because the engine's
    // `cost_categories()` gate reports no `Discards` category for a branch list
    // with nothing in it. Verified by mutation — deleting the emptiness check
    // leaves this green. It is kept because the OUTCOME is worth pinning (a cost
    // that pays nothing must not be credited a discard) at whichever layer
    // enforces it, but it must not be counted as covering that precondition.
    let mut st = state();
    engine_on_battlefield(&mut st, Some(discarded_engine_trigger()));
    let outlet = cost_paying_permanent(&mut st, AbilityCost::OneOf { costs: Vec::new() });

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = activate(outlet, 0);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(DiscardPayoffPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "discard_payoff_na");
    assert_eq!(delta, 0.0);
}
