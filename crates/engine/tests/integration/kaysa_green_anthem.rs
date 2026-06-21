//! Kaysa — "Green creatures you control get +1/+1."
//!
//! Regression coverage for the continuous static P/T anthem building block on
//! the **color** filter axis (green) — distinct from the subtype/controller
//! axes. Axes: color filter (only green creatures), self-inclusion (the green
//! source buffs itself), the "you control" exclusion, and buff lifetime
//! (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test. Colors are set via `with_mana_cost` (derives color from the
//! cost, the same path production uses).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;

const KAYSA: &str = "Green creatures you control get +1/+1.";

fn green() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Green],
        generic: 0,
    }
}

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
fn kaysa_buffs_green_creatures_you_control_including_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: a green 3/3 creature carrying the anthem (real parse + synthesis
    // pipeline). It is green and you control it, so it buffs itself.
    let kaysa = scenario
        .add_creature_from_oracle(P0, "Kaysa", 3, 3, KAYSA)
        .with_mana_cost(green())
        .with_subtypes(vec!["Dryad"])
        .id();

    // Another GREEN creature you control — gets +1/+1.
    let green_ally = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_mana_cost(green())
        .with_subtypes(vec!["Elf", "Druid"])
        .id();

    // A NON-green creature you control — outside the color filter.
    let colorless_ally = scenario
        .add_creature(P0, "Ornithopter", 0, 2)
        .with_subtypes(vec!["Thopter"])
        .id();

    // An opponent's green creature — outside the "you control" filter.
    let green_foe = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_mana_cost(green())
        .with_subtypes(vec!["Bear"])
        .id();

    let mut runner = scenario.build();

    // CR 613.4c: green creatures you control (including the source) get +1/+1.
    assert_eq!(
        effective_pt(&mut runner, kaysa),
        (4, 4),
        "Kaysa is a green creature you control and buffs itself: 3/3 + 1/1 = 4/4"
    );
    assert_eq!(
        effective_pt(&mut runner, green_ally),
        (2, 2),
        "another green creature you control gets +1/+1: 1/1 → 2/2"
    );

    // CR 105.2: a non-green creature is outside the color filter.
    assert_eq!(
        effective_pt(&mut runner, colorless_ally),
        (0, 2),
        "a non-green creature you control must NOT be buffed"
    );

    // CR 109.4: "you control" excludes the opponent's green creature.
    assert_eq!(
        effective_pt(&mut runner, green_foe),
        (2, 2),
        "an opponent's green creature must NOT be buffed ('you control')"
    );
}

#[test]
fn kaysa_buff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let kaysa = scenario
        .add_creature_from_oracle(P0, "Kaysa", 3, 3, KAYSA)
        .with_mana_cost(green())
        .with_subtypes(vec!["Dryad"])
        .id();
    let green_ally = scenario
        .add_creature(P0, "Llanowar Elves", 1, 1)
        .with_mana_cost(green())
        .with_subtypes(vec!["Elf", "Druid"])
        .id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, green_ally),
        (2, 2),
        "baseline: green ally buffed to 2/2 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != kaysa);
        state.objects.remove(&kaysa);
    }
    assert_eq!(
        effective_pt(&mut runner, green_ally),
        (1, 1),
        "green ally reverts to base 1/1 once the source is gone"
    );
}
