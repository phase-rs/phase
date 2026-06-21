//! Issue #3319 — War's Toll taps all lands when an opponent taps a land for mana.
//!
//! https://github.com/phase-rs/phase/issues/3319

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const WARS_TOLL_ORACLE: &str = "Whenever an opponent taps a land for mana, tap all lands that player controls.\n\
    If a creature an opponent controls attacks, all creatures that opponent controls attack if able.";

#[test]
fn wars_toll_taps_all_lands_when_opponent_taps_land_for_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _wars_toll = scenario
        .add_creature_from_oracle(P0, "War's Toll", 0, 0, WARS_TOLL_ORACLE)
        .as_enchantment()
        .with_mana_cost(ManaCost::generic(4))
        .id();

    let forest1 = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);
    let forest2 = scenario.add_basic_land(P1, engine::types::mana::ManaColor::Green);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = engine::types::game_state::WaitingFor::Priority { player: P1 };
    }

    runner.activate(forest1, 0).resolve();

    assert!(
        runner.state().objects[&forest1].tapped,
        "tapped forest stays tapped"
    );
    assert!(
        runner.state().objects[&forest2].tapped,
        "War's Toll must tap all other lands the opponent controls"
    );
}
