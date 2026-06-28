use engine::types::format::{FormatConfig, FormatTopology};
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SeatTeamInfo {
    pub team_index: u8,
    pub position_in_team: u8,
}

pub fn seat_team_info(format: &FormatConfig, seat_index: u8) -> Option<SeatTeamInfo> {
    match format.topology() {
        FormatTopology::IndividualSeats => None,
        FormatTopology::FixedTeams {
            team_size,
            team_count,
            ..
        } => {
            let seat_count = team_size * team_count;
            if seat_index >= seat_count {
                return None;
            }
            Some(SeatTeamInfo {
                team_index: seat_index / team_size,
                position_in_team: seat_index % team_size,
            })
        }
        FormatTopology::OneVsMany { archenemy, .. } => {
            if seat_index >= format.max_players {
                return None;
            }
            if seat_index == archenemy.0 {
                return Some(SeatTeamInfo {
                    team_index: 0,
                    position_in_team: 0,
                });
            }
            let hero_position = (0..seat_index).filter(|seat| *seat != archenemy.0).count() as u8;
            Some(SeatTeamInfo {
                team_index: 1,
                position_in_team: hero_position,
            })
        }
    }
}

pub fn seat_team_info_for_seats(
    format: &FormatConfig,
    seat_count: usize,
) -> Vec<Option<SeatTeamInfo>> {
    match format.topology() {
        FormatTopology::IndividualSeats => Vec::new(),
        FormatTopology::FixedTeams { .. } | FormatTopology::OneVsMany { .. } => (0..seat_count)
            .map(|seat_index| seat_team_info(format, seat_index as u8))
            .collect(),
    }
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
            team_info: seat_team_info_for_seats(&self.format, self.seats.len()),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub team_info: Vec<Option<SeatTeamInfo>>,
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
    /// (seat_index, difficulty, deck) for newly added AI seats. The deck is
    /// the name-only `PlayerDeckList` form — see `DeckResolver` for why.
    pub new_ai: Vec<(u8, AiDifficulty, engine::game::deck_loading::PlayerDeckList)>,
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

use engine::game::deck_loading::PlayerDeckList;
use phase_ai::config::Platform;

/// Resolves a `DeckChoice` (Random / Named / DeckList) into the name-only
/// `PlayerDeckList` shape every downstream consumer can use uniformly.
///
/// The reducer deliberately stays at the name-only layer: the WASM host path
/// calls `wasm.initialize_game(...)` which re-resolves names against its own
/// `CARD_DB`, and the server-core path resolves at `start_game` time when
/// the server-side `CardDatabase` is already in scope. Returning a
/// fully-resolved `PlayerDeckPayload` here forced both consumers to coerce
/// shapes at every boundary, hid the name-vs-resolved confusion behind
/// TypeScript `as` casts, and produced the silent empty-libraries bug at
/// `wasm.initialize_game` deserialization.
pub trait DeckResolver {
    fn resolve(&self, choice: &DeckChoice) -> Result<PlayerDeckList, String>;
}

pub struct ReducerCtx<'a> {
    pub platform: Platform,
    pub deck_resolver: &'a dyn DeckResolver,
}
