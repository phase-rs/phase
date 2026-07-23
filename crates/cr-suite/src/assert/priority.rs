//! Priority assertions (CR 117).

use engine::game::scenario::GameRunner;
use engine::types::game_state::WaitingFor;
use engine::types::player::PlayerId;

use super::AssertionFailure;

/// Assert which player currently holds priority (CR 117.1).
///
/// Reads `GameState::priority_player` — the authoritative engine field the
/// priority system maintains (CR 117.3). When the engine is at a
/// `WaitingFor::Priority { player }` window, that window's player must match
/// the tracked `priority_player`; we surface the window player in diagnostics.
pub fn assert_priority_player(
    runner: &GameRunner,
    expected: PlayerId,
) -> Result<(), AssertionFailure> {
    let state = runner.state();
    let actual = state.priority_player;
    if actual != expected {
        let window = match &state.waiting_for {
            WaitingFor::Priority { player } => format!(" (priority window player={player:?})"),
            other => format!(" (waiting_for={other:?})"),
        };
        return Err(AssertionFailure {
            kind: "priority_player".into(),
            detail: format!("expected priority player {expected:?}, got {actual:?}{window}"),
        });
    }
    Ok(())
}
