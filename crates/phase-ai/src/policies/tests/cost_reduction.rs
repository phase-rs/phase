//! Unit tests for `policies::cost_reduction` — CR 601.2f cost-reduction
//! deployment policy. No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! Direct-`verdict` tests cover each branch; a registry-routed regression
//! exercises the production seam (registration + `CastSpell` routing).

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::{
    ControllerRef, PlayerScope, QuantityRef, StaticCondition, StaticDefinition, TargetFilter,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::statics::{CostModifyMode, StaticMode};
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::cost_reduction::{CostReductionFeature, COST_REDUCTION_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::cost_reduction::*;
use crate::policies::registry::{
    PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);

fn state() -> GameState {
    GameState::new(FormatConfig::standard(), 2, 42)
}

fn generic(amount: u32) -> ManaCost {
    ManaCost::Cost {
        shards: Vec::new(),
        generic: amount,
    }
}

/// A CR 601.2f board-wide reducer static scoped to spells YOU cast.
fn reduce_your_spells(amount: u32, mode: CostModifyMode) -> StaticDefinition {
    let mut def = StaticDefinition::new(StaticMode::ModifyCost {
        mode,
        amount: generic(amount),
        spell_filter: None,
        dynamic_count: None,
    });
    def.affected = Some(TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::You),
        ..Default::default()
    }));
    def
}

/// A card in the AI's hand. `statics` are attached as live `static_definitions`,
/// which is what the policy classifies at decision time.
fn hand_card(
    state: &mut GameState,
    name: &str,
    core: CoreType,
    mana_value: u32,
    statics: Vec<StaticDefinition>,
) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, name.to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(core);
    obj.mana_cost = generic(mana_value);
    for def in statics {
        obj.static_definitions.push(def);
    }
    state.players[AI.0 as usize].hand.push_back(id);
    (id, card_id)
}

/// The Goblin Electromancer shape: a two-mana creature that discounts your spells.
fn reducer_in_hand(state: &mut GameState, mana_value: u32) -> (ObjectId, CardId) {
    hand_card(
        state,
        "Cost Reducer",
        CoreType::Creature,
        mana_value,
        vec![reduce_your_spells(1, CostModifyMode::Reduce)],
    )
}

fn plain_spell_in_hand(state: &mut GameState, mana_value: u32) -> (ObjectId, CardId) {
    hand_card(
        state,
        "Plain Spell",
        CoreType::Sorcery,
        mana_value,
        Vec::new(),
    )
}

fn land_in_hand(state: &mut GameState) -> (ObjectId, CardId) {
    hand_card(state, "Island", CoreType::Land, 0, Vec::new())
}

fn feature(commitment: f32) -> CostReductionFeature {
    CostReductionFeature {
        reducer_count: 4,
        total_discount: 4,
        discounted_count: 20,
        commitment,
    }
}

