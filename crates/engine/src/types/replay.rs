use serde::{Deserialize, Serialize};

use super::actions::GameAction;
use super::format::FormatConfig;
use super::match_config::MatchConfig;
use super::player::PlayerId;
use crate::game::deck_loading::DeckList;

/// Everything needed to reconstruct a game's starting state, deterministically,
/// from scratch. Mirrors the inputs `initialize_game` already accepts at the
/// WASM boundary — a replay's header is just those inputs captured at game
/// start instead of thrown away.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayHeader {
    pub format_config: FormatConfig,
    pub match_config: MatchConfig,
    pub player_count: u8,
    /// CR 103.1: `Some(0)` / `Some(1)` for an explicit starting player,
    /// `None` for the engine's own d20 contest.
    pub first_player: Option<u8>,
    pub seed: u64,
    /// `None` when the game was started with empty libraries (no deck data
    /// supplied), mirroring `initialize_game`'s `deck_data: null` path.
    pub deck_data: Option<DeckList>,
}

/// One submitted-and-accepted action, in submission order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedAction {
    /// Position of this action within the recording — equal to its index in
    /// `ReplayLog::actions`. Carried explicitly (rather than relying solely on
    /// vector position) so a recording can be sliced/filtered without losing
    /// the ability to report "this was action #N" to a human.
    pub seq: u32,
    pub actor: PlayerId,
    pub action: GameAction,
}

/// A complete, deterministic recording of a game: the inputs needed to
/// reconstruct its starting state, plus every action that was submitted and
/// accepted afterward. Replaying `actions` against the state produced by
/// `header` reproduces the original game turn-for-turn — see
/// `crate::game::replay`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayLog {
    pub header: ReplayHeader,
    pub actions: Vec<RecordedAction>,
}

impl ReplayLog {
    pub fn new(header: ReplayHeader) -> Self {
        Self {
            header,
            actions: Vec::new(),
        }
    }

    /// Append a successfully-applied action to the recording. Callers must
    /// only record actions after `apply` returned `Ok` — a rejected action
    /// never touched game state and replaying it would desync reconstruction.
    pub fn push_action(&mut self, actor: PlayerId, action: GameAction) {
        let seq = self.actions.len() as u32;
        self.actions.push(RecordedAction { seq, actor, action });
    }
}
