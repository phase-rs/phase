//! Issue #3866 — Insidious Roots must trigger once when creature cards leave
//! your graveyard, not twice.
//!
//! https://github.com/phase-rs/phase/issues/3866

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::move_to_zone;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const REANIMATE_ORACLE: &str =
    "Return target creature card from your graveyard to the battlefield.";

fn card_db() -> &'static CardDatabase {
    shared_card_db().expect("integration card fixture must load")
}

fn plant_token_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.zone == Zone::Battlefield
                && o.is_token
                && o.card_types.subtypes.iter().any(|s| s == "Plant")
        })
        .count()
}

fn insidious_roots_stack_triggers(
    runner: &engine::game::scenario::GameRunner,
    roots: ObjectId,
) -> usize {
    runner
        .state()
        .stack
        .iter()
        .filter(|e| e.source_id == roots)
        .count()
}

#[test]
fn issue_3866_insidious_roots_trigger_definitions_not_duplicated() {
    let db = card_db();
    let face = db
        .get_face_by_name("Insidious Roots")
        .expect("Insidious Roots must be in card-data");
    assert_eq!(
        face.triggers.len(),
        1,
        "Insidious Roots must have exactly one printed trigger definition"
    );
    assert!(
        face.triggers[0].batched,
        "graveyard-leave trigger must be batched (CR 603.2c)"
    );
}

#[test]
fn issue_3866_insidious_roots_fires_once_when_creature_leaves_graveyard() {
    let db = card_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let roots = scenario.add_real_card(P0, "Insidious Roots", Zone::Battlefield, db);
    let gy_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);

    let mut runner = scenario.build();
    let trigger_count = runner
        .state()
        .objects
        .get(&roots)
        .map(|o| o.trigger_definitions.len())
        .unwrap_or(0);
    assert_eq!(
        trigger_count, 1,
        "runtime object must carry a single trigger definition"
    );

    let plants_before = plant_token_count(&runner);

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        gy_creature,
        Zone::Battlefield,
        &mut events,
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());

    assert_eq!(
        insidious_roots_stack_triggers(&runner, roots),
        1,
        "exactly one Insidious Roots trigger must be on the stack"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&gy_creature].zone,
        Zone::Battlefield,
        "creature should be on the battlefield"
    );
    assert_eq!(
        plant_token_count(&runner) - plants_before,
        1,
        "Insidious Roots must create exactly one Plant token"
    );
}

#[test]
fn issue_3866_insidious_roots_fires_once_when_two_creatures_leave_graveyard() {
    let db = card_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let roots = scenario.add_real_card(P0, "Insidious Roots", Zone::Battlefield, db);
    let gy_a = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let gy_b = scenario.add_real_card(P0, "Elvish Mystic", Zone::Graveyard, db);

    let mut runner = scenario.build();
    let plants_before = plant_token_count(&runner);

    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), gy_a, Zone::Battlefield, &mut events);
    move_to_zone(runner.state_mut(), gy_b, Zone::Battlefield, &mut events);
    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());

    assert_eq!(
        insidious_roots_stack_triggers(&runner, roots),
        1,
        "batched graveyard-leave trigger must register once for simultaneous leaves (CR 603.2c)"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        plant_token_count(&runner) - plants_before,
        1,
        "one batched trigger must create one Plant token even when two creatures leave"
    );
}

#[test]
fn issue_3866_double_process_triggers_on_same_events_is_regression_guard() {
    let db = card_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let roots = scenario.add_real_card(P0, "Insidious Roots", Zone::Battlefield, db);
    let gy_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);

    let mut runner = scenario.build();

    let mut events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        gy_creature,
        Zone::Battlefield,
        &mut events,
    );

    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    let after_first = insidious_roots_stack_triggers(&runner, roots);

    engine::game::triggers::process_triggers(runner.state_mut(), &events);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    let after_second = insidious_roots_stack_triggers(&runner, roots);

    assert_eq!(
        after_first, 1,
        "first trigger scan must enqueue exactly one Insidious Roots trigger"
    );
    assert_eq!(
        after_second, after_first,
        "re-scanning the same graveyard-leave events must not enqueue a duplicate trigger"
    );
}

#[test]
fn issue_3866_insidious_roots_fires_again_when_same_card_leaves_graveyard_twice_same_turn() {
    let db = card_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let roots = scenario.add_real_card(P0, "Insidious Roots", Zone::Battlefield, db);
    let gy_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);

    let mut runner = scenario.build();
    let plants_before = plant_token_count(&runner);

    let mut first_leave = Vec::new();
    move_to_zone(
        runner.state_mut(),
        gy_creature,
        Zone::Battlefield,
        &mut first_leave,
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &first_leave);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    assert_eq!(
        insidious_roots_stack_triggers(&runner, roots),
        1,
        "first graveyard leave must trigger once"
    );
    runner.advance_until_stack_empty();

    let mut return_to_graveyard = Vec::new();
    move_to_zone(
        runner.state_mut(),
        gy_creature,
        Zone::Graveyard,
        &mut return_to_graveyard,
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &return_to_graveyard);

    let mut second_leave = Vec::new();
    move_to_zone(
        runner.state_mut(),
        gy_creature,
        Zone::Battlefield,
        &mut second_leave,
    );
    engine::game::triggers::process_triggers(runner.state_mut(), &second_leave);
    engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
    assert_eq!(
        insidious_roots_stack_triggers(&runner, roots),
        1,
        "second distinct graveyard leave by the same card must trigger again"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        plant_token_count(&runner) - plants_before,
        2,
        "each distinct graveyard leave must create one Plant token"
    );
}

#[test]
fn issue_3866_insidious_roots_fires_once_via_reanimate_spell() {
    let db = card_db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let roots = scenario.add_real_card(P0, "Insidious Roots", Zone::Battlefield, db);
    let gy_creature = scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    let reanimate = scenario
        .add_spell_to_hand_from_oracle(P0, "Reanimate", true, REANIMATE_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let plants_before = plant_token_count(&runner);

    runner.cast(reanimate).target_object(gy_creature).resolve();

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&gy_creature].zone,
        Zone::Battlefield,
        "reanimated creature should be on the battlefield"
    );
    assert_eq!(
        plant_token_count(&runner) - plants_before,
        1,
        "Insidious Roots must create exactly one Plant token when a creature leaves the graveyard via Reanimate"
    );
    assert!(
        insidious_roots_stack_triggers(&runner, roots) <= 1,
        "at most one Insidious Roots trigger should remain on stack after resolution"
    );
}
