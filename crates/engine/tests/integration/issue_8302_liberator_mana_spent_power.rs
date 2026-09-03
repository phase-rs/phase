//! Issue #8302 — Liberator, Urza's Battlethopter's intervening-if must gate the
//! +1/+1 counter on the mana actually spent.
//!
//! "Whenever you cast a spell, if the amount of mana spent to cast that spell is
//! greater than Liberator's power, put a +1/+1 counter on Liberator."
//!
//! The comparison clause parser only accepted `its mana value` as the
//! right-hand subject, so after `normalize_card_name_refs` collapsed
//! "Liberator's power" to `~'s power` the whole extractor bailed and the
//! trigger lowered with `condition: None`. Every spell cast then added a
//! counter regardless of mana spent.
//!
//! CR 603.4: the intervening-if is checked when the trigger event occurs and
//! again on resolution. CR 601.2h: the amount of mana spent is recorded by the
//! real payment path, so these tests pay for each spell through the cast
//! pipeline rather than stamping state by hand.
//!
//! CR 208.1: Liberator is printed 1/2, so with a power of 1 the three cases
//! below straddle the `greater than` boundary exactly: 0 < 1, 1 == 1, 2 > 1.
//! The `1 == 1` case is the one that proves the comparator is GT and not GE.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

const LIBERATOR_ORACLE: &str = "Flash\nFlying\nYou may cast colorless spells and artifact spells as though they had flash.\nWhenever you cast a spell, if the amount of mana spent to cast that spell is greater than Liberator's power, put a +1/+1 counter on Liberator.";

/// Put a printed 1/2 Liberator on P0's battlefield, then cast a vanilla
/// creature spell costing `spell_cost` generic mana through the real pipeline.
/// Returns the number of +1/+1 counters on Liberator afterwards.
fn cast_spell_costing(spell_cost: u32) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // CR 208.1: printed power 1 is the value the intervening-if compares against.
    let liberator = scenario
        .add_creature_from_oracle(
            P0,
            "Liberator, Urza's Battlethopter",
            1,
            2,
            LIBERATOR_ORACLE,
        )
        .id();

    let spell = scenario
        .add_creature_to_hand(P0, "Test Bear", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: spell_cost,
        })
        .id();
    // CR 601.2h: real lands funding a real payment, so the engine records the
    // spent amount authentically instead of the test asserting on a stamp.
    for _ in 0..spell_cost {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    outcome.counters(liberator, CounterType::Plus1Plus1)
}

/// Mana spent (2) > Liberator's power (1): the trigger's condition holds and
/// the counter is added. This is the positive reach-guard for the two negative
/// cases below — it proves the trigger fires, is put on the stack, and resolves
/// at all, so a `0` there means "condition correctly false", not "trigger never
/// happened".
#[test]
fn liberator_gains_counter_when_mana_spent_exceeds_its_power() {
    assert_eq!(
        cast_spell_costing(2),
        1,
        "mana spent (2) > power (1): CR 603.4 intervening-if holds, counter added"
    );
}

/// Mana spent (0) < Liberator's power (1): no counter.
///
/// Before the fix the condition was `None`, so this cast added a counter
/// anyway — this assertion flips on revert.
#[test]
fn liberator_gains_no_counter_when_mana_spent_is_below_its_power() {
    assert_eq!(
        cast_spell_costing(0),
        0,
        "mana spent (0) < power (1): the intervening-if fails, no counter"
    );
}

/// Boundary: mana spent (1) == Liberator's power (1). "Greater than" is a
/// strict comparison (`Comparator::GT`, not `GE`), so equality must not add a
/// counter. Also flips on revert, and additionally catches a GT→GE regression
/// that the `<` case alone would miss.
#[test]
fn liberator_gains_no_counter_when_mana_spent_equals_its_power() {
    assert_eq!(
        cast_spell_costing(1),
        0,
        "mana spent (1) == power (1): 'greater than' is strict, no counter"
    );
}
