//! `LobbyManager` — the pure, WASM-safe registry of waiting lobby entries.
//!
//! Moved here verbatim from `server_core::lobby` (plan §2), with one change:
//! the two WASM hazards — `SystemTime::now()` (created_at / reservation
//! expiry / staleness) and `generate_player_token()` (reservation tokens) —
//! are replaced by [`BrokerEnv`] calls threaded into the methods that need
//! them. No game logic was altered; the unit tests carried over from
//! server-core (with a deterministic fake `BrokerEnv`) guard the behavior.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::env::BrokerEnv;
use crate::protocol::{DraftLobbyMetadata, LobbyGame};

/// Public-seat reservation lifetime (ms). DUPLICATED from
/// `server_core::session::PUBLIC_SEAT_RESERVATION_MS` deliberately (plan
/// decision M3): that const is also used by Full-mode `SessionManager`, so
/// importing it here would force `session.rs` to depend on this WASM-leaf
/// crate. It is a policy constant, not logic — keep the literal in both places
/// with a cross-reference so a future change touches both.
pub const PUBLIC_SEAT_RESERVATION_MS: u64 = 120_000;

/// Fields a caller supplies when registering a lobby entry. Using a struct
/// here rather than a long positional argument list means adding a new field
/// doesn't require touching every caller — just add it here with a `Default`
/// and populate where relevant.
#[derive(Debug, Clone, Default)]
pub struct RegisterGameRequest {
    pub host_name: String,
    pub public: bool,
    pub password: Option<String>,
    pub timer_seconds: Option<u32>,
    pub host_version: String,
    pub host_build_commit: String,
    pub current_players: u32,
    pub max_players: u32,
    pub format_config: Option<engine::types::format::FormatConfig>,
    pub match_config: engine::types::match_config::MatchConfig,
    /// Optional match-scoped label shown in lobby listings.
    pub room_name: Option<String>,
    /// PeerJS peer ID of the host for lobby-only server mode. Empty string on
    /// `Full`-mode servers.
    pub host_peer_id: String,
    /// Draft-specific metadata for lobby display. `None` for constructed-play.
    pub draft_metadata: Option<DraftLobbyMetadata>,
}

/// Fields returned by `join_target_info` — everything the server needs to
/// answer a typed-code lookup or populate `PeerInfo` for a brokered join in
/// one atomic snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinTargetInfo {
    pub host_peer_id: String,
    pub max_players: u32,
    pub current_players: u32,
    pub format_config: Option<engine::types::format::FormatConfig>,
    pub match_config: engine::types::match_config::MatchConfig,
    pub is_p2p: bool,
    pub reservation_token: Option<String>,
    pub reservation_expires_at_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LobbyReservation {
    pub token: String,
    pub display_name: String,
    pub expires_at_ms: Option<u64>,
}

#[derive(Serialize, Deserialize)]
struct LobbyGameMeta {
    host_name: String,
    created_at: u64,
    password: Option<String>,
    has_password: bool,
    timer_seconds: Option<u32>,
    public: bool,
    host_version: String,
    host_build_commit: String,
    current_players: u32,
    max_players: u32,
    format_config: Option<engine::types::format::FormatConfig>,
    match_config: engine::types::match_config::MatchConfig,
    room_name: Option<String>,
    host_peer_id: String,
    draft_metadata: Option<DraftLobbyMetadata>,
    reservations: HashMap<String, LobbyReservation>,
}

#[derive(Serialize, Deserialize)]
pub struct LobbyManager {
    games: HashMap<String, LobbyGameMeta>,
}

impl LobbyManager {
    pub fn new() -> Self {
        Self {
            games: HashMap::new(),
        }
    }

