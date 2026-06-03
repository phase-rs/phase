//! Wire validation for the `POST /p2p-draft-backup` HTTP endpoint in `phase-server`.
//!
//! The P2P draft-backup endpoint persists a host-supplied peer id and a
//! serialized draft-state snapshot to SQLite (`save_p2p_backup`, an upsert keyed
//! on `draft_code`) and echoes both fields back to any caller of
//! `GET /p2p-draft-backup/{code}`. Unlike the WebSocket lobby path — which
//! bounds `host_peer_id` to [`MAX_TOKEN_LEN`] via
//! [`lobby_broker::validation::validate_token`] before the broker stores or
//! broadcasts it — the HTTP body was stored verbatim, so the same field was
//! bounded on one transport and unbounded on the other.
//!
//! This guard applies the shared size/shape contract at the HTTP boundary,
//! before the database write, so both transports agree. `draft_code` is
//! validated separately by the endpoint (the check is shared with the GET/DELETE
//! routes); this guard bounds the two free-form fields the row stores verbatim.

use lobby_broker::validation::{validate_token, MAX_TOKEN_LEN};

/// Max byte length of the serialized draft snapshot accepted on the wire. A full
/// draft session (up to 8 seats × 3 packs plus pools and pairings) serializes to
/// well under this ceiling; the cap rejects abusive blobs before they are
/// persisted and echoed back, while staying clear of the host-authoritative
/// snapshots a real client produces.
pub const MAX_P2P_SNAPSHOT_LEN: usize = 1024 * 1024;

/// Validate the free-form body fields of a `POST /p2p-draft-backup` request
/// before persistence. `host_peer_id` reuses the same [`validate_token`] bound
/// (`MAX_TOKEN_LEN`, plus control-character rejection) the WebSocket lobby path
/// applies to the host peer id; `snapshot_json` is an opaque serialized blob, so
/// it is required and bounded by byte length only.
pub fn guard_p2p_backup(host_peer_id: &str, snapshot_json: &str) -> Result<(), String> {
    if host_peer_id.trim().is_empty() {
        return Err("host_peer_id must not be empty".to_string());
    }
    validate_token("host_peer_id", host_peer_id, MAX_TOKEN_LEN)?;
    if snapshot_json.trim().is_empty() {
        return Err("snapshot_json must not be empty".to_string());
    }
    if snapshot_json.len() > MAX_P2P_SNAPSHOT_LEN {
        return Err(format!(
            "snapshot_json must be at most {MAX_P2P_SNAPSHOT_LEN} bytes"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{guard_p2p_backup, MAX_P2P_SNAPSHOT_LEN};
    use lobby_broker::validation::MAX_TOKEN_LEN;

    #[test]
    fn accepts_valid_backup() {
        assert!(guard_p2p_backup("peer-host-abc", r#"{"status":"Drafting"}"#).is_ok());
    }

    #[test]
    fn accepts_host_peer_id_at_limit() {
        let at_limit = "p".repeat(MAX_TOKEN_LEN);
        assert!(guard_p2p_backup(&at_limit, "{}").is_ok());
    }

    #[test]
    fn rejects_blank_host_peer_id() {
        let err = guard_p2p_backup("  ", "{}").unwrap_err();
        assert!(err.contains("host_peer_id"));
    }

    #[test]
    fn rejects_oversized_host_peer_id() {
        let oversized = "p".repeat(MAX_TOKEN_LEN + 1);
        let err = guard_p2p_backup(&oversized, "{}").unwrap_err();
        assert!(err.contains("host_peer_id"));
    }

    #[test]
    fn rejects_host_peer_id_with_control_char() {
        let err = guard_p2p_backup("peer\u{0007}id", "{}").unwrap_err();
        assert!(err.contains("host_peer_id"));
    }

    #[test]
    fn accepts_snapshot_at_limit() {
        let at_limit = "x".repeat(MAX_P2P_SNAPSHOT_LEN);
        assert!(guard_p2p_backup("peer", &at_limit).is_ok());
    }

    #[test]
    fn rejects_blank_snapshot() {
        let err = guard_p2p_backup("peer", "\n\t ").unwrap_err();
        assert!(err.contains("snapshot_json"));
    }

    #[test]
    fn rejects_oversized_snapshot() {
        let oversized = "x".repeat(MAX_P2P_SNAPSHOT_LEN + 1);
        let err = guard_p2p_backup("peer", &oversized).unwrap_err();
        assert!(err.contains("snapshot_json"));
    }
}
