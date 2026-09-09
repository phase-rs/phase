use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};

use server_core::{
    guard_p2p_backup, guard_p2p_backup_overwrite, redact_p2p_backup_snapshot_secrets,
    validate_p2p_backup_host_peer_id,
};

use crate::AppState;

/// Validate draft code format: exactly 6 alphanumeric uppercase chars.
fn is_valid_draft_code(code: &str) -> bool {
    code.len() == 6
        && code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
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
            drop(drafts);
            // Remove active game sessions spawned by this draft (Pitfall 4 mitigation)
            let match_codes: Vec<String> = session.active_matches.values().cloned().collect();
            if !match_codes.is_empty() {
                let mut sessions = app_state.sessions.lock().await;
                for game_code in &match_codes {
                    sessions.remove_game(game_code);
                }
            }
            // A destroyed subject leaves no orphaned routing behind: the
            // abandon teardown clears connections, spectators and the lobby
            // entry, and a force-delete destroys the same kinds of subject.
            {
                let mut conns = app_state.connections.lock().await;
                conns.remove(&code);
                for game_code in &match_codes {
                    conns.remove(game_code);
                }
            }
            app_state.draft_spectators.lock().await.remove(&code);
            {
                let mut specs = app_state.game_spectators.lock().await;
                for game_code in &match_codes {
                    specs.remove(game_code);
                }
            }
            crate::delist_and_announce(
                &app_state.lobby,
                &app_state.lobby_subscribers,
                std::iter::once(code.as_str()),
            )
            .await;
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
    if let Err(reason) = guard_p2p_backup(&req.host_peer_id, &req.snapshot_json) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }
    if let Ok(Some((existing_peer, _, _))) = app_state.game_db.load_p2p_backup(&req.draft_code) {
        if let Err(reason) = guard_p2p_backup_overwrite(&existing_peer, &req.host_peer_id) {
            return (StatusCode::FORBIDDEN, reason).into_response();
        }
    }
    let snapshot_json = match redact_p2p_backup_snapshot_secrets(&req.snapshot_json) {
        Ok(json) => json,
        Err(reason) => return (StatusCode::BAD_REQUEST, reason).into_response(),
    };
    match app_state
        .game_db
        .save_p2p_backup(&req.draft_code, &req.host_peer_id, &snapshot_json)
    {
        Ok(_) => (StatusCode::OK, "Stored").into_response(),
        Err(e) => {
            warn!(error = %e, "P2P backup save failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Storage failed").into_response()
        }
    }
}

/// Query params for `GET /p2p-draft-backup/:code`.
#[derive(Deserialize)]
pub struct P2pBackupGetQuery {
    pub host_peer_id: String,
}

/// GET /p2p-draft-backup/:code — Retrieve a P2P draft backup.
pub async fn p2p_backup_get(
    State(app_state): State<AppState>,
    Path(code): Path<String>,
    Query(query): Query<P2pBackupGetQuery>,
) -> impl IntoResponse {
    if !is_valid_draft_code(&code) {
        return (StatusCode::BAD_REQUEST, "Invalid draft code").into_response();
    }
    if let Err(reason) = validate_p2p_backup_host_peer_id(&query.host_peer_id) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }
    match app_state.game_db.load_p2p_backup(&code) {
        Ok(Some((existing_peer, snapshot_json, updated_at))) => {
            if guard_p2p_backup_overwrite(&existing_peer, &query.host_peer_id).is_err() {
                return (StatusCode::NOT_FOUND, "No backup found").into_response();
            }
            let snapshot_json = match redact_p2p_backup_snapshot_secrets(&snapshot_json) {
                Ok(json) => json,
                Err(reason) => return (StatusCode::INTERNAL_SERVER_ERROR, reason).into_response(),
            };
            Json(serde_json::json!({
                "host_peer_id": existing_peer,
                "snapshot_json": snapshot_json,
                "updated_at": updated_at,
            }))
            .into_response()
        }
        Ok(None) => (StatusCode::NOT_FOUND, "No backup found").into_response(),
        Err(e) => {
            warn!(error = %e, "P2P backup load failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Load failed").into_response()
        }
    }
}

/// Query params for `DELETE /p2p-draft-backup/:code`.
#[derive(Deserialize)]
pub struct P2pBackupDeleteQuery {
    pub host_peer_id: String,
}

