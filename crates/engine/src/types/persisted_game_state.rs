use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::types::game_state::{GameState, PendingCastSacrificeRollback};
use crate::types::identifiers::ObjectId;

/// Authoritative game-state snapshot for browser/P2P persistence.
///
/// Mirrors `server_core::PersistedSession`'s sidecar pattern: `GameState`
/// serde skips `pending_cast_sacrifice_rollbacks` so hidden permanent identity
/// cannot leak through filtered client views (CR 400.2), while the wrapper
/// carries rollback snapshots for authoritative resume.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedGameState {
    pub state: GameState,
    /// CR 601.2i + CR 733.1: sacrifice rollback snapshots for in-flight casts.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub pending_cast_sacrifice_rollbacks: HashMap<ObjectId, PendingCastSacrificeRollback>,
}

impl PersistedGameState {
    pub fn capture(state: &GameState) -> Self {
        Self {
            state: state.clone(),
            pending_cast_sacrifice_rollbacks: state.pending_cast_sacrifice_rollbacks.clone(),
        }
    }

    pub fn into_game_state(self) -> GameState {
        let mut game_state = self.state;
        if !self.pending_cast_sacrifice_rollbacks.is_empty() {
            game_state.pending_cast_sacrifice_rollbacks = self.pending_cast_sacrifice_rollbacks;
        }
        game_state
    }
}

/// Deserialize either a `PersistedGameState` wrapper or a legacy bare `GameState`.
pub fn deserialize_resumable_game_state(json: &str) -> Result<GameState, serde_json::Error> {
    if let Ok(wrapper) = serde_json::from_str::<PersistedGameState>(json) {
        return Ok(wrapper.into_game_state());
    }
    serde_json::from_str(json)
}
