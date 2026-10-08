//! Regression for #8302: Liberator, Urza's Battlethopter's intervening-if
//! must compare against the mana actually spent to cast the triggering spell.

use engine::game::scenario::{GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;

const LIBERATOR_ORACLE: &str = "Flash\nFlying\nYou may cast colorless spells and artifact spells as though they had flash.\nWhenever you cast a spell, if the amount of mana spent to cast that spell is greater than Liberator's power, put a +1/+1 counter on Liberator.";

/// Cast a creature spell through the real pipeline, then report the counter
/// created by Liberator's intervening-if.
fn liberator_counters_after_casting(spell_cost: u32) -> u32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // CR 208.1: Liberator's printed power is 1.
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
    // CR 601.2h: this funds the normal payment path, which records actual
    // mana spent on the triggering spell rather than manually seeding it.
    for _ in 0..spell_cost {
        scenario.add_basic_land(P0, ManaColor::Green);
    }

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    outcome.counters(liberator, CounterType::Plus1Plus1)
}

/// CR 603.4: the intervening-if is checked through trigger creation and
/// resolution. The positive case proves the two negative cases reach that
/// path rather than passing because the trigger failed to parse or fire.
#[test]
fn liberator_gains_a_counter_only_when_mana_spent_exceeds_its_power() {
    assert_eq!(liberator_counters_after_casting(2), 1);
    assert_eq!(liberator_counters_after_casting(0), 0);
    assert_eq!(liberator_counters_after_casting(1), 0);
}
