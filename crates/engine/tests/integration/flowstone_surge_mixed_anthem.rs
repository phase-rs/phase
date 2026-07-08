//! Flowstone Surge — "Creatures you control get +1/-1." (an enchantment)
//!
//! Regression coverage for the continuous static P/T anthem building block with
//! a MIXED modification — power up, toughness down in the same clause. Axes:
//!   - **mixed +1/-1** — one anthem applies both a positive and a negative
//!     modifier (CR 613.4c),
//!   - **"you control"** — opponents' creatures are excluded (CR 109.4),
//!   - **lifetime** — the effect ends when the source leaves (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const FLOWSTONE_SURGE: &str = "Creatures you control get +1/-1.";

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
fn flowstone_surge_applies_mixed_pt_to_your_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: an enchantment carrying the mixed anthem (real parse + synthesis
    // pipeline, then flipped to an enchantment permanent).
    let _surge = scenario
        .add_creature_from_oracle(P0, "Flowstone Surge", 0, 0, FLOWSTONE_SURGE)
        .as_enchantment()
        .id();

    // A creature you control — gets +1/-1.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    // An opponent's creature — excluded by "you control".
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();

    // CR 613.4c: a creature you control gets +1/-1 → 3/1.
    assert_eq!(
        effective_pt(&mut runner, ally),
        (3, 1),
        "a creature you control gets +1/-1: 2/2 → 3/1"
    );

    // CR 109.4: "you control" excludes the opponent's creature.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (2, 2),
        "an opponent's creature must NOT be affected ('you control')"
    );
}

#[test]
fn flowstone_surge_effect_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let surge = scenario
        .add_creature_from_oracle(P0, "Flowstone Surge", 0, 0, FLOWSTONE_SURGE)
        .as_enchantment()
        .id();
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, ally),
        (3, 1),
        "baseline: ally is 3/1 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != surge);
        state.objects.remove(&surge);
    }
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "ally reverts to base 2/2 once the source is gone"
    );
}
