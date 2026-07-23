//! Library assertions (CR 401).

use engine::game::scenario::GameRunner;
use engine::types::player::PlayerId;

use super::AssertionFailure;

/// Assert a player's library size (CR 401.1: a player's deck becomes their library).
pub fn assert_library_count(
    runner: &GameRunner,
    player: PlayerId,
    expected: usize,
) -> Result<(), AssertionFailure> {
    let actual = library_len(runner, player)?;
    if actual != expected {
        return Err(AssertionFailure {
            kind: "library_count".into(),
            detail: format!("player {player:?}: expected library size {expected}, got {actual}"),
        });
    }
    Ok(())
}

/// Assert the top card of a player's library resolves to a named object handle.
///
/// CR 401.2: the owner of a library keeps its cards in a single face-down pile;
/// the "top" is the first card that would be drawn. The engine stores the
/// library top-first, so the first entry is the top card.
pub fn assert_library_top(
    runner: &GameRunner,
    handles: &super::HandleMap,
    player: PlayerId,
    creature: &str,
) -> Result<(), AssertionFailure> {
    let expected_id = handles.get(creature).ok_or_else(|| AssertionFailure {
        kind: "library_top".into(),
        detail: format!("unknown handle {creature:?}"),
    })?;
    let top = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .and_then(|p| p.library.front().copied())
        .ok_or_else(|| AssertionFailure {
            kind: "library_top".into(),
            detail: format!("player {player:?} has an empty library"),
        })?;
    if top != *expected_id {
        return Err(AssertionFailure {
            kind: "library_top".into(),
            detail: format!("player {player:?}: expected top {expected_id:?}, got {top:?}"),
        });
    }
    Ok(())
}

fn library_len(runner: &GameRunner, player: PlayerId) -> Result<usize, AssertionFailure> {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.library.len())
        .ok_or_else(|| AssertionFailure {
            kind: "library_count".into(),
            detail: format!("player {player:?} not found"),
        })
}
