//! Regression for GitHub issue #3649 — Tetsuko Umezawa, Fugitive unblockable.

use engine::game::combat::can_block_pair;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

const TETSUKO_ORACLE: &str =
    "Creatures you control with power or toughness 1 or less can't be blocked.";

#[test]
fn tetsuko_makes_small_creatures_unblockable() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _tetsuko = scenario
        .add_creature(P0, "Tetsuko Umezawa, Fugitive", 1, 3)
        .from_oracle_text(TETSUKO_ORACLE)
        .id();
    let small = scenario.add_creature(P0, "1/1", 1, 1).id();
    let blocker = scenario.add_creature(P1, "Blocker", 3, 3).id();

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    let state = runner.state();
    assert!(
        !can_block_pair(state, blocker, small),
        "1/1 under Tetsuko must be unblockable"
    );
}
