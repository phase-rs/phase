use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use engine::types::format::GameFormat;
use tracing::{debug, warn};

use crate::protocol::LobbyGame;

/// Fields a caller supplies when registering a lobby entry. Using a struct
/// here rather than a long positional argument list means adding a new field
/// (e.g. when the lobby UI grows to display more info) doesn't require
/// touching every caller — just add it here with a `Default` and populate
/// where relevant.
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
    pub format: Option<GameFormat>,
    /// Optional match-scoped label shown in lobby listings. Distinct from
    /// `host_name` (the player identity). `None` means the lobby row falls
    /// back to the host's name.
    pub room_name: Option<String>,
}

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
    format: Option<GameFormat>,
    room_name: Option<String>,
}

pub struct LobbyManager {
    games: HashMap<String, LobbyGameMeta>,
}

impl LobbyManager {
    pub fn new() -> Self {
        Self {
            games: HashMap::new(),
        }
    }

    pub fn register_game(&mut self, game_code: &str, req: RegisterGameRequest) {
        let has_password = req.password.is_some();
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

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
                format: req.format,
                room_name: req.room_name,
            },
        );
    }

    /// Updates the `current_players` count for an existing lobby entry. Called
    /// when a guest joins or leaves a waiting room so the public lobby listing
    /// stays accurate. No-op if the game isn't tracked.
    pub fn set_current_players(&mut self, game_code: &str, current_players: u32) {
        if let Some(meta) = self.games.get_mut(game_code) {
            meta.current_players = current_players;
        }
    }

    /// Returns the host's build identity for a game, used to gate joins in
    /// `JoinGameWithPassword` when the guest's build differs from the host's.
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
    /// the game isn't tracked or isn't public. Callers use this after
    /// `set_current_players` to build a `LobbyGameUpdated` broadcast
    /// without cloning the full public list.
    pub fn public_game(&self, game_code: &str) -> Option<LobbyGame> {
        let meta = self.games.get(game_code)?;
        if !meta.public {
            return None;
        }
        Some(LobbyGame {
            game_code: game_code.to_string(),
            host_name: meta.host_name.clone(),
            created_at: meta.created_at,
            has_password: meta.has_password,
            host_version: meta.host_version.clone(),
            host_build_commit: meta.host_build_commit.clone(),
            current_players: meta.current_players,
            max_players: meta.max_players,
            format: meta.format,
            room_name: meta.room_name.clone(),
        })
    }

    pub fn public_games(&self) -> Vec<LobbyGame> {
        self.games
            .iter()
            .filter(|(_, meta)| meta.public)
            .map(|(code, meta)| LobbyGame {
                game_code: code.clone(),
                host_name: meta.host_name.clone(),
                created_at: meta.created_at,
                has_password: meta.has_password,
                host_version: meta.host_version.clone(),
                host_build_commit: meta.host_build_commit.clone(),
                current_players: meta.current_players,
                max_players: meta.max_players,
                format: meta.format,
                room_name: meta.room_name.clone(),
            })
            .collect()
    }

    pub fn has_game(&self, game_code: &str) -> bool {
        self.games.contains_key(game_code)
    }

    pub fn timer_seconds(&self, game_code: &str) -> Option<u32> {
        self.games
            .get(game_code)
            .and_then(|meta| meta.timer_seconds)
    }

    /// Returns and removes games older than `timeout_secs`.
    pub fn check_expired(&mut self, timeout_secs: u64) -> Vec<String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

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

    /// Test helper: registers a game with default metadata so existing tests
    /// don't have to care about the extended set of fields. Tests that
    /// exercise specific metadata call `register_game` with a fully-populated
    /// `RegisterGameRequest` directly.
    fn register_basic(
        lobby: &mut LobbyManager,
        code: &str,
        host: &str,
        public: bool,
        password: Option<String>,
        timer: Option<u32>,
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
        );
    }

    #[test]
    fn register_and_list_public_games() {
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None);
        register_basic(&mut lobby, "GAME02", "Bob", false, None, None);
        register_basic(
            &mut lobby,
            "GAME03",
            "Carol",
            true,
            Some("pw".to_string()),
            Some(60),
        );

        let public = lobby.public_games();
        assert_eq!(public.len(), 2);
        let codes: Vec<&str> = public.iter().map(|g| g.game_code.as_str()).collect();
        assert!(codes.contains(&"GAME01"));
        assert!(codes.contains(&"GAME03"));
    }

    #[test]
    fn unregister_removes_game() {
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None);
        assert_eq!(lobby.public_games().len(), 1);

        lobby.unregister_game("GAME01");
        assert_eq!(lobby.public_games().len(), 0);
    }

    #[test]
    fn verify_password_no_password_required() {
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None);

        assert!(lobby.verify_password("GAME01", None).is_ok());
        assert!(lobby.verify_password("GAME01", Some("anything")).is_ok());
    }

    #[test]
    fn verify_password_correct() {
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
        );

        assert!(lobby.verify_password("GAME01", Some("secret")).is_ok());
    }

    #[test]
    fn verify_password_wrong() {
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
        );

        let result = lobby.verify_password("GAME01", Some("wrong"));
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "Wrong password");
    }

    #[test]
    fn verify_password_required_but_missing() {
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("secret".to_string()),
            None,
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
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, Some(90));
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None);

        assert_eq!(lobby.timer_seconds("GAME01"), Some(90));
        assert_eq!(lobby.timer_seconds("GAME02"), None);
        assert_eq!(lobby.timer_seconds("NOPE"), None);
    }

    #[test]
    fn check_expired_removes_old_games() {
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None);

        // Manually set created_at to the past
        lobby.games.get_mut("GAME01").unwrap().created_at = 0;

        let expired = lobby.check_expired(300);
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0], "GAME01");
        assert!(lobby.public_games().is_empty());
    }

    #[test]
    fn check_expired_retains_fresh_games() {
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", true, None, None);

        let expired = lobby.check_expired(300);
        assert!(expired.is_empty());
        assert_eq!(lobby.public_games().len(), 1);
    }

    #[test]
    fn lobby_game_has_password_flag() {
        let mut lobby = LobbyManager::new();
        register_basic(
            &mut lobby,
            "GAME01",
            "Alice",
            true,
            Some("pw".to_string()),
            None,
        );
        register_basic(&mut lobby, "GAME02", "Bob", true, None, None);

        let games = lobby.public_games();
        let g1 = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        let g2 = games.iter().find(|g| g.game_code == "GAME02").unwrap();
        assert!(g1.has_password);
        assert!(!g2.has_password);
    }

    #[test]
    fn host_build_commit_returned_from_register() {
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
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 2,
                max_players: 4,
                format: Some(GameFormat::Commander),
                ..Default::default()
            },
        );
        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.current_players, 2);
        assert_eq!(g.max_players, 4);
        assert_eq!(g.format, Some(GameFormat::Commander));
    }

    #[test]
    fn set_current_players_updates_existing_entry() {
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
        );

        lobby.set_current_players("GAME01", 3);
        let games = lobby.public_games();
        let g = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        assert_eq!(g.current_players, 3);
    }

    #[test]
    fn public_game_returns_entry_when_public() {
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            RegisterGameRequest {
                host_name: "Alice".to_string(),
                public: true,
                current_players: 2,
                max_players: 4,
                format: Some(GameFormat::Commander),
                ..Default::default()
            },
        );

        let game = lobby.public_game("GAME01").expect("entry should exist");
        assert_eq!(game.game_code, "GAME01");
        assert_eq!(game.current_players, 2);
        assert_eq!(game.format, Some(GameFormat::Commander));
    }

    #[test]
    fn public_game_returns_none_for_private_entry() {
        let mut lobby = LobbyManager::new();
        register_basic(&mut lobby, "GAME01", "Alice", false, None, None);
        assert!(lobby.public_game("GAME01").is_none());
    }

    #[test]
    fn public_game_returns_none_for_missing_entry() {
        let lobby = LobbyManager::new();
        assert!(lobby.public_game("NOPE").is_none());
    }

    #[test]
    fn set_current_players_no_op_on_missing_game() {
        let mut lobby = LobbyManager::new();
        // Must not panic or mutate anything.
        lobby.set_current_players("NOPE", 5);
        assert!(lobby.public_games().is_empty());
    }
}