    pub fn register_game(
        &mut self,
        game_code: &str,
        req: RegisterGameRequest,
        env: &impl BrokerEnv,
    ) {
        let has_password = req.password.is_some();
        let created_at = env.now_ms() / 1000;

        debug!(
            game = %game_code,
            host = %req.host_name,
            version = %req.host_version,
            commit = %req.host_build_commit,
            "lobby game registered"
        );

        self.games.insert(
            game_code.to_string(),
            LobbyGameMeta {
                host_name: req.host_name,
                created_at,
                password: req.password,
                has_password,
                timer_seconds: req.timer_seconds,
                public: req.public,
                host_version: req.host_version,
                host_build_commit: req.host_build_commit,
                current_players: req.current_players,
                max_players: req.max_players,
                format_config: req.format_config,
                match_config: req.match_config,
                room_name: req.room_name,
                host_peer_id: req.host_peer_id,
                draft_metadata: req.draft_metadata,
                reservations: HashMap::new(),
            },
        );
    }

    fn cleanup_expired_for(meta: &mut LobbyGameMeta, now_ms: u64) -> bool {
        let before = meta.reservations.len();
        meta.reservations.retain(|_, reservation| {
            reservation
                .expires_at_ms
                .is_none_or(|expires| expires > now_ms)
        });
        before != meta.reservations.len()
    }

    pub fn cleanup_expired_reservations(&mut self, game_code: &str, env: &impl BrokerEnv) -> bool {
        let now = env.now_ms();
        self.games
            .get_mut(game_code)
            .is_some_and(|meta| Self::cleanup_expired_for(meta, now))
    }

    pub fn reserve_seat(
        &mut self,
        game_code: &str,
        display_name: String,
        env: &impl BrokerEnv,
    ) -> Result<LobbyReservation, String> {
        let now = env.now_ms();
        let meta = self
            .games
            .get_mut(game_code)
            .ok_or_else(|| format!("Game not found in lobby: {}", game_code))?;
        Self::cleanup_expired_for(meta, now);
        let occupied = meta.current_players + meta.reservations.len() as u32;
        if meta.max_players > 0 && occupied >= meta.max_players {
            return Err(format!("Game {game_code} is full"));
        }
        let token = env.new_token();
        let reservation = LobbyReservation {
            token: token.clone(),
            display_name,
            expires_at_ms: if meta.has_password {
                None
            } else {
                Some(now + PUBLIC_SEAT_RESERVATION_MS)
            },
        };
        meta.reservations.insert(token, reservation.clone());
        Ok(reservation)
    }

    pub fn release_reservation(&mut self, game_code: &str, token: &str) -> bool {
        self.games
            .get_mut(game_code)
            .and_then(|meta| meta.reservations.remove(token))
            .is_some()
    }

    pub fn has_active_reservation(
        &mut self,
        game_code: &str,
        token: &str,
        env: &impl BrokerEnv,
    ) -> bool {
        let Some(meta) = self.games.get_mut(game_code) else {
            return false;
        };
        Self::cleanup_expired_for(meta, env.now_ms());
        meta.reservations.contains_key(token)
    }

    pub fn release_reservations(&mut self, reservations: &[(String, String)]) -> bool {
        let mut changed = false;
        for (game_code, token) in reservations {
            changed |= self.release_reservation(game_code, token);
        }
        changed
    }

    pub fn consume_reservation(&mut self, game_code: &str, token: &str) -> bool {
        let Some(meta) = self.games.get_mut(game_code) else {
            return false;
        };
        if meta.reservations.remove(token).is_none() {
            return false;
        }
        meta.current_players = (meta.current_players + 1).min(meta.max_players);
        true
    }

    /// Returns the seated player count, excluding pending reservations.
    pub fn seated_player_count(&self, game_code: &str) -> Option<u32> {
        self.games.get(game_code).map(|meta| meta.current_players)
    }

    /// Updates the `current_players` count for an existing lobby entry. No-op
    /// if the game isn't tracked.
    pub fn set_current_players(
        &mut self,
        game_code: &str,
        current_players: u32,
        env: &impl BrokerEnv,
    ) {
        let now = env.now_ms();
        if let Some(meta) = self.games.get_mut(game_code) {
            Self::cleanup_expired_for(meta, now);
            meta.current_players = current_players;
        }
    }

