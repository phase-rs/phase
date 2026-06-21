//! Issue #3250 — Paladin Class level 1 tax applies only during controller's turn.
//!
//! https://github.com/phase-rs/phase/issues/3250

use engine::game::casting::display_spell_cost;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle_static::parse_static_line;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

#[test]
fn paladin_class_tax_only_during_controllers_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Paladin Class", 0, 0)
        .with_static_definition(
            parse_static_line("Spells your opponents cast during your turn cost {1} more to cast.")
                .expect("Paladin Class level-1 static should parse"),
        );

    let bear = scenario
        .add_spell_to_hand(P1, "Grizzly Bears", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    let cost_on_opponent_turn =
        display_spell_cost(runner.state(), P1, bear).expect("Grizzly Bears cost");

    runner.state_mut().active_player = P0;
    let cost_on_controller_turn =
        display_spell_cost(runner.state(), P1, bear).expect("Grizzly Bears cost");

    assert_eq!(
        cost_on_opponent_turn,
        ManaCost::generic(2),
        "Paladin Class must not tax opponents on the controller's off-turn"
    );
    assert_eq!(
        cost_on_controller_turn,
        ManaCost::generic(3),
        "Paladin Class must tax opponents during the controller's turn"
    );
}

#[test]
fn paladin_class_tax_does_not_apply_to_controller_spells() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Paladin Class", 0, 0)
        .with_static_definition(
            parse_static_line("Spells your opponents cast during your turn cost {1} more to cast.")
                .expect("Paladin Class level-1 static should parse"),
        );

    let bear = scenario
        .add_spell_to_hand(P0, "Grizzly Bears", false)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    let cost = display_spell_cost(runner.state(), P0, bear).expect("Grizzly Bears cost");

    assert_eq!(
        cost,
        ManaCost::generic(2),
        "Paladin Class must not tax the controller's own spells"
    );
}
