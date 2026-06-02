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
