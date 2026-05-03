use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

use crate::AppState;

/// Validate draft code format: exactly 6 alphanumeric uppercase chars.
fn is_valid_draft_code(code: &str) -> bool {
    code.len() == 6 && code.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// GET /admin/drafts — List all active draft sessions with summary info.
pub async fn admin_list_drafts(State(app_state): State<AppState>) -> Json<Value> {
    let drafts = app_state.draft_sessions.lock().await;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let list: Vec<Value> = drafts
        .sessions
        .values()
        .map(|s| {
            serde_json::json!({
                "draft_code": s.draft_code,
                "player_count": s.player_tokens.iter().filter(|t| !t.is_empty()).count(),
                "connected_players": s.connected.iter().filter(|&&c| c).count(),
                "status": format!("{:?}", s.session.status),
                "elapsed_minutes": now.saturating_sub(s.session.created_at) / 60,
            })
        })
        .collect();
    Json(serde_json::json!({ "drafts": list }))
}

/// GET /admin/drafts/:code — Inspect full draft session state.
pub async fn admin_get_draft(
    State(app_state): State<AppState>,
    Path(code): Path<String>,
) -> impl IntoResponse {
    if !is_valid_draft_code(&code) {
        return (StatusCode::BAD_REQUEST, "Invalid draft code").into_response();
    }
    let drafts = app_state.draft_sessions.lock().await;
    match drafts.sessions.get(&code) {
        Some(session) => {
            let persisted = session.to_persisted();
            match serde_json::to_value(&persisted) {
                Ok(val) => Json(val).into_response(),
                Err(_) => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "Serialization failed").into_response()
                }
            }
        }
        None => (StatusCode::NOT_FOUND, "Draft not found").into_response(),
    }
}

/// DELETE /admin/drafts/:code — Force-end a draft session and clean up.
pub async fn admin_delete_draft(
    State(app_state): State<AppState>,
    Path(code): Path<String>,
) -> impl IntoResponse {
    if !is_valid_draft_code(&code) {
        return (StatusCode::BAD_REQUEST, "Invalid draft code").into_response();
    }
    let mut drafts = app_state.draft_sessions.lock().await;
    match drafts.remove_draft(&code) {
        Some(session) => {
            // Remove active game sessions spawned by this draft (Pitfall 4 mitigation)
            if !session.active_matches.is_empty() {
                let mut sessions = app_state.sessions.lock().await;
                for game_code in session.active_matches.values() {
                    sessions.sessions.remove(game_code);
                }
            }
            // Delete from persistence
            let _ = app_state.game_db.delete_draft_session(&code);
            info!(draft = %code, "admin force-deleted draft session");
            (StatusCode::OK, "Deleted").into_response()
        }
        None => (StatusCode::NOT_FOUND, "Draft not found").into_response(),
    }
}

/// POST /p2p-draft-backup — Store a P2P draft state snapshot.
#[derive(Deserialize)]
pub struct P2pBackupRequest {
    pub draft_code: String,
    pub host_peer_id: String,
    pub snapshot_json: String,
}

pub async fn p2p_backup_store(
    State(app_state): State<AppState>,
    Json(req): Json<P2pBackupRequest>,
) -> impl IntoResponse {
    if !is_valid_draft_code(&req.draft_code) {
        return (StatusCode::BAD_REQUEST, "Invalid draft code").into_response();
    }
    match app_state
        .game_db
        .save_p2p_backup(&req.draft_code, &req.host_peer_id, &req.snapshot_json)
    {
        Ok(_) => (StatusCode::OK, "Stored").into_response(),
        Err(e) => {
            warn!(error = %e, "P2P backup save failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Storage failed").into_response()
        }
    }
}

/// GET /p2p-draft-backup/:code — Retrieve a P2P draft backup.
pub async fn p2p_backup_get(
    State(app_state): State<AppState>,
    Path(code): Path<String>,
) -> impl IntoResponse {
    if !is_valid_draft_code(&code) {
        return (StatusCode::BAD_REQUEST, "Invalid draft code").into_response();
    }
    match app_state.game_db.load_p2p_backup(&code) {
        Ok(Some((host_peer_id, snapshot_json, updated_at))) => Json(serde_json::json!({
            "host_peer_id": host_peer_id,
            "snapshot_json": snapshot_json,
            "updated_at": updated_at,
        }))
        .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "No backup found").into_response(),
        Err(e) => {
            warn!(error = %e, "P2P backup load failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Load failed").into_response()
        }
    }
}

/// DELETE /p2p-draft-backup/:code — Remove a P2P draft backup.
pub async fn p2p_backup_delete(
    State(app_state): State<AppState>,
    Path(code): Path<String>,
) -> impl IntoResponse {
    if !is_valid_draft_code(&code) {
        return (StatusCode::BAD_REQUEST, "Invalid draft code").into_response();
    }
    let _ = app_state.game_db.delete_p2p_backup(&code);
    (StatusCode::OK, "Deleted").into_response()
}
