//! Regression for issue #3878: Go-Shintai of Life's Origin must be able to
//! take combat damage as an enchantment creature.
//!
//! https://github.com/phase-rs/phase/issues/3878

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

use super::rules::run_combat;

const GO_SHINTAI_ORACLE: &str = "{W}{U}{B}{R}{G}, {T}: Return target enchantment card from your graveyard to the battlefield.\n\
    Whenever Go-Shintai of Life's Origin or another nontoken Shrine you control enters, create a 1/1 colorless Shrine enchantment creature token.";

#[test]
fn go_shintai_takes_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let shintai = scenario
        .add_creature_from_oracle(P1, "Go-Shintai of Life's Origin", 1, 1, GO_SHINTAI_ORACLE)
        .id();

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![(shintai, attacker)]);

    assert!(
        runner.state().objects[&shintai].damage_marked >= 3,
        "Go-Shintai must take combat damage from the blocked attacker, got damage_marked={}",
        runner.state().objects[&shintai].damage_marked
    );
}
