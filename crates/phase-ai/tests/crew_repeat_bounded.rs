//! AI crew-repeat pathology (CR 702.122a) regression guards.
//!
//! Both Vehicles are Final Fantasy (FIN) 2/3 Artifact Vehicles that differ in
//! Crew requirement:
//! - Cargo Ship (#47): **Crew 1**.
//! - Adventurer's Airship: **Crew 2**.
//!
//! Once the AI has already crewed a Vehicle (it is now a creature and a valid
//! attacker), there is no benefit to activating Crew again — yet a pre-fix AI
//! keeps re-activating it at each priority window, tapping a fresh 1/1 body
//! each time until *every* creature it controls is tapped.
//!
//! These tests drive the AI's decision loop at PreCombatMain on a board of a
//! Vehicle plus three 1/1 bodies, apply each chosen action through the engine,
//! and assert correct play:
//! - a Crew 1 Vehicle is crewed exactly once (one body) then passes, and
//! - a Crew 2 Vehicle is crewed exactly once with the required **two** bodies
//!   (not one — insufficient total power — and not three — extra taps are
//!   waste), then passes.
//!
//! The first guards the crew-repeat regression (re-tapping every body); the
//! second guards against over-correction — the redundant-crew reject must not
//! prevent the AI from actually crewing a higher-N Vehicle.

use engine::game::engine::apply_as_current_for_simulation;
use engine::game::scenario::GameScenario;
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use phase_ai::config::AiConfig;
use phase_ai::search::choose_action;
use rand::rngs::SmallRng;
use rand::SeedableRng;

const P0: PlayerId = PlayerId(0);
/// Number of 1/1 bodies on the board the AI can tap to crew with.
const BODIES: usize = 3;
/// Safety bound: correct play crews once and then passes well under this.
const MAX_STEPS: usize = 50;

/// A 2/3 Artifact Vehicle with the given `crew_power`, modeled imperatively
/// like `crew_timing`'s `crew_fixture`. It entered a prior turn, so it is not
/// summoning-sick (CR 302.6).
fn add_vehicle(state: &mut GameState, name: &str, crew_power: u32) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        P0,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Vehicle".to_string());
    obj.base_card_types = obj.card_types.clone();
    obj.keywords.extend([
        Keyword::Flying,
        Keyword::Vigilance,
        Keyword::Crew {
            power: crew_power,
            once_per_turn: None,
        },
    ]);
    obj.base_power = Some(2);
    obj.base_toughness = Some(3);
    obj.power = Some(2);
    obj.toughness = Some(3);
    obj.entered_battlefield_turn = Some(0);
    obj.summoning_sick = false;
    id
}

/// PreCombatMain priority board of `BODIES` untapped 1/1 bodies plus `name`,
/// a Vehicle with `crew_power`, both controlled by P0.
fn setup_with(name: &str, crew_power: u32) -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..BODIES {
        scenario.add_vanilla(P0, 1, 1);
    }
    let mut state = scenario.build().state().clone();
    add_vehicle(&mut state, name, crew_power);
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state
}

/// every body tapped across all non-empty Crew selections, plus the action the
/// loop terminated on: `Some(GameAction::PassPriority)` when the AI passed;
/// `Some(action)` when the loop broke because applying that action errored;
/// `None` when `choose_action` returned no action or the step bound was hit.
///
/// Returning the terminating action lets both tests assert the loop ended by
/// PASSING, so a future regression that errored after exactly one tap could not
/// pass the body-count assertion without the AI ever having passed.
fn driven_crewed_bodies(state: &mut GameState) -> (Vec<ObjectId>, Option<GameAction>) {
    let config = AiConfig::default();
    let mut rng = SmallRng::seed_from_u64(42);
    let mut crewed_bodies: Vec<ObjectId> = Vec::new();

    for _ in 0..MAX_STEPS {
        let Some(action) = choose_action(state, P0, &config, &mut rng) else {
            return (crewed_bodies, None);
        };
        if let GameAction::CrewVehicle { creature_ids, .. } = &action {
            if !creature_ids.is_empty() {
                crewed_bodies.extend(creature_ids.iter().copied());
            }
        }
        if matches!(action, GameAction::PassPriority) {
            return (crewed_bodies, Some(action));
        }
        if apply_as_current_for_simulation(state, action.clone()).is_err() {
            return (crewed_bodies, Some(action));
        }
    }
    (crewed_bodies, None)
}

#[test]
fn ai_crews_crew1_vehicle_exactly_once() {
    // Cargo Ship (FIN #47): Crew 1. Correct play taps exactly one 1/1 body
    // then passes. The regression this guards is the pre-fix AI re-crewing at
    // every priority window — Crew's only effect (becoming a creature UEOT,
    // CR 702.122a) is already in force after the first crew, so every later
    // re-crew just taps a fresh body for nothing.
    let mut state = setup_with("Cargo Ship", 1);
    let (crewed, terminating_action) = driven_crewed_bodies(&mut state);
    assert_eq!(
        terminating_action.as_ref(),
        Some(&GameAction::PassPriority),
        "the AI decision loop must terminate by PASSING — not by an apply error or \
         the step bound, or a one-tap count would falsely pass without the AI ever passing"
    );
    assert_eq!(
        crewed.len(),
        1,
        "a Crew 1 Vehicle must be crewed exactly once (tap one body) then pass; \
         the AI tapped {:#?} ({})",
        crewed,
        crewed.len()
    );
}

#[test]
fn ai_crews_crew2_vehicle_with_exactly_the_required_two() {
    // Adventurer's Airship (FIN): Crew 2. The correct initial crew needs TWO
    // 1/1 bodies — not one (total power 1 < 2, insufficient) and not three
    // (Crew only needs total power N; extra taps are waste). Guards that the
    // redundant-crew reject does not over-correct and prevent the AI from
    // correctly crewing a higher-N Vehicle.
    //
    // NOTE (test asymmetry): this test intentionally does NOT fail on pre-fix
    // code — with 3 bodies, after a 2-body crew one 1/1 remains with power
    // 1 < 2, so no legal non-empty subset exists and pre-fix code also settles
    // on pass. It guards over-correction, which only exists post-fix, and the
    // exact-2 selection — not the original crew-repeat pathology (that is
    // Crew 1's test).
    let mut state = setup_with("Adventurer's Airship", 2);
    let (crewed, terminating_action) = driven_crewed_bodies(&mut state);
    assert_eq!(
        terminating_action.as_ref(),
        Some(&GameAction::PassPriority),
        "the AI decision loop must terminate by PASSING — not by an apply error or \
         the step bound, or a two-tap count would falsely pass without the AI ever passing"
    );
    assert_eq!(
        crewed.len(),
        2,
        "a Crew 2 Vehicle must be crewed exactly once with exactly TWO bodies \
         then pass; the AI tapped {:#?} ({})",
        crewed,
        crewed.len()
    );
}
