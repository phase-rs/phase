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

#[cfg(test)]
mod tests {
    use super::{guard_game_reconnect, MAX_GAME_CODE_LEN, MAX_TOKEN_LEN};

    #[test]
    fn game_reconnect_accepts_valid_fields() {
        assert!(guard_game_reconnect("ABC123", &"t".repeat(32)).is_ok());
    }

    #[test]
    fn game_reconnect_rejects_oversized_game_code() {
        let err = guard_game_reconnect(&"x".repeat(MAX_GAME_CODE_LEN + 1), "token").unwrap_err();
        assert!(err.contains("game_code"));
    }

    #[test]
    fn game_reconnect_rejects_oversized_player_token() {
        let err = guard_game_reconnect("ABC123", &"t".repeat(MAX_TOKEN_LEN + 1)).unwrap_err();
        assert!(err.contains("player_token"));
    }
}
