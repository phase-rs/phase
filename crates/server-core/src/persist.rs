use std::collections::HashMap;

use engine::types::game_state::GameState;
use phase_ai::config::AiDifficulty;
use serde::{Deserialize, Serialize};

use draft_core::types::{DraftConfig, DraftSession as DraftCoreSession};

/// Serializable snapshot of a game session for disk persistence.
///
/// Fields that can be reconstructed at restore time are excluded:
/// - `connected` — all players are disconnected on restore
/// - `ai_configs` — reconstructed from `ai_difficulties` + `player_count`
/// - `decks` — consumed at game start, data lives in `state` after that
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    pub game_code: String,
    pub state: GameState,
    pub player_tokens: Vec<String>,
    pub display_names: Vec<String>,
    pub timer_seconds: Option<u32>,
    pub player_count: u8,
    /// Seat indices occupied by AI (PlayerId is a u8 newtype).
    pub ai_seats: Vec<u8>,
    /// AI difficulty per seat, keyed by seat index.
    pub ai_difficulties: HashMap<u8, AiDifficulty>,
    /// Whether the game has been started (all seats filled, engine initialized).
    pub game_started: bool,
    /// Lobby metadata for games still waiting for players.
    pub lobby_meta: Option<PersistedLobbyMeta>,
}

/// Lobby metadata persisted alongside a waiting game.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedLobbyMeta {
    pub host_name: String,
    pub public: bool,
    pub password: Option<String>,
    pub timer_seconds: Option<u32>,
}

/// Serializable snapshot of a draft session for disk persistence.
///
/// Fields excluded (reconstructed at restore time):
/// - `connected` — all players are disconnected on restore
/// - `timer_task` — JoinHandle is not serializable; re-arm from `timer_remaining_ms`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedDraftSession {
    pub draft_code: String,
    pub session: DraftCoreSession,
    pub player_tokens: Vec<String>,
    pub display_names: Vec<String>,
    pub config: DraftConfig,
    pub active_matches: HashMap<String, String>,
    pub lobby_meta: Option<PersistedLobbyMeta>,
    pub timer_remaining_ms: Option<u32>,
}
