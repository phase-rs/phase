//! Maraxus — "Creatures you control get +1/+0."
//!
//! Regression coverage for the continuous static P/T anthem building block on
//! the **controller-only** filter axis (no subtype/color narrowing), from a
//! creature source that buffs itself. Axes:
//!   - **controller-only** — every creature you control gets +1/+0,
//!   - **self-inclusion** — the source is a creature you control and is buffed,
//!   - **"you control"** — opponents' creatures are excluded (CR 109.4),
//!   - **lifetime** — the buff ends when the source leaves (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const MARAXUS: &str = "Creatures you control get +1/+0.";

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
fn maraxus_pumps_all_creatures_you_control_including_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a 4/1 creature carrying the anthem (real parse + synthesis
    // pipeline). It is a creature you control, so it buffs itself.
    let maraxus = scenario
        .add_creature_from_oracle(P0, "Maraxus", 4, 1, MARAXUS)
        .with_subtypes(vec!["Human", "Warrior"])
        .id();

    // Another creature you control — gets +1/+0.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    // CR 613.4c: every creature you control (including the source) gets +1/+0.
    assert_eq!(
        effective_pt(&mut runner, maraxus),
        (5, 1),
        "Maraxus buffs itself: base 4/1 + 1/0 = 5/1"
    );
    assert_eq!(
        effective_pt(&mut runner, ally),
        (3, 2),
        "another creature you control gets +1/+0: 2/2 → 3/2"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (2, 2),
        "an opponent's creature must NOT be buffed ('you control')"
    );
}

#[test]
fn maraxus_buff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let maraxus = scenario
        .add_creature_from_oracle(P0, "Maraxus", 4, 1, MARAXUS)
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, ally),
        (3, 2),
        "baseline: ally buffed to 3/2 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != maraxus);
        state.objects.remove(&maraxus);
    }
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "ally reverts to base 2/2 once the source is gone"
    );
}
