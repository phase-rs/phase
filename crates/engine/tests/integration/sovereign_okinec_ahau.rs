//! Runtime coverage for Sovereign Okinec Ahau's member-driven attack trigger.
//!
//! Sovereign Okinec Ahau: "Whenever Sovereign Okinec Ahau attacks, for each
//! creature you control with power greater than that creature's base power, put
//! a number of +1/+1 counters on that creature equal to the difference."
//!
//! The scenario exercises the real Oracle parser and combat/trigger pipeline.
//! A pumped creature is the discriminating member: a layer-7c +2/+0 effect
//! makes current power 4 versus base power 2, producing two counters. An
//! unpumped creature remains at zero. The repeated body must rebind both the
//! ParentTarget recipient and the difference operands independently for each
//! member.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//! - CR 508.1a: the active player chooses which creatures attack.
//! - CR 603.2: the attack event automatically triggers the ability.
//! - CR 608.2c: the controller follows the instructions in order, including the
//!   per-member repeat and its counter instruction.
//! - CR 208.4b + CR 613.4a-b: base power includes layer-7a/7b set effects;
//!   counters are applied afterward.
//! - CR 122.1a + CR 613.4c: +1/+1 counters modify a creature's power and
//!   toughness in layer 7c.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, TargetFilter};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::StaticDefinition;

use super::rules::run_combat;

const SOVEREIGN_ORACLE: &str = "Ward {2}\nWhenever Sovereign Okinec Ahau attacks, for each creature you control with power greater than that creature's base power, put a number of +1/+1 counters on that creature equal to the difference.";

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|object| object.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

#[test]
fn attacks_put_difference_counters_on_each_pumped_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sovereign = scenario
        .add_creature(P0, "Sovereign Okinec Ahau", 3, 4)
        .from_oracle_text(SOVEREIGN_ORACLE)
        .id();

    let pumped = {
        let mut creature = scenario.add_creature(P0, "Pumped Creature", 2, 2);
        creature.with_static_definition(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddPower { value: 2 }]),
        );
        creature.id()
    };
    let unpumped = scenario.add_creature(P0, "Unpumped Creature", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![sovereign], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        plus_one_counters(&runner, pumped),
        2,
        "power 4 minus base power 2 must add two +1/+1 counters"
    );
    assert_eq!(
        plus_one_counters(&runner, unpumped),
        0,
        "a creature whose power equals base power is not in the repeated set"
    );
}

#[test]
fn attacks_use_the_layer_7b_base_power_not_printed_power() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sovereign = scenario
        .add_creature(P0, "Sovereign Okinec Ahau", 3, 4)
        .from_oracle_text(SOVEREIGN_ORACLE)
        .id();

    // The base-P/T setter applies before the separate current-power modifier.
    // The printed 1 is deliberately neither value.
    scenario.add_enchantment_from_oracle(
        P0,
        "Base-Form Anthem",
        "Creatures you control have base power and toughness 4/4.",
    );
    scenario.add_enchantment_from_oracle(P0, "Power Anthem", "Creatures you control get +3/+0.");
    let layered_creature = scenario.add_creature(P0, "Layered Creature", 1, 1).id();
    let second_layered_creature = {
        let mut creature = scenario.add_creature(P0, "Second Layered Creature", 2, 2);
        // This self static is a genuine layer-7c modifier: current 8/base 4
        // differs from the first recipient's current 7/base 4.
        creature.with_static_definition(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );
        creature.id()
    };
    let opponent_creature = scenario.add_creature(P1, "Opponent Creature", 2, 2).id();
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![sovereign], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        plus_one_counters(&runner, layered_creature),
        3,
        "current power 7 minus layer-7b base power 4 must add three counters, not six from printed power 1"
    );
    assert_eq!(
        plus_one_counters(&runner, second_layered_creature),
        4,
        "the second eligible creature must receive its own difference: 8 minus 4"
    );
    assert_eq!(
        plus_one_counters(&runner, sovereign),
        3,
        "Sovereign is eligible under the same layer effects and must receive its own difference"
    );
    assert_eq!(
        plus_one_counters(&runner, opponent_creature),
        0,
        "an opponent's creature is outside Sovereign's controlled-creature filter"
    );
}

#[test]
fn sovereign_attack_does_not_count_itself_without_a_power_modifier() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sovereign = scenario
        .add_creature(P0, "Sovereign Okinec Ahau", 3, 4)
        .from_oracle_text(SOVEREIGN_ORACLE)
        .id();
    let eligible = {
        let mut creature = scenario.add_creature(P0, "Eligible Attacker", 1, 1);
        creature.with_static_definition(
            StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddPower { value: 1 }]),
        );
        creature.id()
    };
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![sovereign, eligible], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        plus_one_counters(&runner, sovereign),
        0,
        "the source has no power/base-power difference and must not self-pump"
    );
    assert_eq!(
        plus_one_counters(&runner, eligible),
        1,
        "an independently eligible attacking creature must receive its own difference"
    );
}
