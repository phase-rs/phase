//! AI crew-repeat pathology (CR 702.122a) regression guard.
//!
//! Cargo Ship (Final Fantasy #47) is a 2/3 Artifact Vehicle with Flying,
//! Vigilance, and **Crew 1**: "Tap any number of creatures you control with
//! total power 1 or more: This Vehicle becomes an artifact creature until end
//! of turn."
//!
//! Once the AI has already crewed the Vehicle (it is now a creature and a
//! valid attacker), there is no benefit to activating Crew again — yet a
//! pre-fix AI keeps re-activating it at each priority window, tapping a fresh
//! 1/1 body each time until *every* creature it controls is tapped.
//!
//! This test drives the AI's decision loop at PreCombatMain on a board of
//! Cargo Ship plus three 1/1 bodies, applies each chosen action through the
//! engine, and asserts the fix: the AI crews exactly once (one body) and then
//! passes, rather than tapping every body via repeated redundant Crew
//! activations. The regression it guards is the pre-fix AI that re-crewed at
//! every priority window until every creature it controlled was tapped.

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
/// Cargo Ship is Crew 1, so one 1/1 body is the minimum tap to crew it.
const BODIES: usize = 3;
/// Safety bound: correct play crews once and then passes well under this.
const MAX_STEPS: usize = 50;

/// Cargo Ship (FIN #47): 2/3 Artifact Vehicle, Flying, Vigilance, Crew 1.
/// Modeled imperatively like `crew_timing`'s `crew_fixture`; it entered a prior
/// turn, so it is not summoning-sick (CR 302.6).
fn add_cargo_ship(state: &mut GameState) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        P0,
        "Cargo Ship".to_string(),
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
            power: 1,
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

fn setup() -> GameState {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Three pre-existing 1/1 bodies (the "1/1 tokens") the AI can tap to crew.
    for _ in 0..BODIES {
        scenario.add_vanilla(P0, 1, 1);
    }
    let mut state = scenario.build().state().clone();
    add_cargo_ship(&mut state);
    state.waiting_for = WaitingFor::Priority { player: P0 };
    state
}

#[test]
fn ai_crews_crewed_vehicle_exactly_once() {
    let mut state = setup();
    let config = AiConfig::default();
    let mut rng = SmallRng::seed_from_u64(42);

    // `crewed_bodies` records each body the AI chose in a Crew selection
    // (non-empty `creature_ids`). Correct play crews exactly once — the
    // minimum tap count — then passes. The regression we guard is the pre-fix
    // AI tapping every body via redundant re-crews.
    let mut crewed_bodies: Vec<ObjectId> = Vec::new();

    for _ in 0..MAX_STEPS {
        let action = choose_action(&state, P0, &config, &mut rng);
        let Some(action) = action else { break };
        if matches!(action, GameAction::PassPriority) {
            break;
        }
        if let GameAction::CrewVehicle { creature_ids, .. } = &action {
            if !creature_ids.is_empty() {
                crewed_bodies.extend(creature_ids.iter().copied());
            }
        }
        if apply_as_current_for_simulation(&mut state, action.clone()).is_err() {
            break;
        }
    }

    assert_eq!(
        crewed_bodies.len(),
        1,
        "the AI must crew the Vehicle exactly once (tap one body) then pass; \
         it tapped {:#?} ({})",
        crewed_bodies,
        crewed_bodies.len()
    );
}
