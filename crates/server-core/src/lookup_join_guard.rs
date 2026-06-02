//! Wire validation for native `LookupJoinTarget` handling in `phase-server`.
//!
//! The broker validates this frame via `parse_lobby_client_message`, but the
//! native shell handles `LookupJoinTarget` directly for both Full and
//! LobbyOnly modes without running `validate_lobby_message` first.

use lobby_broker::protocol::LobbyClientMessage;
use lobby_broker::validation::validate_lobby_message;

/// Validate `LookupJoinTarget` wire fields before lobby lookup and reservation
/// work.
pub fn guard_lookup_join_target(msg: &LobbyClientMessage) -> Result<(), String> {
    match msg {
        LobbyClientMessage::LookupJoinTarget { .. } => validate_lobby_message(msg),
        _ => Err("unexpected lobby message for LookupJoinTarget guard".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_msg(game_code: &str) -> LobbyClientMessage {
        LobbyClientMessage::LookupJoinTarget {
            game_code: game_code.to_string(),
            password: None,
            reserve: false,
            display_name: None,
            release_reservation_token: None,
        }
    }

    #[test]
    fn lookup_accepts_valid_game_code() {
        assert!(guard_lookup_join_target(&lookup_msg("ABC123")).is_ok());
    }

    #[test]
    fn lookup_rejects_oversized_game_code() {
        let err = guard_lookup_join_target(&lookup_msg(&"x".repeat(65))).unwrap_err();
        assert!(err.contains("game_code"));
    }
}