    /// Updates the `max_players` count for an existing lobby entry. No-op if
    /// the game isn't tracked.
    pub fn set_max_players(&mut self, game_code: &str, max: u8) {
        if let Some(meta) = self.games.get_mut(game_code) {
            meta.max_players = max as u32;
        }
    }

    /// Returns the host's build identity for a game, used to gate joins when
    /// the guest's build differs from the host's.
    pub fn host_build_commit(&self, game_code: &str) -> Option<&str> {
        self.games
            .get(game_code)
            .map(|meta| meta.host_build_commit.as_str())
    }

    pub fn unregister_game(&mut self, game_code: &str) {
        self.games.remove(game_code);
        debug!(game = %game_code, "lobby game unregistered");
    }

    pub fn verify_password(&self, game_code: &str, password: Option<&str>) -> Result<(), String> {
        let meta = self
            .games
            .get(game_code)
            .ok_or_else(|| format!("Game not found in lobby: {}", game_code))?;

        match (&meta.password, password) {
            (None, _) => Ok(()),
            (Some(_), None) => Err("password_required".to_string()),
            (Some(expected), Some(provided)) => {
                if expected == provided {
                    Ok(())
                } else {
                    warn!(game = %game_code, "wrong password");
                    Err("Wrong password".to_string())
                }
            }
        }
    }

    /// Returns the public-lobby view of a single game by code, or `None` if
    /// the game isn't tracked or isn't public.
    pub fn public_game(&self, game_code: &str) -> Option<LobbyGame> {
        let meta = self.games.get(game_code)?;
        if !meta.public {
            return None;
        }
        Some(Self::meta_to_lobby_game(game_code, meta))
    }

    pub fn public_games(&self) -> Vec<LobbyGame> {
        self.games
            .iter()
            .filter(|(_, meta)| meta.public)
            .map(|(code, meta)| Self::meta_to_lobby_game(code, meta))
            .collect()
    }

    /// Converts internal `LobbyGameMeta` to the wire-level `LobbyGame`. Single
    /// construction site prevents field drift when new metadata fields are added.
    fn meta_to_lobby_game(game_code: &str, meta: &LobbyGameMeta) -> LobbyGame {
        LobbyGame {
            game_code: game_code.to_string(),
            host_name: meta.host_name.clone(),
            created_at: meta.created_at,
            has_password: meta.has_password,
            host_version: meta.host_version.clone(),
            host_build_commit: meta.host_build_commit.clone(),
            current_players: (meta.current_players + meta.reservations.len() as u32)
                .min(meta.max_players),
            max_players: meta.max_players,
            format: meta.format_config.as_ref().map(|fc| fc.format),
            room_name: meta.room_name.clone(),
            is_p2p: !meta.host_peer_id.is_empty(),
            is_sandbox: meta
                .format_config
                .as_ref()
                .is_some_and(|fc| fc.allow_debug_actions),
            draft_metadata: meta.draft_metadata.clone(),
        }
    }

    pub fn has_game(&self, game_code: &str) -> bool {
        self.games.contains_key(game_code)
    }

    /// Current number of registered lobby entries. Used by the broker path to
    /// enforce a capacity cap (`LobbyManager` itself is unbounded).
    pub fn len(&self) -> usize {
        self.games.len()
    }

    /// Reports whether the lobby has any registered entries.
    pub fn is_empty(&self) -> bool {
        self.games.is_empty()
    }

