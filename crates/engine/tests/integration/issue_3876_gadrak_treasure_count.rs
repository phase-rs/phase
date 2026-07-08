//! Regression for issue #3876: Gadrak must create one Treasure per nontoken
//! creature that died this turn, not one every end step.
//!
//! https://github.com/phase-rs/phase/issues/3876

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const GADRAK_ORACLE: &str = "Flying\nGadrak can't attack unless you control four or more artifacts.\nAt the beginning of your end step, create a Treasure token for each nontoken creature that died this turn. (It's an artifact with \"{T}, Sacrifice this token: Add one mana of any color.\")";

const MURDER_ORACLE: &str = "Destroy target creature.";

fn treasure_count(runner: &engine::game::scenario::GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == P0 && o.zone == Zone::Battlefield && o.is_token && o.name == "Treasure"
        })
        .count()
}

#[test]
fn gadrak_creates_treasure_per_nontoken_creature_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Gadrak, the Crown-Scourge", 5, 4, GADRAK_ORACLE)
        .id();
    let doomed = scenario.add_creature(P0, "Doomed", 1, 1).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner.cast(murder).target_objects(&[doomed]).resolve();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        treasure_count(&runner),
        1,
        "one nontoken creature died → one Treasure at end step"
    );
}

#[test]
fn gadrak_creates_no_treasure_when_nothing_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Gadrak, the Crown-Scourge", 5, 4, GADRAK_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        treasure_count(&runner),
        0,
        "no deaths this turn → no Treasures"
    );
}

#[test]
fn gadrak_ignores_token_creatures_that_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Gadrak, the Crown-Scourge", 5, 4, GADRAK_ORACLE)
        .id();
    let doomed = scenario.add_creature(P0, "Doomed Token", 1, 1).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, MURDER_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        (0..3)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&doomed)
        .unwrap()
        .is_token = true;
    runner.cast(murder).target_objects(&[doomed]).resolve();
    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        treasure_count(&runner),
        0,
        "token creature deaths do not satisfy Gadrak's nontoken count"
    );
}
