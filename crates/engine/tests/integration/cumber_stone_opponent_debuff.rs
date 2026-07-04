//! Cumber Stone — "Creatures your opponents control get -1/-0." (an enchantment)
//!
//! Regression coverage for the continuous static P/T anthem building block on
//! the **opponent-controlled** filter axis with a NEGATIVE power modification —
//! the mirror of the "you control" anthems. Axes:
//!   - **opponent filter** — only creatures your opponents control are debuffed
//!     (CR 109.4),
//!   - **negative modification** — power is reduced (CR 613.4c),
//!   - **your creatures excluded** — the controller's own creatures are untouched,
//!   - **lifetime** — the debuff ends when the source leaves (CR 611.3).
//!
//! Drives the REAL parse → synthesis → layer pipeline and reads back the
//! EFFECTIVE post-`evaluate_layers` power/toughness — a runtime test, not an
//! AST-shape test.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const CUMBER_STONE: &str = "Creatures your opponents control get -1/-0.";

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
fn cumber_stone_debuffs_only_opponents_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Source: an enchantment controlled by P0 (real parse + synthesis pipeline,
    // then flipped to an enchantment permanent). "Your opponents" = P1.
    let _stone = scenario
        .add_creature_from_oracle(P0, "Cumber Stone", 0, 0, CUMBER_STONE)
        .as_enchantment()
        .id();

    // An opponent's creature — gets -1/-0.
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    // Your own creature — outside the "your opponents control" filter.
    let ally = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();

    // CR 613.4c: the opponent's creature gets -1/-0 → 1/2.
    assert_eq!(
        effective_pt(&mut runner, foe),
        (1, 2),
        "an opponent's creature must get -1/-0: 2/2 → 1/2"
    );

    // CR 109.4: the controller's own creature is excluded.
    assert_eq!(
        effective_pt(&mut runner, ally),
        (2, 2),
        "your own creature must NOT be debuffed ('your opponents control')"
    );
}

#[test]
fn cumber_stone_debuff_turns_off_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let stone = scenario
        .add_creature_from_oracle(P0, "Cumber Stone", 0, 0, CUMBER_STONE)
        .as_enchantment()
        .id();
    let foe = scenario.add_creature(P1, "Runeclaw Bear", 2, 2).id();

    let mut runner = scenario.build();
    assert_eq!(
        effective_pt(&mut runner, foe),
        (1, 2),
        "baseline: opponent creature debuffed to 1/2 while the source is present"
    );

    // CR 611.3: the continuous effect ends when its source leaves the battlefield.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != stone);
        state.objects.remove(&stone);
    }
    assert_eq!(
        effective_pt(&mut runner, foe),
        (2, 2),
        "opponent creature reverts to base 2/2 once the source is gone"
    );
}
