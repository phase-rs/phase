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
//!
//! P2P host snapshots also carry per-seat session credentials (`seatTokens`,
//! `kickedTokens`) that authorize WebRTC draft seats. Those secrets must never
//! be persisted or echoed on the unauthenticated HTTP backup surface — the host
//! keeps credentials in local IndexedDB; the server backup is for draft progress
//! recovery only.
//!
//! The host also embeds a full `draftSessionJson` blob (exported `DraftSession`).
//! That nested payload can carry `config.rng_seed` and unopened `packs_by_seat`,
//! which must be stripped before SQLite persistence or `GET` echo — merged #5053
//! only removed top-level seat tokens.

use lobby_broker::validation::{validate_token, MAX_TOKEN_LEN};
use serde_json::{Map, Value};

/// Max byte length of the serialized draft snapshot accepted on the wire. A full
/// draft session (up to 8 seats × 3 packs plus pools and pairings) serializes to
/// well under this ceiling; the cap rejects abusive blobs before they are
/// persisted and echoed back, while staying clear of the host-authoritative
/// snapshots a real client produces.
pub const MAX_P2P_SNAPSHOT_LEN: usize = 1024 * 1024;

/// Validate a `host_peer_id` on any P2P draft-backup HTTP surface (POST store,
/// DELETE cleanup). Reuses the same [`validate_token`] bound the WebSocket lobby
/// path applies to the host peer id.
pub fn validate_p2p_backup_host_peer_id(host_peer_id: &str) -> Result<(), String> {
    if host_peer_id.trim().is_empty() {
        return Err("host_peer_id must not be empty".to_string());
    }
    validate_token("host_peer_id", host_peer_id, MAX_TOKEN_LEN)
}