/// DELETE /p2p-draft-backup/:code — Remove a P2P draft backup.
///
/// Requires `host_peer_id` to match the row owner (same contract as POST
/// overwrite) so knowing the 6-char draft code alone cannot grief-delete a
/// recovery snapshot.
pub async fn p2p_backup_delete(
    State(app_state): State<AppState>,
    Path(code): Path<String>,
    Query(query): Query<P2pBackupDeleteQuery>,
) -> impl IntoResponse {
    if !is_valid_draft_code(&code) {
        return (StatusCode::BAD_REQUEST, "Invalid draft code").into_response();
    }
    if let Err(reason) = validate_p2p_backup_host_peer_id(&query.host_peer_id) {
        return (StatusCode::BAD_REQUEST, reason).into_response();
    }
    match app_state.game_db.load_p2p_backup(&code) {
        Ok(Some((existing_peer, _, _))) => {
            if let Err(reason) = guard_p2p_backup_overwrite(&existing_peer, &query.host_peer_id) {
                return (StatusCode::FORBIDDEN, reason).into_response();
            }
            match app_state.game_db.delete_p2p_backup(&code) {
                Ok(()) => (StatusCode::OK, "Deleted").into_response(),
                Err(e) => {
                    warn!(error = %e, "P2P backup delete failed");
                    (StatusCode::INTERNAL_SERVER_ERROR, "Delete failed").into_response()
                }
            }
        }
        Ok(None) => (StatusCode::NOT_FOUND, "No backup found").into_response(),
        Err(e) => {
            warn!(error = %e, "P2P backup load failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "Load failed").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU32;
    use std::sync::Arc;

    use axum::extract::{Path, State};
    use axum::response::IntoResponse;
    use draft_core::types::{
        DeckAddableCards, DraftConfig, DraftKind, DraftSource, PodPolicy, SpectatorVisibility,
        TournamentFormat,
    };
    use lobby_broker::lobby::RegisterGameRequest;
    use lobby_broker::{Broker, BrokerEnv};
    use server_core::draft_session::DraftSessionManager;
    use server_core::session::SessionManager;
    use tokio::sync::{mpsc, Mutex};

    use crate::{draft_pools, persistence, AppState, PlayerId, ServerContext, ServerMode};

    struct FixedEnv;
    impl BrokerEnv for FixedEnv {
        fn now_ms(&self) -> u64 {
            1_000
        }
        fn new_token(&self) -> String {
            "token".to_string()
        }
        fn new_game_code(&self) -> String {
            "CODE00".to_string()
        }
    }

    fn draft_config() -> DraftConfig {
        DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            pod_size: 8,
            cards_per_pack: 14,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        }
    }

    fn app_state(temp_dir: &tempfile::TempDir) -> AppState {
        let game_db = Arc::new(
            persistence::GameDb::open(
                &temp_dir.path().join("games.db"),
                persistence::SessionRetention::Multiplayer,
            )
            .expect("game db"),
        );
        AppState {
            sessions: Arc::new(Mutex::new(SessionManager::new())),
            draft_sessions: Arc::new(Mutex::new(DraftSessionManager::new())),
            draft_pools: Arc::new(draft_pools::DraftPools::default()),
            connections: Arc::new(Mutex::new(HashMap::new())),
            db: Arc::new(engine::database::CardDatabase::default()),
            lobby: Arc::new(Mutex::new(Broker::new())),
            lobby_subscribers: Arc::new(Mutex::new(Vec::new())),
            player_count: Arc::new(AtomicU32::new(0)),
            game_db,
            draft_spectators: Arc::new(Mutex::new(HashMap::new())),
            game_spectators: Arc::new(Mutex::new(HashMap::new())),
            mode: ServerMode::Full,
            context: ServerContext::default(),
            public_url: None,
            allowed_origin: None,
        }
    }

    /// A force-deleted draft leaves no orphaned routing behind, for both kinds
    /// of subject it destroys: the draft itself and each game it spawned.
    #[tokio::test]
    async fn force_delete_orphans_no_map_for_the_draft_or_its_matches() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let state = app_state(&temp_dir);
        let match_code = "MATCH1".to_string();

        let draft_code = {
            let mut drafts = state.draft_sessions.lock().await;
            let (draft_code, _token, _seat) =
                drafts.create_draft(draft_config(), "Host".to_string());
            drafts
                .sessions
                .get_mut(&draft_code)
                .expect("draft")
                .active_matches
                .insert("m1".to_string(), match_code.clone());
            draft_code
        };

        let (tx, _rx) = mpsc::unbounded_channel();
        state.connections.lock().await.insert(
            draft_code.clone(),
            HashMap::from([(PlayerId(0), tx.clone())]),
        );
        state.connections.lock().await.insert(
            match_code.clone(),
            HashMap::from([(PlayerId(0), tx.clone())]),
        );
        state.draft_spectators.lock().await.insert(
            draft_code.clone(),
            vec![(SpectatorVisibility::default(), tx.clone())],
        );
        state
            .game_spectators
            .lock()
            .await
            .insert(match_code.clone(), vec![tx.clone()]);
        {
            let mut lob = state.lobby.lock().await;
            lob.lobby_mut().register_game(
                &draft_code,
                RegisterGameRequest {
                    host_name: "Host".to_string(),
                    public: true,
                    ..Default::default()
                },
                &FixedEnv,
            );
        }

        // Reach guard: every map really is populated before the delete, so the
        // emptiness assertions below cannot pass on a fixture that never wired
        // anything up.
        assert!(state.connections.lock().await.contains_key(&draft_code));
        assert!(state.connections.lock().await.contains_key(&match_code));
        assert!(state
            .draft_spectators
            .lock()
            .await
            .contains_key(&draft_code));
        assert!(state.game_spectators.lock().await.contains_key(&match_code));
        assert!(state.lobby.lock().await.lobby_mut().has_game(&draft_code));

        let response = super::admin_delete_draft(State(state.clone()), Path(draft_code.clone()))
            .await
            .into_response();
        assert_eq!(response.status(), axum::http::StatusCode::OK);

        assert!(!state
            .draft_sessions
            .lock()
            .await
            .sessions
            .contains_key(&draft_code));
        assert!(!state.connections.lock().await.contains_key(&draft_code));
        assert!(!state.connections.lock().await.contains_key(&match_code));
        assert!(!state
            .draft_spectators
            .lock()
            .await
            .contains_key(&draft_code));
        assert!(!state.game_spectators.lock().await.contains_key(&match_code));
        assert!(!state.lobby.lock().await.lobby_mut().has_game(&draft_code));
    }
}
