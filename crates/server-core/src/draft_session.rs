use std::collections::HashMap;
use std::time::Duration;

use draft_core::pack_source::PackSource;
use draft_core::types::{DraftAction, DraftConfig, DraftSeat, DraftStatus};
use draft_core::view::DraftPlayerView;
use engine::types::player::PlayerId;
use rand::Rng;
use tracing::{info, warn};

use crate::persist::{PersistedDraftSession, PersistedLobbyMeta};
use crate::reconnect::ReconnectManager;
use crate::session::generate_player_token;

/// Server-side draft session, mirroring `GameSession` for game play.
/// Wraps `draft_core::types::DraftSession` (the pure reducer state) with
/// server-specific concerns: player tokens, connection tracking, reconnect
/// management, timer state, and active match tracking.
pub struct DraftSession {
    pub draft_code: String,
    pub session: draft_core::types::DraftSession,
    /// Per-seat player tokens (seat_index -> token). Empty string = seat not claimed.
    pub player_tokens: Vec<String>,
    pub connected: Vec<bool>,
    pub display_names: Vec<String>,
    pub config: DraftConfig,
    /// Active game sessions spawned at GeneratePairings. match_id -> game_code.
    pub active_matches: HashMap<String, String>,
    /// Lobby metadata -- set at creation, cleared when draft starts.
    pub lobby_meta: Option<PersistedLobbyMeta>,
    /// Server-side remaining pick timer in ms. Injected into DraftPlayerView before send.
    pub timer_remaining_ms: Option<u32>,
    /// JoinHandle for the active pick timer task (prevents double-fire).
    pub timer_task: Option<tokio::task::JoinHandle<()>>,
}

impl DraftSession {
    /// Returns the seat index for the given token, if valid.
    pub fn seat_for_token(&self, token: &str) -> Option<usize> {
        self.player_tokens
            .iter()
            .position(|t| !t.is_empty() && t == token)
    }

    /// Returns the first unclaimed seat index, if any.
    pub fn first_open_seat(&self) -> Option<usize> {
        self.player_tokens.iter().position(|t| t.is_empty())
    }

    /// Returns true if all seats are claimed.
    pub fn is_full(&self) -> bool {
        self.player_tokens.iter().all(|t| !t.is_empty())
    }

    /// Create a serializable snapshot for disk persistence.
    pub fn to_persisted(&self) -> PersistedDraftSession {
        PersistedDraftSession {
            draft_code: self.draft_code.clone(),
            session: self.session.clone(),
            player_tokens: self.player_tokens.clone(),
            display_names: self.display_names.clone(),
            config: self.config.clone(),
            active_matches: self.active_matches.clone(),
            lobby_meta: self.lobby_meta.clone(),
            timer_remaining_ms: self.timer_remaining_ms,
        }
    }

    /// Restore a draft session from a persisted snapshot.
    ///
    /// All players start disconnected. `timer_task` is None — the caller
    /// should re-arm from `timer_remaining_ms` if needed.
    pub fn from_persisted(ps: PersistedDraftSession) -> Self {
        let pod_size = ps.player_tokens.len();
        Self {
            draft_code: ps.draft_code,
            session: ps.session,
            player_tokens: ps.player_tokens,
            connected: vec![false; pod_size],
            display_names: ps.display_names,
            config: ps.config,
            active_matches: ps.active_matches,
            lobby_meta: ps.lobby_meta,
            timer_remaining_ms: ps.timer_remaining_ms,
            timer_task: None,
        }
    }

    /// Inject server-side timer into the filtered view before serializing.
    pub fn view_for_seat(&self, seat: usize) -> DraftPlayerView {
        let mut view = draft_core::view::filter_for_player(&self.session, seat as u8);
        view.timer_remaining_ms = self.timer_remaining_ms;
        view
    }
}

pub struct DraftSessionManager {
    pub sessions: HashMap<String, DraftSession>,
    pub reconnect: ReconnectManager,
    /// Maps player_token -> draft_code for O(1) token-based lookups.
    token_to_draft: HashMap<String, String>,
}