/// Validate the free-form body fields of a `POST /p2p-draft-backup` request
/// before persistence. `host_peer_id` reuses the same [`validate_token`] bound
/// (`MAX_TOKEN_LEN`, plus control-character rejection) the WebSocket lobby path
/// applies to the host peer id; `snapshot_json` is an opaque serialized blob, so
/// it is required and bounded by byte length only.
pub fn guard_p2p_backup(host_peer_id: &str, snapshot_json: &str) -> Result<(), String> {
    validate_p2p_backup_host_peer_id(host_peer_id)?;
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

/// Keys stripped from a P2P host backup snapshot before SQLite persistence or
/// HTTP response. These are session credentials, not recoverable draft state.
const P2P_BACKUP_SECRET_KEYS: &[&str] = &["seatTokens", "kickedTokens"];

/// Host snapshot field carrying a serialized [`draft_core::types::DraftSession`].
const DRAFT_SESSION_JSON_KEY: &str = "draftSessionJson";

/// Nested draft session fields that must not be stored or echoed on the backup API.
const NESTED_DRAFT_SECRET_KEYS: &[&str] = &["packs_by_seat"];

/// Remove session credentials from a host backup snapshot JSON blob.
///
/// The backup row is keyed only by the 6-character draft code and is readable by
/// any caller of `GET /p2p-draft-backup/{code}`, so stored snapshots must not
/// contain per-seat tokens or competitive secrets embedded in `draftSessionJson`.
pub fn redact_p2p_backup_snapshot_secrets(snapshot_json: &str) -> Result<String, String> {
    let mut value: Value = serde_json::from_str(snapshot_json)
        .map_err(|_| "snapshot_json must be a JSON object".to_string())?;
    let Some(obj) = value.as_object_mut() else {
        return Err("snapshot_json must be a JSON object".to_string());
    };
    redact_secret_keys(obj);
    serde_json::to_string(&value).map_err(|e| format!("snapshot_json serialization failed: {e}"))
}

fn redact_secret_keys(obj: &mut Map<String, Value>) {
    for key in P2P_BACKUP_SECRET_KEYS {
        obj.remove(*key);
    }
    redact_nested_draft_session_json(obj);
}

fn redact_nested_draft_session_json(obj: &mut Map<String, Value>) {
    let Some(nested_raw) = obj.get(DRAFT_SESSION_JSON_KEY).and_then(|v| v.as_str()) else {
        return;
    };
    let Ok(mut nested) = serde_json::from_str::<Value>(nested_raw) else {
        return;
    };
    let Some(nested_obj) = nested.as_object_mut() else {
        return;
    };
    redact_draft_session_object(nested_obj);
    if let Ok(serialized) = serde_json::to_string(&nested) {
        obj.insert(
            DRAFT_SESSION_JSON_KEY.to_string(),
            Value::String(serialized),
        );
    }
}

fn redact_draft_session_object(session: &mut Map<String, Value>) {
    for key in NESTED_DRAFT_SECRET_KEYS {
        session.remove(*key);
    }
    if let Some(Value::Object(config)) = session.get_mut("config") {
        config.insert("rng_seed".to_string(), Value::Number(0.into()));
    }
}

/// Reject overwrites from a different host peer than the row's owner.
pub fn guard_p2p_backup_overwrite(
    existing_host_peer_id: &str,
    incoming_host_peer_id: &str,
) -> Result<(), String> {
    if existing_host_peer_id != incoming_host_peer_id {
        Err("host_peer_id does not match the existing backup owner".to_string())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        guard_p2p_backup, guard_p2p_backup_overwrite, redact_p2p_backup_snapshot_secrets,
        validate_p2p_backup_host_peer_id, MAX_P2P_SNAPSHOT_LEN,
    };
    use lobby_broker::validation::MAX_TOKEN_LEN;
    use serde_json::Value;

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

    #[test]
    fn redact_p2p_backup_snapshot_secrets_strips_seat_and_kicked_tokens() {
        let raw = r#"{
            "draftCode": "ABC123",
            "seatTokens": {"0": "host-secret", "1": "guest-secret"},
            "kickedTokens": ["evicted-secret"],
            "draftStarted": true
        }"#;
        let redacted = redact_p2p_backup_snapshot_secrets(raw).expect("valid snapshot");
        let parsed: Value = serde_json::from_str(&redacted).unwrap();
        assert!(parsed.get("seatTokens").is_none());
        assert!(parsed.get("kickedTokens").is_none());
        assert_eq!(parsed["draftCode"], "ABC123");
        assert_eq!(parsed["draftStarted"], true);
    }

    #[test]
    fn redact_p2p_backup_snapshot_secrets_strips_nested_draft_session_secrets() {
        let nested = serde_json::json!({
            "draft_code": "ABC123",
            "config": { "rng_seed": 42, "pod_size": 8 },
            "packs_by_seat": [[{"card_id": "secret-pack"}]],
            "status": "Drafting"
        });
        let raw = serde_json::json!({
            "draftCode": "ABC123",
            "draftSessionJson": nested.to_string(),
            "seatTokens": { "0": "host-secret" },
            "draftStarted": true
        });
        let redacted =
            redact_p2p_backup_snapshot_secrets(&raw.to_string()).expect("valid snapshot");
        let parsed: Value = serde_json::from_str(&redacted).unwrap();
        assert!(parsed.get("seatTokens").is_none());
        let nested_out: Value =
            serde_json::from_str(parsed["draftSessionJson"].as_str().unwrap()).unwrap();
        assert_eq!(nested_out["config"]["rng_seed"], 0);
        assert!(nested_out.get("packs_by_seat").is_none());
        assert_eq!(nested_out["status"], "Drafting");
    }

    #[test]
    fn redact_p2p_backup_snapshot_secrets_rejects_non_object() {
        assert!(redact_p2p_backup_snapshot_secrets("[]").is_err());
        assert!(redact_p2p_backup_snapshot_secrets("not-json").is_err());
    }

    #[test]
    fn guard_p2p_backup_overwrite_rejects_peer_mismatch() {
        assert!(guard_p2p_backup_overwrite("peer-a", "peer-b").is_err());
        assert!(guard_p2p_backup_overwrite("peer-a", "peer-a").is_ok());
    }

    #[test]
    fn validate_p2p_backup_host_peer_id_rejects_blank() {
        let err = validate_p2p_backup_host_peer_id("  ").unwrap_err();
        assert!(err.contains("host_peer_id"));
    }
}
