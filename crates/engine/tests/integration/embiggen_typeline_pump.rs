//! Runtime regression for issue #1997: Embiggen's pump scales with the target's
//! full typeline, not a fixed +1/+1.

use engine::game::scenario::{GameScenario, P0};
use engine::types::phase::Phase;

const EMBIGGEN_ORACLE: &str = "Until end of turn, target non-Brushwagg creature gets +1/+1 for each supertype, card type, and subtype it has.";

/// CR 205.2a + CR 205.3 + CR 205.4a: the target's typeline components are its
/// supertypes, card types, and subtypes. Glistener Elf has one card type
/// (Creature) and three subtypes (Phyrexian, Elf, Warrior), so Embiggen must
/// apply +4/+4 through the real cast pipeline.
#[test]
fn embiggen_pumps_for_target_typeline_component_count() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let target = scenario
        .add_creature(P0, "Glistener Elf", 1, 1)
        .with_subtypes(vec!["Phyrexian", "Elf", "Warrior"])
        .id();
    let embiggen = scenario
        .add_spell_to_hand_from_oracle(P0, "Embiggen", true, EMBIGGEN_ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(embiggen).target_object(target).resolve();

    outcome.assert_power_toughness(target, 5, 5);
}
