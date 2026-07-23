//! Life-total assertions (CR 119).

use engine::game::scenario::GameRunner;
use engine::types::player::PlayerId;

use super::AssertionFailure;

pub fn assert_player_life(
    runner: &GameRunner,
    player: PlayerId,
    expected: i32,
) -> Result<(), AssertionFailure> {
    let actual = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.life)
        .ok_or_else(|| AssertionFailure {
            kind: "player_life".into(),
            detail: format!("player {player:?} not found"),
        })?;

    if actual != expected {
        return Err(AssertionFailure {
            kind: "player_life".into(),
            detail: format!("player {player:?}: expected life {expected}, got {actual}"),
        });
    }
    Ok(())
}
