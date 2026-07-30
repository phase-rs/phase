//! Unit tests for `policies::vehicle_deployment` — CR 702.122a crewable-Vehicle
//! deployment. No `#[cfg(test)]` in SOURCE files; tests live here.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::StaticDefinition;
use engine::types::ability::TargetFilter;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::player::PlayerId;
use engine::types::statics::{CrewAction, CrewContributionKind, StaticMode};
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::vehicles::{VehiclesFeature, VEHICLES_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{
    PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::policies::vehicle_deployment::*;
use crate::session::AiSession;

const AI: PlayerId = PlayerId(0);

fn state() -> GameState {
    GameState::new(FormatConfig::standard(), 2, 42)
}

/// A Vehicle in hand with `Crew N`.
fn vehicle_in_hand(state: &mut GameState, crew: u32) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Copter".to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Vehicle".to_string());
    obj.keywords.push(Keyword::Crew {
        power: crew,
        once_per_turn: None,
    });
    state.players[AI.0 as usize].hand.push_back(id);
    (id, card_id)
}

/// A non-Vehicle artifact in hand.
fn artifact_in_hand(state: &mut GameState) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Signet".to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    state.players[AI.0 as usize].hand.push_back(id);
    (id, card_id)
}

/// An untapped creature the AI controls, with `power`.
fn creature(state: &mut GameState, power: i32, controller: PlayerId) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        "Bear".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(power.max(1));
    id
}

fn feature(commitment: f32) -> VehiclesFeature {
    VehiclesFeature {
        vehicle_count: 5,
        total_crew_cost: 10,
        crew_body_count: 10,
        total_crew_power: 20,
        commitment,
    }
}

