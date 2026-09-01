//! Unit tests for `policies::self_bounce_target` — CR 608.2c "return a land you
//! control" self-bounce target choice (#4730). No `#[cfg(test)]` in SOURCE
//! files; tests live here.
//!
//! `return_desirability` is the pure core (source-loop guard + tapped/untapped
//! tempo ordering); the composed `verdict` runs against a real `PolicyContext`
//! built over a `WaitingFor::EffectZoneChoice` + `SelectCards` candidate, the
//! seam the engine surfaces for a non-targeted battlefield→hand land bounce.

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::zones::create_object;
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{PolicyId, PolicyRegistry, PolicyVerdict, TacticalPolicy};
use crate::policies::self_bounce_target::*;

const AI: PlayerId = PlayerId(0);

fn state() -> GameState {
    GameState::new(FormatConfig::standard(), 2, 42)
}

// ─── return_desirability (pure core) ────────────────────────────────────────

#[test]
fn source_ranks_below_every_other_land() {
    let mut st = state();
    let source = land(&mut st, 1, true); // Karoo enters tapped
    let tapped = land(&mut st, 2, true);
    let untapped = land(&mut st, 3, false);

    let d_source = SelfBounceTargetPolicy::return_desirability(&st, source, source);
    let d_tapped = SelfBounceTargetPolicy::return_desirability(&st, tapped, source);
    let d_untapped = SelfBounceTargetPolicy::return_desirability(&st, untapped, source);

    assert_eq!(d_source, RETURN_SOURCE_PENALTY);
    assert_eq!(d_tapped, RETURN_TAPPED_BONUS);
    assert_eq!(d_untapped, RETURN_UNTAPPED_PENALTY);
    // The whole point of #4730: the just-played bounce-land is the worst return,
    // and a spent (tapped) land is the best.
    assert!(
        d_source < d_untapped && d_untapped < d_tapped,
        "ordering source < untapped < tapped, got {d_source} {d_untapped} {d_tapped}"
    );
}

// ─── verdict (composed over EffectZoneChoice) ───────────────────────────────

#[test]
fn verdict_prefers_a_spent_land_over_the_bounce_source() {
    // Pool: the just-played Karoo (source, tapped) + a spent basic + an untapped
    // basic. Returning the source must score lowest, the tapped basic highest.
    let d_source = verdict_delta(&[(true, true), (false, true), (false, false)], &[0]);
    let d_tapped = verdict_delta(&[(true, true), (false, true), (false, false)], &[1]);
    let d_untapped = verdict_delta(&[(true, true), (false, true), (false, false)], &[2]);
    assert_eq!(d_source, RETURN_SOURCE_PENALTY);
    assert_eq!(d_tapped, RETURN_TAPPED_BONUS);
    assert_eq!(d_untapped, RETURN_UNTAPPED_PENALTY);
    assert!(
        d_source < d_untapped && d_untapped < d_tapped,
        "AI must rank the just-played bounce-land last"
    );
}

#[test]
fn verdict_is_neutral_for_a_non_land_pool() {
    // A value self-bounce of creatures (blink / save-from-removal) must be left
    // to the value policies — this policy only governs land bounces.
    assert_eq!(
        verdict_delta_kinds(
            &[(true, true), (false, true)],
            &[1],
            false,
            Some(Zone::Hand),
            EffectKind::ChangeZone
        ),
        0.0
    );
}

#[test]
fn verdict_is_neutral_for_non_hand_destination() {
    // Battlefield→exile / other ChangeZone choices are not the "return to hand"
    // bounce class.
    assert_eq!(
        verdict_delta_kinds(
            &[(true, true), (false, true)],
            &[1],
            true,
            Some(Zone::Exile),
            EffectKind::ChangeZone
        ),
        0.0
    );
}

// ─── helpers ────────────────────────────────────────────────────────────────

fn land(state: &mut GameState, idx: u64, tapped: bool) -> ObjectId {
    make_object(state, idx, true, tapped)
}

fn make_object(state: &mut GameState, idx: u64, is_land: bool, tapped: bool) -> ObjectId {
    let oid = create_object(
        state,
        CardId(idx),
        AI,
        format!("Perm {idx}"),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&oid).unwrap();
    obj.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![if is_land {
            CoreType::Land
        } else {
            CoreType::Creature
        }],
        subtypes: Vec::new(),
    };
    obj.tapped = tapped;
    oid
}

