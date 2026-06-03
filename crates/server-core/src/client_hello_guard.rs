//! Wire validation for the WebSocket `ClientHello` handshake.
//!
//! Lobby broker frames validate `client_version` and `build_commit` via
//! `validate_lobby_message`, but native `phase-server` accepted and cloned
//! unbounded strings in `classify_hello_gate` before any size check.

use lobby_broker::validation::{validate_token, MAX_TOKEN_LEN};

/// Validate `ClientHello` identity fields before they are stored on the socket.
pub fn guard_client_hello(client_version: &str, build_commit: &str) -> Result<(), String> {
    validate_token("client_version", client_version, MAX_TOKEN_LEN)?;
    validate_token("build_commit", build_commit, MAX_TOKEN_LEN)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_broker::validation::MAX_TOKEN_LEN;

    #[test]
    fn client_hello_accepts_valid_fields() {
        assert!(guard_client_hello("1.0.0", "abc123").is_ok());
    }

    #[test]
    fn client_hello_rejects_oversized_version() {
        let err = guard_client_hello(&"v".repeat(MAX_TOKEN_LEN + 1), "abc").unwrap_err();
        assert!(err.contains("client_version"));
    }

    #[test]
    fn client_hello_rejects_oversized_build_commit() {
        let err = guard_client_hello("1.0.0", &"c".repeat(MAX_TOKEN_LEN + 1)).unwrap_err();
        assert!(err.contains("build_commit"));
    }
}
