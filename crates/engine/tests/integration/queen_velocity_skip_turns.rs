//! Regression coverage for Eater of Days' plural controller turn skip.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::turns::start_next_turn;
use engine::types::events::GameEvent;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const EATER_OF_DAYS: &str =
    "Flying, trample\nWhen this creature enters, you skip your next two turns.";

/// CR 614.10 + CR 614.10a: Eater of Days' ETB creates two independently
/// satisfied next-turn skip replacements for its controller.
#[test]
fn eater_of_days_etb_skips_controllers_next_two_turns() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let eater = scenario
        .add_creature_to_hand_from_oracle(P0, "Eater of Days", 9, 8, EATER_OF_DAYS)
        .with_mana_cost(ManaCost::zero())
        .id();
    let mut runner = scenario.build();

    runner.cast(eater).resolve();
    assert_eq!(
        runner.state().turns_to_skip[P0.0 as usize],
        2,
        "ETB must arm both of Eater of Days' skipped turns"
    );

    let mut events = Vec::<GameEvent>::new();
    start_next_turn(runner.state_mut(), &mut events);
    assert_eq!(
        runner.state().active_player,
        P1,
        "P1 takes the intervening turn"
    );
    assert_eq!(runner.state().turns_to_skip[P0.0 as usize], 2);
    runner.state_mut().phase = Phase::Cleanup;

    start_next_turn(runner.state_mut(), &mut events);
    assert_eq!(
        runner.state().active_player,
        P1,
        "P0's first scheduled turn is skipped"
    );
    assert_eq!(runner.state().turns_to_skip[P0.0 as usize], 1);
    runner.state_mut().phase = Phase::Cleanup;

    start_next_turn(runner.state_mut(), &mut events);
    assert_eq!(
        runner.state().active_player,
        P1,
        "P0's second scheduled turn is skipped"
    );
    assert_eq!(runner.state().turns_to_skip[P0.0 as usize], 0);
}