impl DraftSessionManager {
    pub fn new() -> Self {
        Self {
            sessions: HashMap::new(),
            reconnect: ReconnectManager::default(),
            token_to_draft: HashMap::new(),
        }
    }

    /// Create a new draft session. Returns (draft_code, player_token, seat_index).
    ///
    /// The creator occupies seat 0. Remaining seats are empty (awaiting joins).
    pub fn create_draft(
        &mut self,
        config: DraftConfig,
        display_name: String,
    ) -> (String, String, u8) {
        let draft_code = generate_draft_code();
        let player_token = generate_player_token();
        let pod_size = config.kind.pod_size() as usize;

        let mut player_tokens = vec![String::new(); pod_size];
        player_tokens[0] = player_token.clone();

        let mut connected = vec![false; pod_size];
        connected[0] = true;

        let mut display_names = vec![String::new(); pod_size];
        display_names[0] = display_name.clone();

        // Build draft-core seats -- creator is seat 0, rest are empty humans
        let seats: Vec<DraftSeat> = (0..pod_size)
            .map(|i| DraftSeat::Human {
                player_id: PlayerId(i as u8),
                display_name: if i == 0 {
                    display_name.clone()
                } else {
                    String::new()
                },
                connected: i == 0,
            })
            .collect();

        let inner = draft_core::types::DraftSession::new(config.clone(), seats, draft_code.clone());

        let session = DraftSession {
            draft_code: draft_code.clone(),
            session: inner,
            player_tokens,
            connected,
            display_names,
            config,
            active_matches: HashMap::new(),
            lobby_meta: None,
            timer_remaining_ms: None,
            timer_task: None,
        };

        self.token_to_draft
            .insert(player_token.clone(), draft_code.clone());
        self.sessions.insert(draft_code.clone(), session);

        info!(draft = %draft_code, "draft session created");

        (draft_code, player_token, 0)
    }

    /// Join an existing draft. Returns (player_token, seat_index, initial_view).
    pub fn join_draft(
        &mut self,
        draft_code: &str,
        display_name: String,
        _password: Option<&str>,
    ) -> Result<(String, u8, DraftPlayerView), String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;

        let seat = session
            .first_open_seat()
            .ok_or_else(|| "Draft is already full".to_string())?;

        let player_token = generate_player_token();
        session.player_tokens[seat] = player_token.clone();
        session.connected[seat] = true;
        session.display_names[seat] = display_name.clone();

        // Update the draft-core seat
        session.session.seats[seat] = DraftSeat::Human {
            player_id: PlayerId(seat as u8),
            display_name,
            connected: true,
        };

        self.token_to_draft
            .insert(player_token.clone(), draft_code.to_string());

        info!(draft = %draft_code, seat, "player joined draft");

