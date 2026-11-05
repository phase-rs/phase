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

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityDefinition, Effect};
use engine::types::counter::{parse_counter_type, CounterType};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const CONQUEST_MISSION: &str = "Whenever a nontoken artifact you control enters, put a quest counter on this enchantment. Create a 1/1 colorless Borg artifact creature token. If this enchantment has six or more quest counters on it, create two of those tokens instead.";

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
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mission = scenario
        .add_enchantment_from_oracle(P0, "Conquest Mission", CONQUEST_MISSION)
        .id();
    scenario.with_counter(mission, parse_counter_type("quest"), quest);
    let artifact = scenario
        .add_artifact_to_hand_from_oracle(P0, "Bauble", "")
        .id();
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
