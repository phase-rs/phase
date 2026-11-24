//! Conquest Mission (Star Trek: Strange New Worlds / TRK):
//!
//! "Whenever a nontoken artifact you control enters, put a quest counter on
//! this enchantment. Create a 1/1 colorless Borg artifact creature token. If
//! this enchantment has six or more quest counters on it, create two of those
//! tokens instead."
//!
//! CR 614.1a + CR 111.1 + CR 608.2c: "create two of those tokens instead"
//! restates the preceding TOKEN instruction, not the first printed instruction
//! (the counter), so the override replaces the one-token creation and the
//! counter is always added.

use crate::support::shared_card_db;
use engine::game::coverage::card_face_gaps;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn counters(runner: &GameRunner, id: ObjectId, kind: CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&kind)
        .copied()
        .unwrap_or(0)
}

fn contains_unimplemented(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def
            .sub_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
        || def
            .else_ability
            .as_deref()
            .is_some_and(contains_unimplemented)
}

fn borg_count(runner: &GameRunner) -> usize {
    let state = runner.state();
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Borg"))
        .count()
}

/// Enter a nontoken artifact with `quest` counters already on the enchantment;
/// returns the runner after the trigger resolves and the enchantment's id.
fn enter_artifact(quest: u32) -> (GameRunner, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mission = scenario.add_real_card(P0, "Conquest Mission", Zone::Battlefield, db);
    scenario.with_counter(mission, parse_counter_type("quest"), quest);
    // A printed nontoken artifact (a {0} vanilla artifact creature) enters.
    let artifact = scenario.add_real_card(P0, "Phyrexian Walker", Zone::Hand, db);
    let mut runner = scenario.build();

    // Reach guard: no Unimplemented residue, so the counts below cannot pass
    // vacuously on a half-parsed trigger.
    let triggers = &runner.state().objects[&mission].trigger_definitions;
    assert!(
        triggers.iter_all().all(|t| t
            .definition
            .execute
            .as_deref()
            .is_some_and(|d| !contains_unimplemented(d))),
        "Conquest Mission must parse with no Unimplemented effects"
    );

    runner.cast(artifact).resolve();
    (runner, mission)
}

#[test]
fn below_six_counters_creates_one_borg() {
    let (runner, mission) = enter_artifact(2);
    assert_eq!(counters(&runner, mission, parse_counter_type("quest")), 3);
    assert_eq!(borg_count(&runner), 1, "fewer than six counters: one token");
}

#[test]
fn reaching_six_counters_creates_two_borg_instead_of_one() {
    // The counter is put first (5 -> 6), so the threshold is met on this very
    // trigger: two tokens instead of one, never three.
    let (runner, mission) = enter_artifact(5);
    assert_eq!(counters(&runner, mission, parse_counter_type("quest")), 6);
    assert_eq!(borg_count(&runner), 2, "six counters: two tokens instead");
}

#[test]
fn counter_is_added_even_when_override_applies() {
    let (runner, mission) = enter_artifact(9);
    assert_eq!(counters(&runner, mission, parse_counter_type("quest")), 10);
    assert_eq!(borg_count(&runner), 2);
}

/// CR 603.2: the trigger is "Whenever a NONTOKEN artifact you control enters".
/// Quark's printed upkeep trigger creates a Treasure token (an artifact) under
/// P0's control, which must not trigger the Mission; a printed nontoken artifact
/// cast afterwards does, in the same game (the paired positive).
#[test]
fn an_artifact_token_entering_does_not_trigger_the_mission() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let mission = scenario.add_real_card(P0, "Conquest Mission", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Quark, Humble Proprietor", Zone::Battlefield, db);
    let artifact = scenario.add_real_card(P0, "Phyrexian Walker", Zone::Hand, db);
    scenario.with_library_top(P0, &["Island"]);
    let mut runner = scenario.build();

    // Quark's upkeep trigger creates the Treasure token for real.
    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();
    let treasure_tokens = {
        let state = runner.state();
        state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.is_token && o.card_types.subtypes.iter().any(|s| s == "Treasure"))
            .count()
    };
    assert_eq!(treasure_tokens, 1, "REACH: an artifact token entered");
    assert_eq!(counters(&runner, mission, parse_counter_type("quest")), 0);
    assert_eq!(borg_count(&runner), 0, "a token entering does not trigger");

    // Paired positive: a nontoken artifact entering does.
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.cast(artifact).resolve();
    assert_eq!(counters(&runner, mission, parse_counter_type("quest")), 1);
    assert_eq!(borg_count(&runner), 1);
}

/// CR 111.1 + CR 608.2c: the whole card is supported only when the "instead"
/// override carries a real token instruction ("those tokens" resolved), not an
/// `Unimplemented` placeholder. The fixture entry is the production writer's
/// output; the runtime witnesses above prove the behavior.
#[test]
fn conquest_mission_whole_card_has_no_coverage_gap() {
    let db = shared_card_db().expect("integration card fixture must load");
    let face = db
        .get_face_by_name("Conquest Mission")
        .expect("Conquest Mission is in the integration fixture");
    assert!(
        !face.triggers.is_empty(),
        "REACH: the trigger line parses: {:?}",
        face.triggers
    );
    assert!(
        card_face_gaps(face).is_empty(),
        "Conquest Mission must have no coverage gaps: {:?}",
        card_face_gaps(face)
    );
}
