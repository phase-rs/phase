//! Regression for issue #3878: Go-Shintai of Life's Origin must be able to
//! take combat damage as an enchantment creature.
//!
//! https://github.com/phase-rs/phase/issues/3878

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::{assert_damage_dealt, run_combat_and_collect_events};

#[test]
fn go_shintai_takes_combat_damage() {
    let db = crate::support::shared_card_db().expect("shared integration card database loads");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Attacker", 3, 3).id();
    let shintai = scenario.add_real_card(P1, "Go-Shintai of Life's Origin", Zone::Battlefield, db);

    let mut runner = scenario.build();
    assert_eq!(runner.state().objects[&shintai].power, Some(3));
    assert_eq!(runner.state().objects[&shintai].toughness, Some(4));
    let events =
        run_combat_and_collect_events(&mut runner, vec![attacker], vec![(shintai, attacker)]);

    assert_damage_dealt(&events, attacker, TargetRef::Object(shintai), 3, true);
    assert_damage_dealt(&events, shintai, TargetRef::Object(attacker), 3, true);
    assert_eq!(runner.state().objects[&shintai].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&shintai].damage_marked, 3);
    assert_eq!(runner.state().objects[&attacker].zone, Zone::Graveyard);
}
