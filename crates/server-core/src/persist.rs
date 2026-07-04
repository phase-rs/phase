use std::collections::HashMap;

use engine::types::game_state::GameState;
use phase_ai::config::AiDifficulty;
use serde::{Deserialize, Serialize};

use draft_core::types::{DraftConfig, DraftSession as DraftCoreSession};

use crate::lobby::RegisterGameRequest;
use crate::protocol::DraftLobbyMetadata;

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
    /// Whether the room should auto-start when every configured seat is occupied.
    #[serde(default = "default_true")]
    pub start_when_full: bool,
    #[serde(default)]
    pub ranked: bool,
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
    #[serde(default = "default_true")]
    pub start_when_full: bool,
    #[serde(default)]
    pub ranked: bool,
}

fn default_true() -> bool {
    true
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

impl PersistedDraftSession {
    /// Lobby registration is only valid while the draft is still in the pre-start lobby.
    pub fn should_register_in_lobby(&self) -> bool {
        self.lobby_meta.is_some() && self.session.status == draft_core::types::DraftStatus::Lobby
    }
}

/// Build the lobby-broker registration payload for a restored draft, if and
/// only if the persisted snapshot is still joinable. This is the single
/// production seam used by startup restore in `phase-server` — callers must
/// not re-implement the status/meta gate inline.
pub fn restored_draft_lobby_register_request(
    ps: &PersistedDraftSession,
) -> Option<RegisterGameRequest> {
    if !ps.should_register_in_lobby() {
        return None;
    }
    let meta = ps.lobby_meta.as_ref()?;
    let filled = ps.player_tokens.iter().filter(|t| !t.is_empty()).count();
    Some(RegisterGameRequest {
        host_name: meta.host_name.clone(),
        public: meta.public,
        password: meta.password.clone(),
        timer_seconds: meta.timer_seconds,
        current_players: filled as u32,
        max_players: ps.config.pod_size as u32,
        draft_metadata: Some(DraftLobbyMetadata {
            set_code: ps.config.set_code.clone(),
            draft_kind: format!("{:?}", ps.config.kind),
            cube_name: None,
        }),
        ..Default::default()
    })
}
