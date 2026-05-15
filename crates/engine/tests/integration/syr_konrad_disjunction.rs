//! Integration tests for issue #411 — Syr Konrad's three-clause disjunctive
//! graveyard trigger and its sibling cards (Dreadhound, Scrap Trawler).
//!
//! Pre-fix symptom: the parser captured only the first clause ("dies") and
//! emitted `Effect::Unimplemented { name: "or", ... }` for the rest, so a
//! 36-card mill triggered Syr Konrad zero times. Post-fix, each clause
//! produces its own `TriggerDefinition` and fires independently.
//!
//! The tests drive the engine pipeline end-to-end:
//!   `move_to_zone(...)` → `process_triggers(state, &events)` →
//!   stack drain via `runner.act(GameAction::PassPriority)`.
//!
//! No object zone, controller, or counter map is mutated directly to satisfy
//! preconditions — every trigger source is observed through the same
//! `ZoneChanged` event path the rest of the engine uses.
//!
//! CR references:
//!   - CR 603.2:   "Whenever ..." trigger semantics — each clause is its own ability.
//!   - CR 603.2c:  Each trigger fires once per trigger event occurrence.
//!   - CR 603.6c:  "from anywhere" / "from anywhere other than the battlefield"
//!     — never treated as a leaves-the-battlefield ability.
//!   - CR 603.10a: Look-back triggers (leaves graveyard).
//!   - CR 700.4:   "dies" means "is put into a graveyard from the battlefield."

use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::triggers::process_triggers;
use engine::game::zones::{create_object, move_to_zone};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const SYR_KONRAD_ORACLE: &str = "Whenever another creature dies, or a creature card is put into a graveyard from anywhere other than the battlefield, or a creature card leaves your graveyard, Syr Konrad, the Grim deals 1 damage to each opponent.";

/// Install a creature card in a non-battlefield zone. Use this for "library
/// creature about to be milled" / "graveyard creature about to be exiled"
/// fixtures — we do not call `move_to_zone` to get them there because the
/// trigger source for the assertion is the *subsequent* zone change.
fn install_creature_in_zone(
    state: &mut engine::types::game_state::GameState,
    owner: PlayerId,
    name: &str,
    zone: Zone,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, owner, name.to_string(), zone);
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    // No power/toughness needed for these tests — Syr Konrad's trigger keys
    // off zone change of a creature card, not its stats.
    id
}

/// Drain the stack by repeatedly passing priority. Mirrors the helper used in
/// `integration_landfall.rs::resolve_all_triggers` minus the TriggerTarget
/// branch (Syr Konrad's effect — DamageEachPlayer — has no choosable target).
fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..100 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// CR 603.2 + CR 700.4 + CR 603.6c + CR 603.10a: All three clauses of Syr
/// Konrad's disjunctive trigger must fire independently, each dealing 1 to
/// each opponent. In a three-player game, P0 controls Syr Konrad and P1/P2
/// are opponents — each clause should drain 1 life from both opponents.
#[test]
fn syr_konrad_fires_on_all_three_clauses_in_three_player_game() {
    let mut scenario = GameScenario::new_n_player(3, 42);

    // P0 controls Syr Konrad. The Oracle text is parsed via the full
    // synthesis pipeline so we exercise the same parse path that
    // `client/public/card-data.json` will follow.
    let _konrad_id = scenario
        .add_creature_from_oracle(PlayerId(0), "Syr Konrad, the Grim", 5, 4, SYR_KONRAD_ORACLE)
        .id();

    // Fixtures for the three clauses:
    //   - clause 1: a creature on the battlefield that will die.
    //   - clause 2: a creature card in the library that will be milled.
    //   - clause 3: a creature card in the graveyard that will be exiled.
    let battlefield_creature = scenario
        .add_creature(PlayerId(1), "Doomed Bystander", 2, 2)
        .id();
    let mut runner = scenario.build();
    let state = runner.state_mut();
    let library_creature =
        install_creature_in_zone(state, PlayerId(1), "Mill Fodder", Zone::Library);
    // CR 109.5: clause 3 ("leaves your graveyard") narrows valid_card.controller
    // to Syr Konrad's controller (P0). The leaving creature must be in P0's
    // graveyard for the trigger to fire — owner P2 would be filtered out by
    // the controller scope.
    let graveyard_creature =
        install_creature_in_zone(state, PlayerId(0), "Past Casualty", Zone::Graveyard);

    let p1_life_before = state
        .players
        .iter()
        .find(|p| p.id == PlayerId(1))
        .unwrap()
        .life;
    let p2_life_before = state
        .players
        .iter()
        .find(|p| p.id == PlayerId(2))
        .unwrap()
        .life;

    // Clause 1: battlefield → graveyard (dies).
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, battlefield_creature, Zone::Graveyard, &mut events);
        process_triggers(state, &events);
    }
    drain_stack(&mut runner);

    // Clause 2: library → graveyard (mill).
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, library_creature, Zone::Graveyard, &mut events);
        process_triggers(state, &events);
    }
    drain_stack(&mut runner);

    // Clause 3: graveyard → exile (leaves your graveyard).
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, graveyard_creature, Zone::Exile, &mut events);
        process_triggers(state, &events);
    }
    drain_stack(&mut runner);

    let p1_life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == PlayerId(1))
        .unwrap()
        .life;
    let p2_life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == PlayerId(2))
        .unwrap()
        .life;
    assert_eq!(
        p1_life_before - p1_life_after,
        3,
        "P1 should lose 3 life (1 per clause); got {} → {}",
        p1_life_before,
        p1_life_after
    );
    assert_eq!(
        p2_life_before - p2_life_after,
        3,
        "P2 should lose 3 life (1 per clause); got {} → {}",
        p2_life_before,
        p2_life_after
    );
}

