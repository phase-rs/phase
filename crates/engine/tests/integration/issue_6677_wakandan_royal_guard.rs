//! Regression for issue #6677: Wakandan Royal Guard's conditional counter
//! override must keep using the creature chosen for its ETB trigger.
//!
//! The real Oracle text first targets a creature, then says "put two +1/+1
//! counters on it instead" when that creature is another Hero. The override's
//! bare pronoun must resolve to the original target, never Wakandan Royal Guard.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const WAKANDAN_ROYAL_GUARD_ORACLE: &str = "Vigilance\n\
    When this creature enters, put a +1/+1 counter on target creature. If that creature is another Hero, put two +1/+1 counters on it instead.";

fn p1p1_counters(state: &engine::types::game_state::GameState, object: ObjectId) -> u32 {
    state
        .objects
        .get(&object)
        .and_then(|card| card.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn resolve_guard_targeting_creature(target_is_hero: bool) -> (u32, u32, u32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let selected = if target_is_hero {
        scenario
            .add_creature(P0, "Selected Hero", 2, 2)
            .with_subtypes(vec!["Hero"])
            .id()
    } else {
        scenario.add_creature(P0, "Selected Soldier", 2, 2).id()
    };
    let unrelated_hero = scenario
        .add_creature(P0, "Unselected Hero", 2, 2)
        .with_subtypes(vec!["Hero"])
        .id();
    let guard = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Wakandan Royal Guard",
            2,
            2,
            WAKANDAN_ROYAL_GUARD_ORACLE,
        )
        .with_subtypes(vec!["Human", "Soldier", "Hero"])
        .with_mana_cost(ManaCost::generic(0))
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(guard).target_object(selected).resolve();

    (
        p1p1_counters(outcome.state(), selected),
        p1p1_counters(outcome.state(), guard),
        p1p1_counters(outcome.state(), unrelated_hero),
    )
}

/// CR 603.2 + CR 115.1d + CR 608.2c + CR 122.1: the ETB trigger targets the
/// selected Hero, and its matching instead-override puts two counters on that
/// same object. The zero-counter siblings prove the pronoun did not rebound to
/// either the resolving guard or another legal Hero.
#[test]
fn wakandan_royal_guard_doubles_counters_on_the_selected_hero() {
    let (selected, guard, unrelated_hero) = resolve_guard_targeting_creature(true);

    assert_eq!(
        selected, 2,
        "the selected Hero must receive two +1/+1 counters"
    );
    assert_eq!(
        guard, 0,
        "Wakandan Royal Guard must not receive the counters"
    );
    assert_eq!(
        unrelated_hero, 0,
        "an unselected Hero must not receive the counters"
    );
}

/// CR 603.2 + CR 115.1d + CR 608.2c + CR 122.1: when the chosen creature is
/// not a Hero, the override does not apply and the printed base instruction
/// still places exactly one counter on that chosen object.
#[test]
fn wakandan_royal_guard_keeps_one_counter_on_a_nonhero_target() {
    let (selected, guard, unrelated_hero) = resolve_guard_targeting_creature(false);

    assert_eq!(
        selected, 1,
        "the non-Hero target must receive the base instruction's one counter"
    );
    assert_eq!(
        guard, 0,
        "Wakandan Royal Guard must not receive the counter"
    );
    assert_eq!(
        unrelated_hero, 0,
        "an unrelated Hero must not satisfy the selected-target condition"
    );
}
