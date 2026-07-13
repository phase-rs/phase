//! Regression coverage for Death-Priest of Myrkul's Oxford-comma subtype anthem.
//!
//! Drives the real Oracle parse → static synthesis → layer pipeline. The middle
//! Vampire item is the revert-failing case: the former two-way split silently
//! omitted it, while Skeleton and Zombie still received the pump.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const DEATH_PRIEST_STATIC: &str = "Skeletons, Vampires, and Zombies you control get +1/+1.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let object = &runner.state().objects[&id];
    (
        object.power.expect("creature has power"),
        object.toughness.expect("creature has toughness"),
    )
}

#[test]
fn death_priest_oxford_anthem_pumps_every_listed_subtype_you_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _death_priest = scenario
        .add_creature_from_oracle(P0, "Death-Priest of Myrkul", 2, 2, DEATH_PRIEST_STATIC)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let skeleton = scenario
        .add_creature(P0, "Skeleton", 1, 1)
        .with_subtypes(vec!["Skeleton"])
        .id();
    let vampire = scenario
        .add_creature(P0, "Vampire", 1, 1)
        .with_subtypes(vec!["Vampire"])
        .id();
    let zombie = scenario
        .add_creature(P0, "Zombie", 1, 1)
        .with_subtypes(vec!["Zombie"])
        .id();
    let unrelated = scenario
        .add_creature(P0, "Elf", 1, 1)
        .with_subtypes(vec!["Elf"])
        .id();
    let opponent_vampire = scenario
        .add_creature(P1, "Opponent Vampire", 1, 1)
        .with_subtypes(vec!["Vampire"])
        .id();

    let mut runner = scenario.build();

    assert_eq!(effective_pt(&mut runner, skeleton), (2, 2));
    assert_eq!(
        effective_pt(&mut runner, vampire),
        (2, 2),
        "the middle Oxford-list item must receive the anthem"
    );
    assert_eq!(effective_pt(&mut runner, zombie), (2, 2));
    assert_eq!(effective_pt(&mut runner, unrelated), (1, 1));
    assert_eq!(effective_pt(&mut runner, opponent_vampire), (1, 1));
}