/// Issue narrative regression: pre-fix, a 36-card mill triggered Syr Konrad
/// zero times because the "or a creature card is put into a graveyard from
/// anywhere other than the battlefield" clause was dropped. Post-fix, milling
/// N creatures fires the trigger N times.
///
/// CR 603.2c: Each individual zone-change event matches the trigger condition
/// independently — "one or more" batching does not apply here because Syr
/// Konrad's clauses are not "one or more" triggers.
#[test]
fn syr_konrad_clause_2_fires_on_each_milled_creature() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    let _konrad_id = scenario
        .add_creature_from_oracle(PlayerId(0), "Syr Konrad, the Grim", 5, 4, SYR_KONRAD_ORACLE)
        .id();
    let mut runner = scenario.build();

    let mill_count: usize = 5;
    let creatures: Vec<ObjectId> = (0..mill_count)
        .map(|i| {
            install_creature_in_zone(
                runner.state_mut(),
                PlayerId(1),
                &format!("Mill Fodder {i}"),
                Zone::Library,
            )
        })
        .collect();

    let p1_life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == PlayerId(1))
        .unwrap()
        .life;

    for c in creatures {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, c, Zone::Graveyard, &mut events);
        process_triggers(state, &events);
        drain_stack(&mut runner);
    }

    let p1_life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == PlayerId(1))
        .unwrap()
        .life;
    assert_eq!(
        p1_life_before - p1_life_after,
        mill_count as i32,
        "Syr Konrad should fire once per milled creature; expected {} life lost, got {}",
        mill_count,
        p1_life_before - p1_life_after
    );
}

/// CR 700.4 + CR 603.2: Dreadhound's two-clause disjunction should fire each
/// clause independently. Dies → 1 life lost; library → graveyard mill →
/// another 1 life lost.
#[test]
fn dreadhound_fires_on_dies_and_mill() {
    const DREADHOUND_ORACLE: &str = "Whenever a creature dies or a creature card is put into a graveyard from a library, each opponent loses 1 life.";
    let mut scenario = GameScenario::new_n_player(2, 42);
    let _dreadhound_id = scenario
        .add_creature_from_oracle(PlayerId(0), "Dreadhound", 6, 6, DREADHOUND_ORACLE)
        .id();
    let battlefield_creature = scenario
        .add_creature(PlayerId(1), "Doomed Bystander", 2, 2)
        .id();
    let mut runner = scenario.build();
    let library_creature = install_creature_in_zone(
        runner.state_mut(),
        PlayerId(1),
        "Mill Fodder",
        Zone::Library,
    );

    let p1_life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == PlayerId(1))
        .unwrap()
        .life;

    // Dies clause.
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, battlefield_creature, Zone::Graveyard, &mut events);
        process_triggers(state, &events);
    }
    drain_stack(&mut runner);

    // Library → graveyard clause.
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, library_creature, Zone::Graveyard, &mut events);
        process_triggers(state, &events);
    }
    drain_stack(&mut runner);

    let p1_life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == PlayerId(1))
        .unwrap()
        .life;
    assert_eq!(
        p1_life_before - p1_life_after,
        2,
        "Dreadhound should fire on both clauses, draining 2 life from each opponent"
    );
}
