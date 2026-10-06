//! Regression for Flare of Faith's conditional `instead` override.

use engine::game::keywords::has_keyword;
use engine::game::scenario::{GameScenario, P0};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const FLARE_OF_FAITH_ORACLE: &str = "Target creature gets +2/+2 until end of turn. If it's a Human, instead it gets +3/+3 and gains indestructible until end of turn.";

#[test]
fn flare_of_faith_human_target_gets_the_override_and_leaves_decoy_unchanged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flare = scenario
        .add_spell_to_hand_from_oracle(P0, "Flare of Faith", true, FLARE_OF_FAITH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let human = scenario
        .add_creature(P0, "Human Target", 7, 5)
        .with_subtypes(vec!["Human"])
        .id();
    let non_human_decoy = scenario
        .add_creature(P0, "Non-Human Decoy", 11, 13)
        .with_subtypes(vec!["Bear"])
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(flare).target_object(human).resolve();
    let state = outcome.state();
    let human_after = &state.objects[&human];
    let decoy_after = &state.objects[&non_human_decoy];

    assert_eq!(human_after.power.expect("Human remains a creature") - 7, 3);
    assert_eq!(
        human_after.toughness.expect("Human remains a creature") - 5,
        3
    );
    assert!(has_keyword(human_after, &Keyword::Indestructible));
    assert_eq!(
        decoy_after.power.expect("decoy remains a creature") - 11,
        0,
        "only the selected target receives the override"
    );
    assert_eq!(
        decoy_after.toughness.expect("decoy remains a creature") - 13,
        0,
        "only the selected target receives the override"
    );
    assert!(!has_keyword(decoy_after, &Keyword::Indestructible));
}

#[test]
fn flare_of_faith_nonhuman_target_gets_base_pump_without_indestructible() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let flare = scenario
        .add_spell_to_hand_from_oracle(P0, "Flare of Faith", true, FLARE_OF_FAITH_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    let non_human = scenario
        .add_creature(P0, "Non-Human Target", 9, 4)
        .with_subtypes(vec!["Bear"])
        .id();
    let human_decoy = scenario
        .add_creature(P0, "Human Decoy", 3, 12)
        .with_subtypes(vec!["Human"])
        .id();
    let mut runner = scenario.build();

    let outcome = runner.cast(flare).target_object(non_human).resolve();
    let state = outcome.state();
    let target_after = &state.objects[&non_human];
    let decoy_after = &state.objects[&human_decoy];

    assert_eq!(
        target_after.power.expect("target remains a creature") - 9,
        2
    );
    assert_eq!(
        target_after.toughness.expect("target remains a creature") - 4,
        2
    );
    assert!(!has_keyword(target_after, &Keyword::Indestructible));
    assert_eq!(
        decoy_after.power.expect("decoy remains a creature") - 3,
        0,
        "the Human condition checks the selected target, not another creature"
    );
    assert_eq!(
        decoy_after.toughness.expect("decoy remains a creature") - 12,
        0,
        "the Human condition checks the selected target, not another creature"
    );
    assert!(!has_keyword(decoy_after, &Keyword::Indestructible));
}
