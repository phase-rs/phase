use engine::types::format::FormatConfig;
use phase_ai::config::AiDifficulty;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum SeatKind {
    HostHuman,
    JoinedHuman,
    WaitingHuman,
    Ai {
        difficulty: AiDifficulty,
        deck: DeckChoice,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "data")]
pub enum DeckChoice {
    Random,
    Named(String),
    DeckList(Box<engine::starter_decks::DeckData>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum SeatMutation {
    #[serde(rename_all = "camelCase")]
    SetKind {
        seat_index: u8,
        kind: SeatKind,
    },
    #[serde(rename_all = "camelCase")]
    Remove {
        seat_index: u8,
    },
    Start,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatState {
    pub seats: Vec<SeatKind>,
    pub tokens: Vec<String>,
    pub format: FormatConfig,
    /// True once the game has started. Mutations are rejected after this point.
    /// Binary internal predicate — documented exception to the no-bool-flags rule.
    pub game_started: bool,
}

impl SeatState {
    /// Every seat is either a joined human or an AI — no waiting seats remain.
    pub fn is_full(&self) -> bool {
        self.seats.iter().all(|s| {
            matches!(
                s,
                SeatKind::HostHuman | SeatKind::JoinedHuman | SeatKind::Ai { .. }
            )
        })
    }

    pub fn is_pregame(&self) -> bool {
        !self.game_started
    }

    pub fn to_view(&self) -> SeatView {
        SeatView {
            seats: self.seats.clone(),
            format: self.format.clone(),
            is_full: self.is_full(),
            game_started: self.game_started,
        }
    }
}

/// Token-free projection of seat state, safe to broadcast to P2P guests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatView {
    pub seats: Vec<SeatKind>,
    pub format: FormatConfig,
    pub is_full: bool,
    /// Binary internal predicate — documented exception to the no-bool-flags rule.
    pub game_started: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeatDelta {
    pub mutated_seats: Vec<u8>,
    pub invalidated_tokens: Vec<String>,
    /// Seat indices of removed AI seats.
    pub removed_ai: Vec<u8>,
    /// (seat_index, difficulty, resolved_deck) for newly added AI seats.
    pub new_ai: Vec<(
        u8,
        AiDifficulty,
        engine::game::deck_loading::PlayerDeckPayload,
    )>,
    pub renumbering: Option<Renumbering>,
    /// True only for Start mutations — signals the caller to begin the game.
    pub now_started: bool,
}

impl SeatDelta {
    pub fn empty() -> Self {
        Self {
            mutated_seats: vec![],
            invalidated_tokens: vec![],
            removed_ai: vec![],
            new_ai: vec![],
            renumbering: None,
            now_started: false,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Renumbering {
    pub removed_index: u8,
    /// (old_index, new_index) for each seat that shifted down.
    pub remapping: Vec<(u8, u8)>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SeatError {
    GameStarted,
    /// Seat 0 (host) or out-of-range index.
    SeatImmutable,
    /// Disallowed transition (e.g., WaitingHuman → JoinedHuman).
    InvalidTransition,
    /// Would drop below the format's minimum player count.
    BelowFormatMin,
    /// Remove on a JoinedHuman seat (must kick or direct-replace instead).
    SeatClaimed,
    /// Start attempted when not all seats are filled.
    NotFull,
    DeckResolutionFailed(String),
}

use engine::game::deck_loading::PlayerDeckPayload;
use phase_ai::config::Platform;

pub trait DeckResolver {
    fn resolve(&self, choice: &DeckChoice) -> Result<PlayerDeckPayload, String>;
}

pub struct ReducerCtx<'a> {
    pub platform: Platform,
    pub deck_resolver: &'a dyn DeckResolver,
}
