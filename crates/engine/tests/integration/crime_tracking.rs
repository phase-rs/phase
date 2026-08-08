//! CR 700.13: committed crimes are durable only after a successful action and
//! reset at the next turn boundary.

use engine::game::ledger::record_crime_committed;
use engine::game::turns::start_next_turn;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

#[test]
fn crime_ledger_edit_is_turn_scoped() {
    let mut state = GameState::new_two_player(42);

    record_crime_committed(&mut state, PlayerId(0)).expect("live player records a crime");
    record_crime_committed(&mut state, PlayerId(0)).expect("repeat crime preserves turn fact");
    assert_eq!(state.players[0].crimes_committed_this_turn, 1);

    start_next_turn(&mut state, &mut Vec::new());
    assert_eq!(
        state.players[0].crimes_committed_this_turn, 0,
        "a new turn clears the CR 700.13 per-turn record"
    );
}
