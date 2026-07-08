//! Blessed Orator — "Other creatures you control get +0/+1."
//!
//! Regression coverage for the continuous static P/T anthem building block on
//! the "Other" + controller filter axis with NO subtype narrowing. Axes: the
//! "Other" self-exclusion (the source does NOT buff itself), the "you control"
//! exclusion, all-creature scope (no subtype), and buff lifetime (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const BLESSED_ORATOR: &str = "Other creatures you control get +0/+1.";

fn effective_pt(runner: &mut GameRunner, id: ObjectId) -> (i32, i32) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

#[test]
fn blessed_orator_buffs_other_creatures_you_control_not_itself() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a 1/4 creature carrying the anthem (real parse + synthesis pipeline).
    let orator = scenario
        .add_creature_from_oracle(P0, "Blessed Orator", 1, 4, BLESSED_ORATOR)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();

    // Two other creatures you control (any subtype) — both get +0/+1.
    let ally_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let ally_goblin = scenario
        .add_creature(P0, "Raging Goblin", 1, 1)
        .with_subtypes(vec!["Goblin"])
        .id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario
        .add_creature(P1, "Runeclaw Bear", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    // CR 613.4c: other creatures you control get +0/+1.
    assert_eq!(
        effective_pt(&mut runner, ally_bear),
        (2, 3),
        "another creature you control gets +0/+1: 2/2 → 2/3"
    );
    assert_eq!(
        effective_pt(&mut runner, ally_goblin),
        (1, 2),
        "another creature you control gets +0/+1: 1/1 → 1/2"
    );

    // CR 109.5: "Other" excludes the source itself — it stays at base 1/4.
    assert_eq!(
        effective_pt(&mut runner, orator),
        (1, 4),
        "Blessed Orator must NOT buff itself ('Other' excludes the source)"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (2, 2),
        "an opponent's creature must NOT be buffed ('you control')"
    );
}

#[test]
fn blessed_orator_buff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let orator = scenario
        .add_creature_from_oracle(P0, "Blessed Orator", 1, 4, BLESSED_ORATOR)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let ally_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, ally_bear),
        (2, 3),
        "baseline: ally buffed to 2/3 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != orator);
        state.objects.remove(&orator);
    }
    assert_eq!(
        effective_pt(&mut runner, ally_bear),
        (2, 2),
        "ally reverts to base 2/2 once the source is gone"
    );
}
