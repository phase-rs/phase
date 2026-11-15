//! CR 700.13: a player commits a crime by targeting an opponent, specified
//! opponent-controlled objects, or an opponent-owned graveyard card.

use engine::game::ledger::{record_crime_committed, resolve_and_apply_ledger_edit};
use engine::game::turns::start_next_turn;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedLedgerEdit, ResolvedLedgerEditReplayInvariantError,
};

#[test]
fn crime_ledger_edit_is_turn_scoped() {
    let mut state = GameState::new_two_player(42);

    record_crime_committed(&mut state, PlayerId(0)).expect("live player records a crime");
    record_crime_committed(&mut state, PlayerId(0)).expect("repeat crime preserves turn fact");
    record_crime_committed(&mut state, PlayerId(1)).expect("other player records their crime");
    assert_eq!(state.players[0].crimes_committed_this_turn, 1);
    assert_eq!(state.players[1].crimes_committed_this_turn, 1);

    start_next_turn(&mut state, &mut Vec::new());
    assert_eq!(
        state.players[0].crimes_committed_this_turn, 0,
        "a new turn clears the first player's per-turn engine record"
    );
    assert_eq!(
        state.players[1].crimes_committed_this_turn, 0,
        "a new turn clears every player's per-turn engine record"
    );
}

#[test]
fn crime_ledger_replay_rejects_a_second_turn_record() {
    let mut state = GameState::new_two_player(42);
    let edit = ResolvedLedgerEdit::CrimeCommitted {
        player: PlayerId(0),
        expected_turn_count: 0,
    };

    resolve_and_apply_ledger_edit(&mut state, edit.clone())
        .expect("the first exact crime edit establishes the turn fact");

    assert_eq!(
        resolve_and_apply_ledger_edit(&mut state, edit),
        Err(ResolvedLedgerEditReplayInvariantError::CrimeCommittedPreconditionMismatch),
    );
    assert_eq!(state.players[0].crimes_committed_this_turn, 1);
}
