//! Regression for issue #3305: Blood Scrivener must draw two cards and lose 1
//! life when you would draw with an empty hand.
//!
//! https://github.com/phase-rs/phase/issues/3305

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

#[test]
fn blood_scrivener_draws_two_when_hand_empty() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Draw);
    scenario.add_real_card(P0, "Blood Scrivener", Zone::Battlefield, db);
    for name in ["Top A", "Top B"] {
        scenario.add_card_to_library_top(P0, name);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let life_before = runner.state().players[P0.0 as usize].life;

    let mut events = Vec::new();
    engine::game::turns::execute_draw(runner.state_mut(), &mut events);

    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        2,
        "Blood Scrivener must replace the draw-step draw with two cards when hand is empty"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before - 1,
        "Blood Scrivener's replacement must also make you lose 1 life"
    );
}
