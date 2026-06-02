//! Wire validation for Full-mode game session `Reconnect` frames.
//!
//! Draft reconnect uses `draft_wire_guard::guard_reconnect_draft`. The legacy
//! game-session reconnect path is handled directly in `phase-server` and
//! clones `game_code` / `player_token` into session lookup without bounds.

use lobby_broker::validation::{validate_token, MAX_GAME_CODE_LEN, MAX_TOKEN_LEN};

/// Validate `Reconnect` wire fields before session and reconnect-manager work.
pub fn guard_game_reconnect(game_code: &str, player_token: &str) -> Result<(), String> {
    validate_token("game_code", game_code, MAX_GAME_CODE_LEN)?;
    validate_token("player_token", player_token, MAX_TOKEN_LEN)?;
    Ok(())
}