    /// Atomic lookup of the fields a typed-code join needs to route correctly.
    /// Returns `None` if the game isn't registered.
    pub fn join_target_info(&self, game_code: &str) -> Option<JoinTargetInfo> {
        let meta = self.games.get(game_code)?;
        let is_p2p = !meta.host_peer_id.is_empty();
        Some(JoinTargetInfo {
            host_peer_id: meta.host_peer_id.clone(),
            max_players: meta.max_players,
            current_players: (meta.current_players + meta.reservations.len() as u32)
                .min(meta.max_players),
            format_config: meta.format_config.clone(),
            match_config: meta.match_config,
            is_p2p,
            reservation_token: None,
            reservation_expires_at_ms: None,
        })
    }

    pub fn timer_seconds(&self, game_code: &str) -> Option<u32> {
        self.games
            .get(game_code)
            .and_then(|meta| meta.timer_seconds)
    }

    /// Returns and removes games older than `timeout_secs`.
    pub fn check_expired(&mut self, timeout_secs: u64, env: &impl BrokerEnv) -> Vec<String> {
        let now = env.now_ms() / 1000;

        let mut expired = Vec::new();
        self.games.retain(|code, meta| {
            if now.saturating_sub(meta.created_at) > timeout_secs {
                expired.push(code.clone());
                false
            } else {
                true
            }
        });
        expired
    }
}