/// Build the land pool (index 0 is the bounce source) and score the given
/// selection under a battlefield→hand `ChangeZone` land bounce, via a direct
/// `verdict` call.
fn verdict_delta(pool_spec: &[(bool, bool)], selection: &[usize]) -> f64 {
    verdict_delta_kinds(
        pool_spec,
        selection,
        true,
        Some(Zone::Hand),
        EffectKind::ChangeZone,
    )
}

/// Direct-call variant; always `Some` (panics on `Reject`).
fn verdict_delta_kinds(
    pool_spec: &[(bool, bool)],
    selection: &[usize],
    lands: bool,
    destination: Option<Zone>,
    effect_kind: EffectKind,
) -> f64 {
    eval(pool_spec, selection, lands, destination, effect_kind, false)
        .expect("direct verdict always returns a score")
}

/// The score for `selection` as the **production registry** produces it —
/// classify → filter by `DecisionKind` → run `activation` → `verdict`. `None`
/// when `SelfBounceTargetPolicy` did not run at all (unregistered, or the
/// `EffectZoneChoice` no longer routes to its declared kind).
fn routed_delta(pool_spec: &[(bool, bool)], selection: &[usize]) -> Option<f64> {
    eval(
        pool_spec,
        selection,
        true,
        Some(Zone::Hand),
        EffectKind::ChangeZone,
        true,
    )
}

/// `pool_spec[i] = (unused, tapped)`; index 0 is always the ability source.
/// `lands` toggles land vs creature pool objects. `route` runs the full
/// `PolicyRegistry` instead of calling the policy directly.
fn eval(
    pool_spec: &[(bool, bool)],
    selection: &[usize],
    lands: bool,
    destination: Option<Zone>,
    effect_kind: EffectKind,
    route: bool,
) -> Option<f64> {
    let mut st = state();
    let pool: Vec<ObjectId> = pool_spec
        .iter()
        .enumerate()
        .map(|(i, &(_, tapped))| make_object(&mut st, 100 + i as u64, lands, tapped))
        .collect();
    let source_id = pool[0];
    let selected: Vec<ObjectId> = selection.iter().map(|&i| pool[i]).collect();

    let decision = AiDecisionContext {
        waiting_for: WaitingFor::EffectZoneChoice {
            player: AI,
            cards: pool,
            count: 1,
            min_count: 1,
            up_to: false,
            source_id,
            effect_kind,
            zone: Zone::Battlefield,
            destination,
            enter_tapped: EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: Vec::new(),
            count_param: 0,
            mass_library_order: None,
            library_position: None,
            is_cost_payment: false,
            enters_modified_if: None,
            duration: None,
        },
        candidates: Vec::new(),
    };
    let candidate = CandidateAction {
        action: GameAction::SelectCards { cards: selected },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
    };
    let config = AiConfig::default();
    let aicontext = AiContext::empty(&config.weights);
    let ctx = PolicyContext {
        state: &st,
        decision: &decision,
        candidate: &candidate,
        ai_player: AI,
        config: &config,
        context: &aicontext,
        cast_facts: None,
        search_depth: SearchDepth::Root,
    };

    let verdict = if route {
        PolicyRegistry::default()
            .verdicts(&ctx)
            .into_iter()
            .find(|(id, _)| *id == PolicyId::SelfBounceTarget)
            .map(|(_, verdict)| verdict)?
    } else {
        SelfBounceTargetPolicy.verdict(&ctx)
    };
    match verdict {
        PolicyVerdict::Score { delta, .. } => Some(delta),
        PolicyVerdict::Reject { reason } => panic!("unexpected Reject: {reason:?}"),
    }
}

// ─── production seam (registry routing) ─────────────────────────────────────

#[test]
fn registry_registers_the_policy() {
    assert!(PolicyRegistry::default().has_policy(PolicyId::SelfBounceTarget));
}

/// End-to-end routing: `WaitingFor::EffectZoneChoice` classifies to
/// `DecisionKind::ActivateAbility`, the policy declares that kind, and the three
/// land selections come out ordered tapped-other > untapped-other > source. A
/// direct-`verdict` probe would stay green even if the policy were dropped from
/// the registry or the routing changed; this asserts the shipped seam.
#[test]
fn registry_routes_the_land_bounce_ordering() {
    let pool = &[(true, true), (false, true), (false, false)];
    let source = routed_delta(pool, &[0]).expect("source selection must reach the policy");
    let tapped = routed_delta(pool, &[1]).expect("tapped selection must reach the policy");
    let untapped = routed_delta(pool, &[2]).expect("untapped selection must reach the policy");
    assert!(
        source < untapped && untapped < tapped,
        "routed ordering source < untapped < tapped, got {source} {untapped} {tapped}"
    );
}
