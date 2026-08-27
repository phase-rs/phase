use serde::{Deserialize, Serialize};

use super::actions::GameAction;
use super::format::FormatConfig;
use super::match_config::MatchConfig;
use super::player::PlayerId;
use crate::game::deck_loading::DeckList;

/// Version 3 adds atomic Resolve All replay boundaries. Version 2 recordings
/// remain readable because they cannot contain those boundaries.
pub const REPLAY_FORMAT_VERSION: u32 = 3;

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

/// One engine-owned Resolve All burst, anchored after the preceding submitted
/// action. Resolve All has no `GameAction` because its Ready latch is consumed
/// by the transport; replay therefore carries the whole burst as one atomic
/// boundary rather than manufacturing individual priority passes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedResolveAll {
    /// Number of ordinary actions already applied when this burst began.
    pub after_action_count: u32,
    pub requester: PlayerId,
}

/// A complete, deterministic recording of a game: the inputs needed to
/// reconstruct its starting state, plus every action that was submitted and
/// accepted afterward. Replaying `actions` against the state produced by
/// `header` reproduces the original game turn-for-turn — see
/// `crate::game::replay`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReplayLog {
    /// Replay schema version. `None` represents a legacy recording that
    /// predates explicit versioning and is rejected before reconstruction.
    #[serde(default)]
    pub format_version: Option<u32>,
    pub header: ReplayHeader,
    pub actions: Vec<RecordedAction>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resolve_all_boundaries: Vec<RecordedResolveAll>,
}

impl ReplayLog {
    pub fn new(header: ReplayHeader) -> Self {
        Self {
            format_version: Some(REPLAY_FORMAT_VERSION),
            header,
            actions: Vec::new(),
            resolve_all_boundaries: Vec::new(),
        }
    }

    /// Append a successfully-applied action to the recording. Callers must
    /// only record actions after `apply` returned `Ok` — a rejected action
    /// never touched game state and replaying it would desync reconstruction.
    pub fn push_action(&mut self, actor: PlayerId, action: GameAction) {
        let seq = self.actions.len() as u32;
        self.actions.push(RecordedAction { seq, actor, action });
    }

    /// Records the transport-owned consumption of an already-ready Resolve All
    /// latch. Playback runs the same engine consumer after this many ordinary
    /// actions, preserving automatic follow-up without replaying its internal
    /// passes as independent user actions.
    pub fn push_resolve_all_boundary(&mut self, requester: PlayerId) {
        self.resolve_all_boundaries.push(RecordedResolveAll {
            after_action_count: self.actions.len() as u32,
            requester,
        });
    }
}
