//! Steelform Sliver — "Sliver creatures you control get +0/+1."
//!
//! Regression coverage for the continuous static P/T anthem building block on
//! the subtype filter axis (Slivers) — the source is itself a Sliver you
//! control, so it buffs itself. Axes: subtype filter, self-inclusion, the
//! "you control" exclusion, and buff lifetime (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const STEELFORM_SLIVER: &str = "Sliver creatures you control get +0/+1.";

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
fn steelform_sliver_buffs_slivers_you_control_including_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a 1/1 Sliver carrying the anthem (real parse + synthesis pipeline).
    // It is itself a Sliver you control, so it buffs itself.
    let steelform = scenario
        .add_creature_from_oracle(P0, "Steelform Sliver", 1, 1, STEELFORM_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();

    // Another Sliver you control — gets +0/+1.
    let ally_sliver = scenario
        .add_creature(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    // A non-Sliver you control — outside the subtype filter.
    let ally_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();

    // An opponent's Sliver — outside the "you control" filter.
    let foe_sliver = scenario
        .add_creature(P1, "Plated Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    let mut runner = scenario.build();

    // CR 613.4c: Slivers you control (including the source) get +0/+1.
    assert_eq!(
        effective_pt(&mut runner, steelform),
        (1, 2),
        "Steelform Sliver is a Sliver you control and buffs itself: 1/1 + 0/1 = 1/2"
    );
    assert_eq!(
        effective_pt(&mut runner, ally_sliver),
        (1, 2),
        "another Sliver you control gets +0/+1: 1/1 → 1/2"
    );

    // CR 205.3m: a non-Sliver you control is outside the subtype filter.
    assert_eq!(
        effective_pt(&mut runner, ally_bear),
        (2, 2),
        "a non-Sliver you control must NOT be buffed"
    );

    // CR 109.4: "you control" excludes the opponent's Sliver.
    assert_eq!(
        effective_pt(&mut runner, foe_sliver),
        (1, 1),
        "an opponent's Sliver must NOT be buffed ('you control')"
    );
}

#[test]
fn steelform_sliver_buff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let steelform = scenario
        .add_creature_from_oracle(P0, "Steelform Sliver", 1, 1, STEELFORM_SLIVER)
        .with_subtypes(vec!["Sliver"])
        .id();
    let ally_sliver = scenario
        .add_creature(P0, "Muscle Sliver", 1, 1)
        .with_subtypes(vec!["Sliver"])
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, ally_sliver),
        (1, 2),
        "baseline: ally Sliver buffed to 1/2 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != steelform);
        state.objects.remove(&steelform);
    }
    assert_eq!(
        effective_pt(&mut runner, ally_sliver),
        (1, 1),
        "ally Sliver reverts to base 1/1 once the source is gone"
    );
}