fn session(commitment: f32) -> AiSession {
    let features = DeckFeatures {
        vehicles: feature(commitment),
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

fn verdict_for(
    st: &GameState,
    obj: ObjectId,
    card: CardId,
    commitment: f32,
) -> (f64, PolicyReason) {
    let config = AiConfig::default();
    let context = context(&config, session(commitment));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    score_of(VehicleDeploymentPolicy.verdict(&ctx(st, &candidate, &decision, &context, &config)))
}

// ─── activation ──────────────────────────────────────────────────────────────

#[test]
fn activation_opts_out_below_floor() {
    let features = DeckFeatures {
        vehicles: feature(VEHICLES_FLOOR - 0.01),
        ..Default::default()
    };
    assert!(VehicleDeploymentPolicy
        .activation(&features, &state(), AI)
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let features = DeckFeatures {
        vehicles: feature(0.8),
        ..Default::default()
    };
    assert_eq!(
        VehicleDeploymentPolicy.activation(&features, &state(), AI),
        Some(0.8)
    );
}

// ─── verdict ─────────────────────────────────────────────────────────────────

#[test]
fn crewable_vehicle_scores_positive() {
    let mut st = state();
    creature(&mut st, 3, AI);
    let (obj, card) = vehicle_in_hand(&mut st, 2);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_crewable");
    assert!(delta > 0.0, "expected positive credit, got {delta}");
}

#[test]
fn uncrewable_vehicle_is_neutral_not_penalized() {
    // No bodies: the Vehicle would enter as a blank. Withholding the bonus is the
    // whole signal — this policy never vetoes a deployment.
    let mut st = state();
    let (obj, card) = vehicle_in_hand(&mut st, 3);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_uncrewable");
    assert_eq!(delta, 0.0, "must never penalize, only withhold");
}

#[test]
fn crew_power_sums_across_multiple_bodies() {
    // CR 702.122a: "any number of other untapped creatures with TOTAL power N".
    let mut st = state();
    creature(&mut st, 1, AI);
    creature(&mut st, 1, AI);
    creature(&mut st, 1, AI);
    let (obj, card) = vehicle_in_hand(&mut st, 3);
    let (_, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_crewable");
}

#[test]
fn insufficient_total_power_is_uncrewable() {
    let mut st = state();
    creature(&mut st, 1, AI);
    creature(&mut st, 1, AI);
    let (_, reason) = {
        let (obj, card) = vehicle_in_hand(&mut st, 5);
        verdict_for(&st, obj, card, 0.8)
    };
    assert_eq!(reason.kind, "vehicle_deployment_uncrewable");
}

#[test]
fn tapped_creatures_do_not_crew() {
    // CR 702.122a requires UNTAPPED creatures.
    let mut st = state();
    let body = creature(&mut st, 4, AI);
    st.objects.get_mut(&body).unwrap().tapped = true;
    let (obj, card) = vehicle_in_hand(&mut st, 2);
    let (_, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_uncrewable");
}

#[test]
fn opponent_creatures_do_not_crew() {
    // CR 702.122a: creatures YOU control.
    let mut st = state();
    creature(&mut st, 5, PlayerId(1));
    let (obj, card) = vehicle_in_hand(&mut st, 2);
    let (_, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_uncrewable");
}

#[test]
fn non_vehicle_is_not_applicable() {
    let mut st = state();
    creature(&mut st, 5, AI);
    let (obj, card) = artifact_in_hand(&mut st);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn surplus_credit_is_bounded_by_the_cap() {
    let mut st = state();
    for _ in 0..12 {
        creature(&mut st, 5, AI);
    }
    let (obj, card) = vehicle_in_hand(&mut st, 1);
    let (delta, _) = verdict_for(&st, obj, card, 0.8);
    let config = AiConfig::default();
    let ceiling = config.policy_penalties.vehicle_deployment_bonus * 2.0;
    assert!(
        delta <= ceiling + f64::EPSILON,
        "delta {delta} exceeded ceiling {ceiling}"
    );
}

#[test]
fn exact_crew_requirement_is_crewable() {
    // Boundary: total power exactly equals N (CR 702.122a says "N or greater").
    let mut st = state();
    creature(&mut st, 2, AI);
    let (obj, card) = vehicle_in_hand(&mut st, 2);
    let (_, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_crewable");
}

// ─── production seam ─────────────────────────────────────────────────────────

#[test]
fn registry_routes_cast_spell_to_this_policy() {
    let mut st = state();
    creature(&mut st, 3, AI);
    let (obj, card) = vehicle_in_hand(&mut st, 2);
    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let verdicts =
        PolicyRegistry::default().verdicts(&ctx(&st, &candidate, &decision, &context, &config));
    let found = verdicts
        .iter()
        .find(|(id, _)| *id == PolicyId::VehicleDeployment)
        .map(|(_, v)| v.clone())
        .expect("VehicleDeploymentPolicy must be registered and routed for CastSpell");
    let (delta, reason) = score_of(found);
    assert_eq!(reason.kind, "vehicle_deployment_crewable");
    assert!(delta > 0.0);
}

#[test]
fn registry_stays_silent_below_the_activation_floor() {
    let mut st = state();
    creature(&mut st, 3, AI);
    let (obj, card) = vehicle_in_hand(&mut st, 2);
    let config = AiConfig::default();
    let context = context(&config, session(VEHICLES_FLOOR - 0.01));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let verdicts =
        PolicyRegistry::default().verdicts(&ctx(&st, &candidate, &decision, &context, &config));
    assert!(
        !verdicts
            .iter()
            .any(|(id, _)| *id == PolicyId::VehicleDeployment),
        "policy must not contribute below its activation floor"
    );
}

// ─── review #6790 blocker: a subtype-only Vehicle has no crew ability ───────

/// A Vehicle by TYPE LINE only — no `Keyword::Crew`. CR 702.122a makes Crew an
/// activated ability, which a subtype alone does not grant.
fn subtype_only_vehicle_in_hand(state: &mut GameState) -> (ObjectId, CardId) {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, AI, "Odd Vehicle".to_string(), Zone::Hand);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Vehicle".to_string());
    state.players[AI.0 as usize].hand.push_back(id);
    (id, card_id)
}

#[test]
fn subtype_only_vehicle_is_not_applicable_on_an_empty_board() {
    // The exact regression: a synthesised `Crew 0` satisfied `0 >= 0` and scored
    // a live crew bonus for a permanent that can never be crewed.
    let mut st = state();
    let (obj, card) = subtype_only_vehicle_in_hand(&mut st);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn subtype_only_vehicle_is_not_applicable_with_a_full_board() {
    // Same, with a board that WOULD satisfy any real crew cost — proving the
    // rejection comes from the missing ability, not from an empty battlefield.
    let mut st = state();
    creature(&mut st, 5, AI);
    creature(&mut st, 5, AI);
    let (obj, card) = subtype_only_vehicle_in_hand(&mut st);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_na");
    assert_eq!(delta, 0.0);
}

#[test]
fn registry_stays_silent_for_a_subtype_only_vehicle() {
    // At the production seam, per the review: registered + routed, still neutral.
    let mut st = state();
    creature(&mut st, 5, AI);
    let (obj, card) = subtype_only_vehicle_in_hand(&mut st);
    let config = AiConfig::default();
    let context = context(&config, session(0.8));
    let candidate = cast(obj, card);
    let decision = priority_decision(&candidate);
    let verdicts =
        PolicyRegistry::default().verdicts(&ctx(&st, &candidate, &decision, &context, &config));
    // `.expect`, not `if let` — the invariant is "registered AND routed AND
    // neutral". Skipping the assertions when the policy is absent would let a
    // routing regression (activation regressing to `None`) pass silently, which
    // is the opposite of what this test claims to enforce.
    let found = verdicts
        .iter()
        .find(|(id, _)| *id == PolicyId::VehicleDeployment)
        .map(|(_, v)| v.clone())
        .expect("VehicleDeploymentPolicy must be registered and routed for CastSpell");
    let (delta, reason) = score_of(found);
    assert_eq!(reason.kind, "vehicle_deployment_na");
    assert_eq!(delta, 0.0);
}

// ─── review #6790 NB: the engine crew authorities must actually be consulted ──

/// Attach a static to a battlefield object.
fn attach_static(state: &mut GameState, id: ObjectId, mode: StaticMode) {
    let obj = state.objects.get_mut(&id).unwrap();
    obj.static_definitions.push(StaticDefinition::new(mode));
}

#[test]
fn cant_crew_creature_does_not_contribute() {
    // CR 702.122d, via `object_has_cant_crew`. Without that call the 4-power body
    // would cover Crew 3 and this would score.
    let mut st = state();
    let body = creature(&mut st, 4, AI);
    attach_static(&mut st, body, StaticMode::CantCrew);
    let (obj, card) = vehicle_in_hand(&mut st, 3);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_uncrewable");
    assert_eq!(delta, 0.0);
}

#[test]
fn power_delta_contribution_is_honored() {
    // CR 702.122a "as though its power were N greater", via
    // `object_crew_power_contribution`. A raw `.power` sum would read 1 and call
    // this uncrewable; the authority reads 3.
    let mut st = state();
    let body = creature(&mut st, 1, AI);
    attach_static(
        &mut st,
        body,
        StaticMode::CrewContribution {
            kind: CrewContributionKind::PowerDelta { delta: 2 },
            actions: vec![CrewAction::Crew],
        },
    );
    let (obj, card) = vehicle_in_hand(&mut st, 3);
    let (_, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(
        reason.kind, "vehicle_deployment_crewable",
        "the engine's crew-power authority must be consulted, not raw power"
    );
}

#[test]
fn toughness_instead_of_power_contribution_is_honored() {
    // CR 702.122a "using its toughness rather than its power".
    let mut st = state();
    let body = creature(&mut st, 1, AI);
    st.objects.get_mut(&body).unwrap().toughness = Some(4);
    attach_static(
        &mut st,
        body,
        StaticMode::CrewContribution {
            kind: CrewContributionKind::ToughnessInsteadOfPower,
            actions: vec![CrewAction::Crew],
        },
    );
    let (obj, card) = vehicle_in_hand(&mut st, 4);
    let (_, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_crewable");
}

// ─── review #6790 blocker: CantTap creatures cannot pay Crew ─────────────────

/// A creature the engine's crew path rejects: `StaticMode::CantTap`.
///
/// Mirrors the engine's own fixture (`restrictions.rs::creature_with_cant_tap`) —
/// the static goes on BOTH `static_definitions` and `base_static_definitions`,
/// then `evaluate_layers` runs, because `object_cant_tap` has an O(1) fast path
/// gated on a static-kind presence index. Skipping the layer pass would leave
/// that index empty, the fast path would return `false`, and this test would pass
/// for the wrong reason.
fn cant_tap_creature(state: &mut GameState, power: i32) -> ObjectId {
    let id = creature(state, power, AI);
    {
        let obj = state.objects.get_mut(&id).unwrap();
        let def = StaticDefinition::new(StaticMode::CantTap).affected(TargetFilter::SelfRef);
        obj.static_definitions.push(def.clone());
        Arc::make_mut(&mut obj.base_static_definitions).push(def);
    }
    engine::game::layers::evaluate_layers(state);
    id
}

#[test]
fn cant_tap_creature_cannot_pay_crew() {
    // CR 702.122a via the engine's `creature_can_pay_crew` authority: a CantTap
    // 3/3 is untapped and controlled, but cannot legally be tapped, so it
    // contributes nothing toward Crew 3. The previous hand-rolled filter omitted
    // `object_cant_tap` and counted it, awarding the crewable bonus for a Vehicle
    // that can never be crewed.
    let mut st = state();
    let restricted = cant_tap_creature(&mut st, 3);
    // Guard the fixture itself through the public authority: if the static did
    // not take effect, this test would pass vacuously. Paired with a plain body
    // of the same power so the assertion discriminates the restriction rather
    // than merely observing a `false`.
    let unrestricted = creature(&mut st, 3, AI);
    assert!(
        !engine::game::engine::creature_can_pay_crew(&st, restricted, AI),
        "fixture must actually be under a CantTap restriction"
    );
    assert!(
        engine::game::engine::creature_can_pay_crew(&st, unrestricted, AI),
        "an identical unrestricted body must be able to pay crew"
    );
    // Remove the control body again so the crew total is the restricted one only.
    st.battlefield.retain(|id| *id != unrestricted);
    let (obj, card) = vehicle_in_hand(&mut st, 3);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_uncrewable");
    assert_eq!(delta, 0.0);
}

#[test]
fn cant_tap_creature_alongside_a_legal_body_still_undercounts() {
    // Discriminating: one legal 1-power body plus a CantTap 3/3 totals 1, not 4,
    // so Crew 3 stays out of reach.
    let mut st = state();
    cant_tap_creature(&mut st, 3);
    creature(&mut st, 1, AI);
    let (obj, card) = vehicle_in_hand(&mut st, 3);
    let (_, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_uncrewable");
}

#[test]
fn an_unrestricted_body_of_the_same_power_does_crew() {
    // The positive control for the two tests above: identical power, no CantTap,
    // so the difference is provably the restriction and not the power total.
    let mut st = state();
    creature(&mut st, 3, AI);
    let (obj, card) = vehicle_in_hand(&mut st, 3);
    let (delta, reason) = verdict_for(&st, obj, card, 0.8);
    assert_eq!(reason.kind, "vehicle_deployment_crewable");
    assert!(delta > 0.0);
}
