//! Wire validation for native `LookupJoinTarget` handling in `phase-server`.
//!
//! The broker validates this frame via `parse_lobby_client_message`, but the
//! native shell handles `LookupJoinTarget` directly for both Full and
//! LobbyOnly modes without running `validate_lobby_message` first.

use lobby_broker::protocol::LobbyClientMessage;
use lobby_broker::validation::{validate_lookup_join_target_fields, LookupJoinTargetFields};

/// Borrowed lookup wire fields for the native projection path.
pub struct LookupJoinTargetInbound<'a> {
    pub game_code: &'a str,
    pub password: Option<&'a str>,
    pub display_name: Option<&'a str>,
    pub release_reservation_token: Option<&'a str>,
}

/// Validate lookup wire fields without cloning into `LobbyClientMessage`.
pub fn guard_lookup_join_target_inbound(fields: LookupJoinTargetInbound<'_>) -> Result<(), String> {
    validate_lookup_join_target_fields(LookupJoinTargetFields {
        game_code: fields.game_code,
        password: fields.password,
        display_name: fields.display_name,
        release_reservation_token: fields.release_reservation_token,
    })
}

/// Validate `LookupJoinTarget` wire fields before lobby lookup and reservation
/// work.
pub fn guard_lookup_join_target(msg: &LobbyClientMessage) -> Result<(), String> {
    match msg {
        LobbyClientMessage::LookupJoinTarget {
            game_code,
            password,
            display_name,
            release_reservation_token,
            ..
        } => guard_lookup_join_target_inbound(LookupJoinTargetInbound {
            game_code,
            password: password.as_deref(),
            display_name: display_name.as_deref(),
            release_reservation_token: release_reservation_token.as_deref(),
        }),
        _ => Err("unexpected lobby message for LookupJoinTarget guard".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_fields(game_code: &str) -> LookupJoinTargetInbound<'_> {
        LookupJoinTargetInbound {
            game_code,
            password: None,
            display_name: None,
            release_reservation_token: None,
        }
    }

    #[test]
    fn lookup_accepts_valid_game_code() {
        assert!(guard_lookup_join_target_inbound(lookup_fields("ABC123")).is_ok());
    }

    #[test]
    fn lookup_rejects_oversized_game_code() {
        let err = guard_lookup_join_target_inbound(lookup_fields(&"x".repeat(65))).unwrap_err();
        assert!(err.contains("game_code"));
    }

    #[test]
    fn owned_lookup_msg_still_validates() {
        let msg = LobbyClientMessage::LookupJoinTarget {
            game_code: "ABC123".to_string(),
            password: None,
            reserve: false,
            display_name: None,
            release_reservation_token: None,
        };
        assert!(guard_lookup_join_target(&msg).is_ok());
    }
}