        let view = session.view_for_seat(seat);
        Ok((player_token, seat as u8, view))
    }

    /// Handle a draft action from a player. Validates token -> seat mapping
    /// before delegating to draft-core. Returns views for all seats.
    pub fn handle_draft_action(
        &mut self,
        draft_code: &str,
        token: &str,
        action: DraftAction,
        pack_source: Option<&dyn PackSource>,
    ) -> Result<Vec<DraftPlayerView>, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;

        let _seat = session
            .seat_for_token(token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        let _deltas = draft_core::session::apply(&mut session.session, action, pack_source)
            .map_err(|e| {
                warn!(draft = %draft_code, error = %e, "draft action rejected");
                format!("Draft error: {}", e)
            })?;

        // Broadcast updated view to all connected seats
        let views: Vec<_> = (0..session.player_tokens.len())
            .map(|i| session.view_for_seat(i))
            .collect();
        Ok(views)
    }

    /// Apply an action without token validation (for server-internal use,
    /// e.g. GameOver auto-report). Lock ordering: always acquire draft_sessions
    /// before sessions (game sessions) to prevent deadlock.
    pub fn apply_system_action(
        &mut self,
        draft_code: &str,
        action: DraftAction,
        pack_source: Option<&dyn PackSource>,
    ) -> Result<Vec<DraftPlayerView>, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;

        let _deltas = draft_core::session::apply(&mut session.session, action, pack_source)
            .map_err(|e| {
                warn!(draft = %draft_code, error = %e, "system draft action rejected");
                format!("Draft error: {}", e)
            })?;

        let views: Vec<_> = (0..session.player_tokens.len())
            .map(|i| session.view_for_seat(i))
            .collect();
        Ok(views)
    }

    /// Mark a player as disconnected.
    pub fn handle_disconnect(&mut self, draft_code: &str, seat: usize) {
        if let Some(session) = self.sessions.get_mut(draft_code) {
            session.connected[seat] = false;
            let fake_pid = PlayerId(seat as u8);
            let default_grace = self.reconnect.grace_period;
            self.reconnect
                .record_disconnect(draft_code, fake_pid, default_grace);
            info!(draft = %draft_code, seat, "player disconnected");
        }
    }

    /// Attempt to reconnect a player. Returns their filtered view on success.
    pub fn handle_reconnect(
        &mut self,
        draft_code: &str,
        token: &str,
    ) -> Result<DraftPlayerView, String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("Draft not found: {}", draft_code))?;

        let seat = session
            .seat_for_token(token)
            .ok_or_else(|| "Invalid player token".to_string())?;

        let fake_pid = PlayerId(seat as u8);
        match self.reconnect.attempt_reconnect(draft_code, fake_pid) {
            crate::reconnect::ReconnectResult::Ok { .. }
            | crate::reconnect::ReconnectResult::NotFound => {
                session.connected[seat] = true;
                Ok(session.view_for_seat(seat))
            }
            crate::reconnect::ReconnectResult::Expired => {
                Err("Reconnect grace period expired".to_string())
            }
        }
    }

    /// O(1) lookup: player_token -> draft_code.
    pub fn draft_for_token(&self, token: &str) -> Option<&str> {
        self.token_to_draft.get(token).map(|s| s.as_str())
    }

    /// Restore a previously persisted draft session, rebuilding the token_to_draft index.
    pub fn restore_session(&mut self, ps: PersistedDraftSession) {
        let session = DraftSession::from_persisted(ps);
        let draft_code = session.draft_code.clone();
        for token in &session.player_tokens {
            if !token.is_empty() {
                self.token_to_draft
                    .insert(token.clone(), draft_code.clone());
            }
        }
        self.sessions.insert(draft_code, session);
    }

    /// Auto-pick a random card for a disconnected seat whose grace period expired.
    ///
    /// Returns `Ok(())` if a pick was made. Only fires during the Drafting phase (D-02).
    /// Called from the draft disconnect expiry path in phase-server.
    pub fn pick_random_for_seat(
        &mut self,
        draft_code: &str,
        seat: u8,
        pack_source: Option<&dyn PackSource>,
    ) -> Result<(), String> {
        let session = self
            .sessions
            .get_mut(draft_code)
            .ok_or_else(|| format!("draft {draft_code} not found"))?;

        if session.session.status != DraftStatus::Drafting {
            return Err("draft not in Drafting status".into());
        }

        let view = draft_core::view::filter_for_player(&session.session, seat);
        let pack = view
            .current_pack
            .ok_or_else(|| format!("seat {seat} has no pending pack"))?;

        if pack.is_empty() {
            return Err(format!("seat {seat} pack is empty"));
        }

        let idx = rand::rng().random_range(0..pack.len());
        let card_instance_id = pack[idx].instance_id.clone();

        let action = DraftAction::Pick {
            seat,
            card_instance_id,
        };
        draft_core::session::apply(&mut session.session, action, pack_source)
            .map_err(|e| format!("auto-pick failed: {e}"))?;

        info!(draft = %draft_code, seat, "auto-picked random card for disconnected seat");
        Ok(())
    }

    /// Scan active_matches across all sessions to find the draft owning a game.
    pub fn draft_for_game_code(&self, game_code: &str) -> Option<String> {
        self.sessions
            .values()
            .find(|s| s.active_matches.values().any(|gc| gc == game_code))
            .map(|s| s.draft_code.clone())
    }

    /// Remove a draft session entirely, cleaning up the token_to_draft index.
    /// Returns the removed session if it existed.
    pub fn remove_draft(&mut self, draft_code: &str) -> Option<DraftSession> {
        let session = self.sessions.remove(draft_code)?;
        for token in &session.player_tokens {
            if !token.is_empty() {
                self.token_to_draft.remove(token);
            }
        }
        Some(session)
    }
}

