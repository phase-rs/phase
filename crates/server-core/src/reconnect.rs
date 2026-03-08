use std::collections::HashMap;
use std::time::{Duration, Instant};

use engine::types::player::PlayerId;

pub struct DisconnectInfo {
    pub player_id: PlayerId,
    pub disconnect_time: Instant,
    pub game_code: String,
}

pub struct ReconnectManager {
    pub grace_period: Duration,
    disconnected: HashMap<String, DisconnectInfo>,
}

#[derive(Debug)]
pub enum ReconnectResult {
    Ok { game_code: String },
    Expired,
    NotFound,
}

impl ReconnectManager {
    pub fn new(grace_period: Duration) -> Self {
        Self {
            grace_period,
            disconnected: HashMap::new(),
        }
    }

    /// Record a player disconnect for potential reconnection.
    pub fn record_disconnect(&mut self, game_code: &str, player: PlayerId) {
        let key = format!("{}:{}", game_code, player.0);
        self.disconnected.insert(
            key,
            DisconnectInfo {
                player_id: player,
                disconnect_time: Instant::now(),
                game_code: game_code.to_string(),
            },
        );
    }

    /// Attempt to reconnect a player using their token.
    /// The player_token is used to look up the game_code externally;
    /// here we check if the disconnect is within the grace period.
    pub fn attempt_reconnect(&mut self, game_code: &str, player: PlayerId) -> ReconnectResult {
        let key = format!("{}:{}", game_code, player.0);
        match self.disconnected.remove(&key) {
            Some(info) => {
                if info.disconnect_time.elapsed() <= self.grace_period {
                    ReconnectResult::Ok {
                        game_code: info.game_code,
                    }
                } else {
                    ReconnectResult::Expired
                }
            }
            None => ReconnectResult::NotFound,
        }
    }

    /// Return game codes with expired grace periods (for forfeit processing).
    pub fn check_expired(&mut self) -> Vec<String> {
        let mut expired = Vec::new();
        self.disconnected.retain(|_key, info| {
            if info.disconnect_time.elapsed() > self.grace_period {
                expired.push(info.game_code.clone());
                false
            } else {
                true
            }
        });
        expired
    }

    pub fn is_disconnected(&self, game_code: &str, player: PlayerId) -> bool {
        let key = format!("{}:{}", game_code, player.0);
        self.disconnected.contains_key(&key)
    }

    pub fn remove_disconnect(&mut self, game_code: &str, player: PlayerId) {
        let key = format!("{}:{}", game_code, player.0);
        self.disconnected.remove(&key);
    }
}

impl Default for ReconnectManager {
    fn default() -> Self {
        Self::new(Duration::from_secs(120))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_within_grace_period_succeeds() {
        let mut mgr = ReconnectManager::new(Duration::from_secs(120));
        mgr.record_disconnect("GAME01", PlayerId(0));

        // Immediately reconnect (within grace period)
        let result = mgr.attempt_reconnect("GAME01", PlayerId(0));
        match result {
            ReconnectResult::Ok { game_code } => assert_eq!(game_code, "GAME01"),
            _ => panic!("expected Ok, got {:?}", result),
        }
    }

    #[test]
    fn reconnect_after_expiry_fails() {
        let mut mgr = ReconnectManager::new(Duration::from_millis(0));
        mgr.record_disconnect("GAME01", PlayerId(0));

        // Grace period is 0ms, so it's already expired
        std::thread::sleep(Duration::from_millis(1));
        let result = mgr.attempt_reconnect("GAME01", PlayerId(0));
        match result {
            ReconnectResult::Expired => {}
            _ => panic!("expected Expired, got {:?}", result),
        }
    }

    #[test]
    fn reconnect_unknown_game_returns_not_found() {
        let mut mgr = ReconnectManager::default();
        let result = mgr.attempt_reconnect("NOPE", PlayerId(0));
        match result {
            ReconnectResult::NotFound => {}
            _ => panic!("expected NotFound"),
        }
    }

    #[test]
    fn check_expired_returns_expired_games() {
        let mut mgr = ReconnectManager::new(Duration::from_millis(0));
        mgr.record_disconnect("GAME01", PlayerId(0));
        mgr.record_disconnect("GAME02", PlayerId(1));
        std::thread::sleep(Duration::from_millis(1));

        let expired = mgr.check_expired();
        assert_eq!(expired.len(), 2);
        assert!(expired.contains(&"GAME01".to_string()));
        assert!(expired.contains(&"GAME02".to_string()));
    }

    #[test]
    fn check_expired_retains_non_expired() {
        let mut mgr = ReconnectManager::new(Duration::from_secs(120));
        mgr.record_disconnect("GAME01", PlayerId(0));

        let expired = mgr.check_expired();
        assert!(expired.is_empty());
        assert!(mgr.is_disconnected("GAME01", PlayerId(0)));
    }
}
