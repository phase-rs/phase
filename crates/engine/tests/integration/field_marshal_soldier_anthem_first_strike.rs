//! Field Marshal — "Other Soldier creatures you control get +1/+1 and have
//! first strike."
//!
//! Regression coverage for a continuous static that jointly exercises filter
//! axes existing anthem/grant tests don't cover together:
//!   - **other** — the source itself is excluded (no self-buff / self-grant),
//!   - **controller** — "you control" scopes to the source's controller (CR 109.5),
//!   - **subtype** — only Soldiers qualify (CR 205.3m),
//!   - **dual effect** — one static both pumps P/T (Layer 7c, CR 613.4c) AND
//!     grants a keyword (Layer 6 ability-adding, CR 613.1f).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` P/T and keyword set — a runtime test, not
//! an AST-shape test.

use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FIELD_MARSHAL: &str = "Other Soldier creatures you control get +1/+1 and have first strike.";

/// Recompute the layer system once (CR 613); callers then read effective state.
fn recalc(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

/// Effective power/toughness (read after a `recalc`).
fn pt(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = &runner.state().objects[&id];
    (
        obj.power.expect("creature has power"),
        obj.toughness.expect("creature has toughness"),
    )
}

/// Whether `id` has first strike (read after a `recalc`).
fn has_first_strike(runner: &GameRunner, id: ObjectId) -> bool {
    has_keyword(&runner.state().objects[&id], &Keyword::FirstStrike)
}

#[test]
fn field_marshal_buffs_other_soldiers_you_control_with_first_strike() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let marshal = scenario
        .add_creature_from_oracle(P0, "Field Marshal", 2, 2, FIELD_MARSHAL)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let ally_soldier = scenario
        .add_creature(P0, "Icatian Soldier", 1, 1)
        .with_subtypes(vec!["Soldier"])
        .id();
    let ally_bear = scenario
        .add_creature(P0, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let foe_soldier = scenario
        .add_creature(P1, "Veteran Soldier", 2, 2)
        .with_subtypes(vec!["Soldier"])
        .id();

    let mut runner = scenario.build();
    recalc(&mut runner);

    // Another Soldier you control: +1/+1 and first strike.
    assert_eq!(
        pt(&runner, ally_soldier),
        (2, 2),
        "ally Soldier 1/1 must become 2/2"
    );
    assert!(
        has_first_strike(&runner, ally_soldier),
        "ally Soldier must gain first strike"
    );

    // The source is itself a Soldier, but "other" excludes it.
    assert_eq!(
        pt(&runner, marshal),
        (2, 2),
        "Field Marshal must not buff itself (other)"
    );
    assert!(
        !has_first_strike(&runner, marshal),
        "Field Marshal must not grant itself first strike (other)"
    );

    // A non-Soldier you control is outside the subtype filter.
    assert_eq!(
        pt(&runner, ally_bear),
        (2, 2),
        "non-Soldier must be unaffected"
    );
    assert!(
        !has_first_strike(&runner, ally_bear),
        "non-Soldier must not gain first strike"
    );

    // An opponent's Soldier is outside the "you control" filter.
    assert_eq!(
        pt(&runner, foe_soldier),
        (2, 2),
        "opponent's Soldier must be unaffected (you control)"
    );
    assert!(
        !has_first_strike(&runner, foe_soldier),
        "opponent's Soldier must not gain first strike"
    );
}

#[test]
fn field_marshal_buff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let marshal = scenario
        .add_creature_from_oracle(P0, "Field Marshal", 2, 2, FIELD_MARSHAL)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let ally_soldier = scenario
        .add_creature(P0, "Icatian Soldier", 1, 1)
        .with_subtypes(vec!["Soldier"])
        .id();

    let mut runner = scenario.build();
    recalc(&mut runner);
    assert_eq!(
        pt(&runner, ally_soldier),
        (2, 2),
        "baseline: ally 1/1 -> 2/2 while the source is present"
    );
    assert!(
        has_first_strike(&runner, ally_soldier),
        "baseline: first strike while the source is present"
    );

    // CR 611.3b: the continuous effect ends when its source leaves the
    // battlefield. Move it to the graveyard (rules-correct — the card persists,
    // it just stops being on the battlefield), not delete it from existence.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), marshal, Zone::Graveyard, &mut events);
    recalc(&mut runner);

    assert_eq!(
        pt(&runner, ally_soldier),
        (1, 1),
        "ally reverts to base 1/1 once the source is gone"
    );
    assert!(
        !has_first_strike(&runner, ally_soldier),
        "first strike ends when the source leaves"
    );
}