impl Default for DraftSessionManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Generate a 6-character uppercase alphanumeric draft code.
pub fn generate_draft_code() -> String {
    let mut rng = rand::rng();
    let chars: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789".chars().collect();
    (0..6)
        .map(|_| chars[rng.random_range(0..chars.len())])
        .collect()
}

/// Returns the appropriate reconnect grace period for the given draft phase.
///
/// Longer than the 10s game reconnect because tournaments span hours.
/// - Lobby: 30 min (gathering players)
/// - Drafting: 5 min (picks in progress, auto-pick kicks in after)
/// - Deckbuilding: 15 min (building takes time)
/// - MatchInProgress / BetweenRounds: 10 min
/// - Complete / Abandoned: 1 min (draft is over)
/// - Paused / Pairing / RoundComplete: 10 min (transient states)
pub fn draft_grace_period(status: &DraftStatus) -> Duration {
    match status {
        DraftStatus::Lobby => Duration::from_secs(1800),
        DraftStatus::Drafting => Duration::from_secs(300),
        DraftStatus::Deckbuilding => Duration::from_secs(900),
        DraftStatus::MatchInProgress => Duration::from_secs(600),
        DraftStatus::RoundComplete => Duration::from_secs(600),
        DraftStatus::Paused => Duration::from_secs(600),
        DraftStatus::Pairing => Duration::from_secs(600),
        DraftStatus::Complete | DraftStatus::Abandoned => Duration::from_secs(60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use draft_core::types::{DraftKind, PodPolicy, SpectatorVisibility, TournamentFormat};

    fn test_config() -> DraftConfig {
        DraftConfig {
            set_code: "TST".to_string(),
            kind: DraftKind::Premier,
            cards_per_pack: 14,
            pack_count: 3,
            rng_seed: 42,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        }
    }

    #[test]
    fn create_draft_returns_code_and_token() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, seat) = mgr.create_draft(test_config(), "Alice".to_string());

        assert_eq!(code.len(), 6);
        assert_eq!(token.len(), 32);
        assert_eq!(seat, 0);
        assert!(mgr.sessions.contains_key(&code));
    }

    #[test]
    fn join_draft_assigns_seat() {
        let mut mgr = DraftSessionManager::new();
        let (code, _host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        let result = mgr.join_draft(&code, "Bob".to_string(), None);
        assert!(result.is_ok());
        let (token, seat, _view) = result.unwrap();
        assert_eq!(token.len(), 32);
        assert_eq!(seat, 1);
    }

    #[test]
    fn join_full_draft_fails() {
        let mut mgr = DraftSessionManager::new();
        let (code, _host_token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        // Fill all 8 seats (seat 0 is the host)
        for i in 1..8 {
            let result = mgr.join_draft(&code, format!("Player {i}"), None);
            assert!(result.is_ok(), "Failed to join seat {i}");
        }

        // 9th join should fail
        let result = mgr.join_draft(&code, "TooMany".to_string(), None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("full"));
    }

    #[test]
    fn draft_for_token_lookup_works() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        assert_eq!(mgr.draft_for_token(&token), Some(code.as_str()));
        assert_eq!(mgr.draft_for_token("nonexistent"), None);
    }

    #[test]
    fn disconnect_and_reconnect_works() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        mgr.handle_disconnect(&code, 0);
        assert!(!mgr.sessions[&code].connected[0]);

        let result = mgr.handle_reconnect(&code, &token);
        assert!(result.is_ok());
        assert!(mgr.sessions[&code].connected[0]);
    }

    #[test]
    fn handle_draft_action_validates_token() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        let result = mgr.handle_draft_action(&code, "invalid-token", DraftAction::StartDraft, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid player token"));
    }

    #[test]
    fn apply_system_action_bypasses_token() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        // Fill the pod so we can start
        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }

        // System action bypasses token validation
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        let result = mgr.apply_system_action(&code, DraftAction::StartDraft, Some(&source));
        assert!(result.is_ok());

        // Verify session transitioned to Drafting
        assert_eq!(mgr.sessions[&code].session.status, DraftStatus::Drafting);
    }

    #[test]
    fn draft_code_is_uppercase_alphanumeric() {
        let code = generate_draft_code();
        assert_eq!(code.len(), 6);
        assert!(code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()));
    }

    #[test]
    fn draft_for_game_code_finds_match() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        mgr.sessions
            .get_mut(&code)
            .unwrap()
            .active_matches
            .insert("r1-t0".to_string(), "GAME01".to_string());

        assert_eq!(mgr.draft_for_game_code("GAME01"), Some(code));
        assert_eq!(mgr.draft_for_game_code("NONEXIST"), None);
    }

    #[test]
    fn to_persisted_roundtrips_through_serde_json() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }

        let session = &mgr.sessions[&code];
        let persisted = session.to_persisted();
        let json = serde_json::to_string(&persisted).expect("serialize");
        let restored: PersistedDraftSession = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.draft_code, persisted.draft_code);
        assert_eq!(restored.player_tokens, persisted.player_tokens);
        assert_eq!(restored.display_names, persisted.display_names);
        assert_eq!(restored.timer_remaining_ms, persisted.timer_remaining_ms);
    }

    #[test]
    fn from_persisted_sets_connected_false_and_timer_task_none() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        let persisted = mgr.sessions[&code].to_persisted();
        let restored = DraftSession::from_persisted(persisted);

        assert!(restored.connected.iter().all(|&c| !c));
        assert!(restored.timer_task.is_none());
    }

    #[test]
    fn restore_session_rebuilds_token_to_draft_index() {
        let mut mgr = DraftSessionManager::new();
        let (code, token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        for i in 1..8 {
            mgr.join_draft(&code, format!("Player {i}"), None).unwrap();
        }

        let persisted = mgr.sessions[&code].to_persisted();
        let tokens = persisted.player_tokens.clone();

        // Create a fresh manager and restore into it
        let mut mgr2 = DraftSessionManager::new();
        mgr2.restore_session(persisted);

        // All tokens should resolve to the same draft code
        for t in &tokens {
            if !t.is_empty() {
                assert_eq!(mgr2.draft_for_token(t), Some(code.as_str()));
            }
        }

        // Original host token should still work
        assert_eq!(mgr2.draft_for_token(&token), Some(code.as_str()));
    }

    fn fill_and_start(mgr: &mut DraftSessionManager, code: &str) {
        for i in 1..8 {
            mgr.join_draft(code, format!("Player {i}"), None).unwrap();
        }
        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };
        mgr.apply_system_action(code, DraftAction::StartDraft, Some(&source))
            .unwrap();
    }

    #[test]
    fn pick_random_for_seat_picks_from_available_pack() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());
        fill_and_start(&mut mgr, &code);

        let source = draft_core::pack_source::FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack: 14,
        };

        // Get seat 0's pool size before auto-pick
        let pool_before = mgr.sessions[&code].session.pools[0].len();

        // Auto-pick for seat 0
        let result = mgr.pick_random_for_seat(&code, 0, Some(&source));
        assert!(result.is_ok(), "auto-pick failed: {:?}", result.err());

        // Pool should grow by 1
        let pool_after = mgr.sessions[&code].session.pools[0].len();
        assert_eq!(pool_after, pool_before + 1);
    }

    #[test]
    fn pick_random_for_seat_fails_when_not_drafting() {
        let mut mgr = DraftSessionManager::new();
        let (code, _token, _) = mgr.create_draft(test_config(), "Alice".to_string());

        // Session is in Lobby status, not Drafting
        let result = mgr.pick_random_for_seat(&code, 0, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not in Drafting status"));
    }

    #[test]
    fn draft_grace_period_returns_correct_durations() {
        assert_eq!(
            draft_grace_period(&DraftStatus::Lobby),
            Duration::from_secs(1800)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::Drafting),
            Duration::from_secs(300)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::Deckbuilding),
            Duration::from_secs(900)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::MatchInProgress),
            Duration::from_secs(600)
        );
        assert_eq!(
            draft_grace_period(&DraftStatus::Complete),
            Duration::from_secs(60)
        );
    }
}
