use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::LobbyGame;

struct LobbyGameMeta {
    host_name: String,
    created_at: u64,
    password: Option<String>,
    has_password: bool,
    timer_seconds: Option<u32>,
    public: bool,
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

    pub fn register_game(
        &mut self,
        game_code: &str,
        host_name: String,
        public: bool,
        password: Option<String>,
        timer_seconds: Option<u32>,
    ) {
        let has_password = password.is_some();
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        self.games.insert(
            game_code.to_string(),
            LobbyGameMeta {
                host_name,
                created_at,
                password,
                has_password,
                timer_seconds,
                public,
            },
        );
    }

    pub fn unregister_game(&mut self, game_code: &str) {
        self.games.remove(game_code);
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
                    Err("Wrong password".to_string())
                }
            }
        }
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
            })
            .collect()
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

    #[test]
    fn register_and_list_public_games() {
        let mut lobby = LobbyManager::new();
        lobby.register_game("GAME01", "Alice".to_string(), true, None, None);
        lobby.register_game("GAME02", "Bob".to_string(), false, None, None);
        lobby.register_game(
            "GAME03",
            "Carol".to_string(),
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
        lobby.register_game("GAME01", "Alice".to_string(), true, None, None);
        assert_eq!(lobby.public_games().len(), 1);

        lobby.unregister_game("GAME01");
        assert_eq!(lobby.public_games().len(), 0);
    }

    #[test]
    fn verify_password_no_password_required() {
        let mut lobby = LobbyManager::new();
        lobby.register_game("GAME01", "Alice".to_string(), true, None, None);

        assert!(lobby.verify_password("GAME01", None).is_ok());
        assert!(lobby.verify_password("GAME01", Some("anything")).is_ok());
    }

    #[test]
    fn verify_password_correct() {
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            "Alice".to_string(),
            true,
            Some("secret".to_string()),
            None,
        );

        assert!(lobby.verify_password("GAME01", Some("secret")).is_ok());
    }

    #[test]
    fn verify_password_wrong() {
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            "Alice".to_string(),
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
        lobby.register_game(
            "GAME01",
            "Alice".to_string(),
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
        lobby.register_game("GAME01", "Alice".to_string(), true, None, Some(90));
        lobby.register_game("GAME02", "Bob".to_string(), true, None, None);

        assert_eq!(lobby.timer_seconds("GAME01"), Some(90));
        assert_eq!(lobby.timer_seconds("GAME02"), None);
        assert_eq!(lobby.timer_seconds("NOPE"), None);
    }

    #[test]
    fn check_expired_removes_old_games() {
        let mut lobby = LobbyManager::new();
        lobby.register_game("GAME01", "Alice".to_string(), true, None, None);

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
        lobby.register_game("GAME01", "Alice".to_string(), true, None, None);

        let expired = lobby.check_expired(300);
        assert!(expired.is_empty());
        assert_eq!(lobby.public_games().len(), 1);
    }

    #[test]
    fn lobby_game_has_password_flag() {
        let mut lobby = LobbyManager::new();
        lobby.register_game(
            "GAME01",
            "Alice".to_string(),
            true,
            Some("pw".to_string()),
            None,
        );
        lobby.register_game("GAME02", "Bob".to_string(), true, None, None);

        let games = lobby.public_games();
        let g1 = games.iter().find(|g| g.game_code == "GAME01").unwrap();
        let g2 = games.iter().find(|g| g.game_code == "GAME02").unwrap();
        assert!(g1.has_password);
        assert!(!g2.has_password);
    }
}
