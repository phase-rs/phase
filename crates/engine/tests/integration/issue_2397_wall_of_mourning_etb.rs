//! Regression for issue #2397: Wall of Mourning ETB must exile from library top,
//! not from the battlefield.
//!
//! https://github.com/phase-rs/phase/issues/2397

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const WALL_ORACLE: &str = "Defender\nWhen this creature enters, exile a card from the top of your library face down for each opponent you have.";

#[test]
fn issue_2397_wall_of_mourning_etb_exiles_from_library_not_battlefield() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::White, ObjectId(0), false, vec![]),
        ],
    );
    for i in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Library Card {i}"));
    }
    let opponent_creature = scenario.add_creature(P1, "Opponent Bear", 2, 2).id();
    let wall = scenario
        .add_creature_to_hand_from_oracle(P0, "Wall of Mourning", 0, 4, WALL_ORACLE)
        .id();

    let mut runner = scenario.build();
    let library_before = runner.state().players[P0.0 as usize].library.len();

    runner.cast(wall).resolve();

    let library_after = runner.state().players[P0.0 as usize].library.len();
    assert_eq!(
        library_after,
        library_before - 1,
        "two-player game: Wall must exile one card from controller's library top"
    );
    assert_eq!(
        runner.state().objects[&opponent_creature].zone,
        Zone::Battlefield,
        "Wall's ETB must not exile opponent's battlefield creatures"
    );
    assert_eq!(
        runner.state().exile.len(),
        1,
        "exactly one card should be face-down exiled for the sole opponent"
    );
}