fn session(commitment: f32) -> AiSession {
    let features = DeckFeatures {
        cost_reduction: feature(commitment),
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
    let features = DeckFeatures {
        cost_reduction: feature(COST_REDUCTION_FLOOR - 0.01),
        ..Default::default()
    };
    assert!(CostReductionPolicy
        .activation(&features, &state(), AI)
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let features = DeckFeatures {
        cost_reduction: feature(0.8),
        ..Default::default()
    };
    let activation = CostReductionPolicy.activation(&features, &state(), AI);
    assert_eq!(activation, Some(0.8));
}

// ─── verdict: deploy the engine ──────────────────────────────────────────────

#[test]
fn deploying_reducer_with_spells_in_hand_scores_positive() {
    let mut st = state();
    let (reducer, card) = reducer_in_hand(&mut st, 2);
    plain_spell_in_hand(&mut st, 3);
    plain_spell_in_hand(&mut st, 4);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_deploy_engine");
    assert!(
        delta > 0.0,
        "expected a positive deployment credit, got {delta}"
    );
}

#[test]
fn deploying_reducer_with_empty_grip_is_neutral() {
    let mut st = state();
    let (reducer, card) = reducer_in_hand(&mut st, 2);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_no_future_casts");
    assert_eq!(delta, 0.0);
}

#[test]
fn lands_in_hand_are_not_future_casts() {
    // CR 305.1: playing a land is not casting a spell, so a grip of lands gives
    // the reducer nothing to discount.
    let mut st = state();
    let (reducer, card) = reducer_in_hand(&mut st, 2);
    land_in_hand(&mut st);
    land_in_hand(&mut st);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (_, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_no_future_casts");
}

#[test]
fn deployment_credit_is_bounded_by_the_caps() {
    // A huge grip and a huge discount must stay within the constants' product,
    // so a stacked hand cannot push one deployment into the critical band.
    let mut st = state();
    let (reducer, card) = hand_card(
        &mut st,
        "Big Reducer",
        CoreType::Creature,
        2,
        vec![reduce_your_spells(9, CostModifyMode::Reduce)],
    );
    for _ in 0..12 {
        plain_spell_in_hand(&mut st, 3);
    }

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (delta, _) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    let ceiling = config.policy_penalties.cost_reduction_deploy_bonus
        * f64::from(MAX_REWARDED_DISCOUNT * MAX_REWARDED_FUTURE_CASTS);
    assert!(
        delta <= ceiling + f64::EPSILON,
        "delta {delta} exceeded ceiling {ceiling}"
    );
}

#[test]
fn taxing_permanent_is_not_a_deployment_engine() {
    // Thalia raises costs (CR 601.2f); deploying her is not a discount plan.
    let mut st = state();
    let (thalia, card) = hand_card(
        &mut st,
        "Thalia",
        CoreType::Creature,
        2,
        vec![reduce_your_spells(1, CostModifyMode::Raise)],
    );
    plain_spell_in_hand(&mut st, 3);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(thalia, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn self_cost_reduction_is_not_a_deployment_engine() {
    // CR 113.6: "this spell costs {1} less" discounts only itself.
    let mut st = state();
    let mut def = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: generic(1),
        spell_filter: None,
        dynamic_count: None,
    });
    def.affected = Some(TargetFilter::SelfRef);
    let (obj, card) = hand_card(&mut st, "Self Discount", CoreType::Artifact, 4, vec![def]);
    plain_spell_in_hand(&mut st, 3);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let (_, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_na");
}

// ─── verdict: defer to the engine ────────────────────────────────────────────

#[test]
fn casting_past_a_cheaper_reducer_is_penalized() {
    let mut st = state();
    reducer_in_hand(&mut st, 2);
    let (spell, card) = plain_spell_in_hand(&mut st, 4);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(spell, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_defer_to_engine");
    assert!(delta < 0.0, "expected a sequencing penalty, got {delta}");
}

#[test]
fn casting_past_a_more_expensive_reducer_is_neutral() {
    // The reducer does not fit ahead of this spell, so there is nothing to defer.
    let mut st = state();
    reducer_in_hand(&mut st, 5);
    let (spell, card) = plain_spell_in_hand(&mut st, 2);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(spell, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn casting_with_no_reducer_in_hand_is_neutral() {
    let mut st = state();
    let (spell, card) = plain_spell_in_hand(&mut st, 4);
    plain_spell_in_hand(&mut st, 2);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(spell, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_na");
    assert_eq!(delta, 0.0);
}

// ─── production seam ─────────────────────────────────────────────────────────

#[test]
fn registry_routes_cast_spell_to_this_policy() {
    // Guards the wiring, not just the classifier: registration in
    // `PolicyRegistry::default` plus `CastSpell` routing must reach `verdict`.
    let mut st = state();
    let (reducer, card) = reducer_in_hand(&mut st, 2);
    plain_spell_in_hand(&mut st, 3);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let verdicts =
        PolicyRegistry::default().verdicts(&ctx(&st, &candidate, &decision, &context, &config));

    let found = verdicts
        .iter()
        .find(|(id, _)| *id == PolicyId::CostReduction)
        .map(|(_, verdict)| verdict.clone())
        .expect("CostReductionPolicy must be registered and routed for CastSpell");
    let (delta, reason) = score_of(found);
    assert_eq!(reason.kind, "cost_reduction_deploy_engine");
    assert!(delta > 0.0);
}

#[test]
fn registry_stays_silent_below_the_activation_floor() {
    let mut st = state();
    let (reducer, card) = reducer_in_hand(&mut st, 2);
    plain_spell_in_hand(&mut st, 3);

    let config = AiConfig::default();
    let context = context(&config, session(COST_REDUCTION_FLOOR - 0.01));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let verdicts =
        PolicyRegistry::default().verdicts(&ctx(&st, &candidate, &decision, &context, &config));

    assert!(
        !verdicts
            .iter()
            .any(|(id, _)| *id == PolicyId::CostReduction),
        "policy must not contribute below its activation floor"
    );
}

// ─── review #6743: live gating + spell_filter narrowing ──────────────────────

/// A reducer whose `spell_filter` restricts it to one core type.
fn typed_reducer_in_hand(
    state: &mut GameState,
    mana_value: u32,
    only: TypeFilter,
) -> (ObjectId, CardId) {
    let mut def = StaticDefinition::new(StaticMode::ModifyCost {
        mode: CostModifyMode::Reduce,
        amount: generic(1),
        spell_filter: Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![only],
            controller: Some(ControllerRef::You),
            ..Default::default()
        })),
        dynamic_count: None,
    });
    def.affected = Some(TargetFilter::Typed(TypedFilter {
        controller: Some(ControllerRef::You),
        ..Default::default()
    }));
    hand_card(
        state,
        "Typed Reducer",
        CoreType::Creature,
        mana_value,
        vec![def],
    )
}

#[test]
fn deploy_credit_counts_only_spells_the_reducer_discounts() {
    // An artifact-only reducer with a grip of sorceries discounts nothing.
    let mut st = state();
    let (reducer, card) = typed_reducer_in_hand(&mut st, 2, TypeFilter::Artifact);
    plain_spell_in_hand(&mut st, 3);
    plain_spell_in_hand(&mut st, 4);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (_, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_no_future_casts");
}

#[test]
fn deploy_credit_counts_matching_spells() {
    // Same reducer, but the grip is artifacts — now it pays off.
    let mut st = state();
    let (reducer, card) = typed_reducer_in_hand(&mut st, 2, TypeFilter::Artifact);
    hand_card(&mut st, "Artifact A", CoreType::Artifact, 3, Vec::new());
    hand_card(&mut st, "Artifact B", CoreType::Artifact, 4, Vec::new());

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_deploy_engine");
    assert!(delta > 0.0);
}

#[test]
fn defer_penalty_does_not_fire_for_a_spell_the_reducer_cannot_reduce() {
    // An artifact-only reducer must not penalize casting a sorcery.
    let mut st = state();
    typed_reducer_in_hand(&mut st, 2, TypeFilter::Artifact);
    let (spell, card) = plain_spell_in_hand(&mut st, 4);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(spell, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn conditional_reducer_earns_no_deploy_credit() {
    // CR 601.2f: an "as long as" gate has no truthful answer for a card still in
    // hand, so the policy fails off rather than banking a discount it cannot
    // guarantee. Mirrors the casting authority's condition gate.
    let mut st = state();
    let mut def = reduce_your_spells(1, CostModifyMode::Reduce);
    def.condition = Some(StaticCondition::IsPresent {
        filter: Some(TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Artifact],
            controller: Some(ControllerRef::You),
            ..Default::default()
        })),
    });
    let (reducer, card) = hand_card(&mut st, "Conditional", CoreType::Creature, 2, vec![def]);
    plain_spell_in_hand(&mut st, 3);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "cost_reduction_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn zero_dynamic_multiplier_earns_no_deploy_credit() {
    // "costs {1} less for each card in your hand" with the multiplier resolving
    // to zero discounts nothing, so it must not be credited.
    let mut st = state();
    let mut def = reduce_your_spells(1, CostModifyMode::Reduce);
    let StaticMode::ModifyCost {
        ref mut dynamic_count,
        ..
    } = def.mode
    else {
        unreachable!("constructed as ModifyCost")
    };
    *dynamic_count = Some(QuantityRef::GraveyardSize {
        player: PlayerScope::Controller,
    });
    let (reducer, card) = hand_card(&mut st, "Scaling", CoreType::Creature, 2, vec![def]);
    plain_spell_in_hand(&mut st, 3);

    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(reducer, card);
    let decision = priority_decision(&candidate);
    let (delta, reason) =
        score_of(CostReductionPolicy.verdict(&ctx(&st, &candidate, &decision, &context, &config)));

    // Empty graveyard → multiplier 0 → no live discount at all.
    assert_eq!(reason.kind, "cost_reduction_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn positive_dynamic_multiplier_scales_the_deploy_credit() {
    // The same scaling reducer with a stocked graveyard credits more than the
    // per-unit amount, matching the multiplier the resolver would apply.
    let mut flat = state();
    let (flat_reducer, flat_card) = reducer_in_hand(&mut flat, 2);
    plain_spell_in_hand(&mut flat, 3);

    let mut scaled = state();
    let mut def = reduce_your_spells(1, CostModifyMode::Reduce);
    let StaticMode::ModifyCost {
        ref mut dynamic_count,
        ..
    } = def.mode
    else {
        unreachable!("constructed as ModifyCost")
    };
    *dynamic_count = Some(QuantityRef::GraveyardSize {
        player: PlayerScope::Controller,
    });
    let (scaled_reducer, scaled_card) =
        hand_card(&mut scaled, "Scaling", CoreType::Creature, 2, vec![def]);
    plain_spell_in_hand(&mut scaled, 3);
    for _ in 0..3 {
        let cid = CardId(scaled.next_object_id);
        let id = create_object(
            &mut scaled,
            cid,
            AI,
            "Dead Card".to_string(),
            Zone::Graveyard,
        );
        scaled.players[AI.0 as usize].graveyard.push_back(id);
    }

    let config = AiConfig::default();
    let context = context(&config, session(0.8));

    let flat_candidate = cast(flat_reducer, flat_card);
    let flat_decision = priority_decision(&flat_candidate);
    let (flat_delta, _) = score_of(CostReductionPolicy.verdict(&ctx(
        &flat,
        &flat_candidate,
        &flat_decision,
        &context,
        &config,
    )));

    let scaled_candidate = cast(scaled_reducer, scaled_card);
    let scaled_decision = priority_decision(&scaled_candidate);
    let (scaled_delta, reason) = score_of(CostReductionPolicy.verdict(&ctx(
        &scaled,
        &scaled_candidate,
        &scaled_decision,
        &context,
        &config,
    )));

    assert_eq!(reason.kind, "cost_reduction_deploy_engine");
    assert!(
        scaled_delta > flat_delta,
        "multiplier>1 must out-score the flat reducer: {scaled_delta} vs {flat_delta}"
    );
}
