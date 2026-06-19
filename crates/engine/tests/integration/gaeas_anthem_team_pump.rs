//! Gaea's Anthem — "Creatures you control get +1/+1." (an enchantment)
//!
//! Regression coverage for the continuous static P/T anthem building block from
//! a NON-creature (enchantment) source on the controller-only filter axis. Axes:
//!   - **non-creature source** — the +1/+1 comes from an enchantment,
//!   - **controller-only** — every creature you control gets +1/+1,
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

const GAEAS_ANTHEM: &str = "Creatures you control get +1/+1.";

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
fn gaeas_anthem_pumps_creatures_you_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: an enchantment carrying the anthem (real parse + synthesis
    // pipeline, then flipped to an enchantment permanent).
    let _anthem = scenario
        .add_creature_from_oracle(P0, "Gaea's Anthem", 0, 0, GAEAS_ANTHEM)
        .as_enchantment()
        .id();

    // Two creatures you control — both get +1/+1.
    let ally1 = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let ally2 = scenario.add_creature(P0, "Llanowar Elves", 1, 1).id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    // CR 613.4c: creatures you control get +1/+1.
    assert_eq!(
        effective_pt(&mut runner, ally1),
        (3, 3),
        "a creature you control gets +1/+1: 2/2 → 3/3"
    );
    assert_eq!(
        effective_pt(&mut runner, ally2),
        (2, 2),
        "another creature you control gets +1/+1: 1/1 → 2/2"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (2, 2),
        "an opponent's creature must NOT be buffed ('you control')"
    );
}

#[test]
fn gaeas_anthem_buff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let anthem = scenario
        .add_creature_from_oracle(P0, "Gaea's Anthem", 0, 0, GAEAS_ANTHEM)
        .as_enchantment()
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, ally),
        (3, 3),
        "baseline: ally buffed to 3/3 while the enchantment is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != anthem);
        state.objects.remove(&anthem);
    }
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "ally reverts to base 2/2 once the enchantment is gone"
    );
}