impl Default for LobbyManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::format::{FormatConfig, GameFormat};
    use engine::types::match_config::MatchConfig;
    use std::cell::Cell;

    /// Deterministic `BrokerEnv` for tests. `now_ms` is settable; tokens and
    /// codes are monotonic counters so assertions are stable.
    struct FakeEnv {
        now: Cell<u64>,
        token_counter: Cell<u64>,
        code_counter: Cell<u64>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                now: Cell::new(1_000_000),
                token_counter: Cell::new(0),
                code_counter: Cell::new(0),
            }
        }
        fn set_now_ms(&self, ms: u64) {
            self.now.set(ms);
        }
    }

    impl BrokerEnv for FakeEnv {
        fn now_ms(&self) -> u64 {
            self.now.get()
        }
        fn new_token(&self) -> String {
            let n = self.token_counter.get();
            self.token_counter.set(n + 1);
            format!("token-{n}")
        }
        fn new_game_code(&self) -> String {
            let n = self.code_counter.get();
            self.code_counter.set(n + 1);
            format!("CODE{n:02}")
        }
    }

    fn register_basic(
        lobby: &mut LobbyManager,
        code: &str,
        host: &str,
        public: bool,
        password: Option<String>,
        timer: Option<u32>,
        env: &impl BrokerEnv,
    ) {
        lobby.register_game(
            code,
            RegisterGameRequest {
                host_name: host.to_string(),
                public,
                password,
                timer_seconds: timer,
                ..Default::default()
            },
            env,
        );
    }

    #[test]
    fn register_and_list_public_games() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        register_basic(&mut lobby, "GAME02", "Bob", false, None, None, &env);
        register_basic(
            &mut lobby,
            "GAME03",
            "Carol",
            true,
            Some("pw".to_string()),
            Some(60),
            &env,
        );

        let public = lobby.public_games();
        assert_eq!(public.len(), 2);
        let codes: Vec<&str> = public.iter().map(|g| g.game_code.as_str()).collect();
        assert!(codes.contains(&"GAME01"));
        assert!(codes.contains(&"GAME03"));
    }

    #[test]
    fn public_game_derives_is_p2p_from_host_peer_id() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "FULL01",
            RegisterGameRequest {
                host_name: "FullHost".to_string(),
                public: true,
                ..Default::default()
            },
            &env,
        );
        lobby.register_game(
            "P2P01",
            RegisterGameRequest {
                host_name: "BrokerHost".to_string(),
                public: true,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );

        let full = lobby.public_game("FULL01").expect("full entry listed");
        let p2p = lobby.public_game("P2P01").expect("p2p entry listed");
        assert!(!full.is_p2p);
        assert!(p2p.is_p2p);

        let all = lobby.public_games();
        let full = all.iter().find(|g| g.game_code == "FULL01").unwrap();
        let p2p = all.iter().find(|g| g.game_code == "P2P01").unwrap();
        assert!(!full.is_p2p);
        assert!(p2p.is_p2p);
    }

    #[test]
    fn unregister_removes_game() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        assert_eq!(lobby.public_games().len(), 1);

        lobby.unregister_game("GAME01");
        assert_eq!(lobby.public_games().len(), 0);
    }

    #[test]
    fn verify_password_no_password_required() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);

        assert!(lobby.verify_password("GAME01", None).is_ok());
        assert!(lobby.verify_password("GAME01", Some("anything")).is_ok());
    }

    #[test]
    fn verify_password_correct() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
            &env,
        );

        assert!(lobby.verify_password("GAME01", Some("secret")).is_ok());
    }

    #[test]
    fn verify_password_wrong() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
            &env,
        );

        let result = lobby.verify_password("GAME01", Some("wrong"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Wrong password");
    }

    #[test]
    fn verify_password_required_but_missing() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
            &env,
        );

        let result = lobby.verify_password("GAME01", None);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "password_required");
    }

    #[test]
    fn verify_password_game_not_found() {
        let lobby = LobbyManager::new();
        let result = lobby.verify_password("NOPE", None);
        assert!(result.is_err());
    }

    #[test]
    fn timer_seconds_returns_configured_value() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, Some(90), &env);
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None, &env);

        assert_eq!(lobby.timer_seconds("GAME01"), Some(90));
        assert_eq!(lobby.timer_seconds("GAME02"), None);
        assert_eq!(lobby.timer_seconds("NOPE"), None);
    }

    #[test]
    fn check_expired_removes_old_games() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);

        // Manually set created_at to the past.
        lobby.games.get_mut("GAME01").unwrap().created_at = 0;

        let expired = lobby.check_expired(300, &env);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], "GAME01");
        assert!(lobby.public_games().is_empty());
    }

    #[test]
    fn check_expired_retains_fresh_games() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);

        let expired = lobby.check_expired(300, &env);
        assert!(expired.is_empty());
        assert_eq!(lobby.public_games().len(), 1);
    }

    #[test]
    fn lobby_game_has_password_flag() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("pw".to_string()),
            None,
            &env,
        );
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None, &env);

        let games = lobby.public_games();
        let g1 = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        let g2 = games.iter().find(|g| g.game_code == "GAME02").unwrap();
        assert!(g1.has_password);
        assert!(!g2.has_password);
    }

    #[test]
    fn host_build_commit_returned_from_register() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                host_version: "0.1.11".to_string(),
                host_build_commit: "abc1234".to_string(),
                ..Default::default()
            },
            &env,
        );
        assert_eq!(lobby.host_build_commit("GAME01"), Some("abc1234"));
        assert_eq!(lobby.host_build_commit("NOPE"), None);

        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.host_version, "0.1.11");
        assert_eq!(g.host_build_commit, "abc1234");
    }

    #[test]
    fn extended_fields_roundtrip_through_public_games() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 2,
                max_players: 4,
                format_config: Some(FormatConfig::commander()),
                ..Default::default()
            },
            &env,
        );
        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.current_players, 2);
        assert_eq!(g.max_players, 4);
        assert_eq!(g.format, Some(GameFormat::Commander));
    }

    #[test]
    fn set_current_players_updates_existing_entry() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 1,
                max_players: 4,
                ..Default::default()
            },
            &env,
        );

        lobby.set_current_players("GAME01", 3, &env);
        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.current_players, 3);
    }

    #[test]
    fn public_game_returns_entry_when_public() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 2,
                max_players: 4,
                format_config: Some(FormatConfig::commander()),
                ..Default::default()
            },
            &env,
        );

        let game = lobby.public_game("GAME01").expect("entry should exist");
        assert_eq!(game.game_code, "GAME01");
        assert_eq!(game.current_players, 2);
        assert_eq!(game.format, Some(GameFormat::Commander));
    }

    #[test]
    fn public_game_returns_none_for_private_entry() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", false, None, None, &env);
        assert!(lobby.public_game("GAME01").is_none());
    }

    #[test]
    fn public_game_returns_none_for_missing_entry() {
        let lobby = LobbyManager::new();
        assert!(lobby.public_game("NOPE").is_none());
    }

    #[test]
    fn join_target_info_returns_atomic_snapshot() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                host_peer_id: "peer-xyz".to_string(),
                current_players: 1,
                max_players: 4,
                format_config: Some(FormatConfig::commander()),
                ..Default::default()
            },
            &env,
        );
        assert_eq!(
            lobby.join_target_info("GAME01"),
            Some(JoinTargetInfo {
                host_peer_id: "peer-xyz".to_string(),
                max_players: 4,
                current_players: 1,
                format_config: Some(FormatConfig::commander()),
                match_config: MatchConfig::default(),
                is_p2p: true,
                reservation_token: None,
                reservation_expires_at_ms: None,
            })
        );
    }

    #[test]
    fn join_target_info_returns_none_for_missing_game() {
        let lobby = LobbyManager::new();
        assert_eq!(lobby.join_target_info("NOPE"), None);
    }

    #[test]
    fn join_target_info_marks_full_mode_entries_as_non_p2p() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                format_config: Some(FormatConfig::standard()),
                ..Default::default()
            },
            &env,
        );
        assert_eq!(
            lobby.join_target_info("GAME01"),
            Some(JoinTargetInfo {
                host_peer_id: String::new(),
                max_players: 0,
                current_players: 0,
                format_config: Some(FormatConfig::standard()),
                match_config: MatchConfig::default(),
                is_p2p: false,
                reservation_token: None,
                reservation_expires_at_ms: None,
            })
        );
        assert!(lobby.has_game("GAME01"));
    }

    #[test]
    fn len_and_is_empty_reflect_registration() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        assert!(lobby.is_empty());
        assert_eq!(lobby.len(), 0);
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None, &env);
        assert!(!lobby.is_empty());
        assert_eq!(lobby.len(), 1);
        lobby.unregister_game("GAME01");
        assert!(lobby.is_empty());
    }

    #[test]
    fn set_current_players_no_op_on_missing_game() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.set_current_players("NOPE", 5, &env);
        assert!(lobby.public_games().is_empty());
    }

    #[test]
    fn public_seat_reservation_expires_at_uses_env_clock() {
        let env = FakeEnv::new();
        env.set_now_ms(5_000);
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                max_players: 4,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );
        let res = lobby
            .reserve_seat("GAME01", "Bob".to_string(), &env)
            .expect("seat reserved");
        assert_eq!(res.expires_at_ms, Some(5_000 + PUBLIC_SEAT_RESERVATION_MS));
    }

    #[test]
    fn password_protected_reservation_never_expires() {
        let env = FakeEnv::new();
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                password: Some("pw".to_string()),
                max_players: 4,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );
        let res = lobby
            .reserve_seat("GAME01", "Bob".to_string(), &env)
            .expect("seat reserved");
        assert_eq!(res.expires_at_ms, None);
    }

    #[test]
    fn expired_reservation_is_cleaned_up() {
        let env = FakeEnv::new();
        env.set_now_ms(1_000);
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                max_players: 4,
                host_peer_id: "peer-xyz".to_string(),
                ..Default::default()
            },
            &env,
        );
        lobby
            .reserve_seat("GAME01", "Bob".to_string(), &env)
            .expect("seat reserved");
        // Advance past the reservation lifetime.
        env.set_now_ms(1_000 + PUBLIC_SEAT_RESERVATION_MS + 1);
        assert!(lobby.cleanup_expired_reservations("GAME01", &env));
        // Public listing no longer counts the lapsed reservation.
        let g = lobby.public_game("GAME01").unwrap();
        assert_eq!(g.current_players, 0);
    }
}
