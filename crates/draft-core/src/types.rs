use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use engine::types::match_config::{MatchConfig, MatchType};
use engine::types::player::PlayerId;

use crate::validation::LimitedDeckError;

/// The kind of draft event, modeled after Arena's three draft modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftKind {
    /// Quick Draft: 1 human + 7 bots, Bo1 matches.
    Quick,
    /// Premier Draft: 8 humans, Bo1 matches.
    Premier,
    /// Traditional Draft: 8 humans, Bo3 matches.
    Traditional,
}

impl DraftKind {
    /// Pod size is always 8 for all draft modes.
    pub fn pod_size(self) -> u8 {
        8
    }

    /// Number of human seats. Quick Draft has 1 human + 7 bots.
    pub fn human_seats(self) -> u8 {
        match self {
            DraftKind::Quick => 1,
            DraftKind::Premier | DraftKind::Traditional => 8,
        }
    }

    /// Match configuration for this draft kind.
    pub fn match_config(self) -> MatchConfig {
        match self {
            DraftKind::Quick | DraftKind::Premier => MatchConfig {
                match_type: MatchType::Bo1,
            },
            DraftKind::Traditional => MatchConfig {
                match_type: MatchType::Bo3,
            },
        }
    }
}

/// Direction packs are passed around the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PassDirection {
    Left,
    Right,
}

impl PassDirection {
    /// Standard MTG draft pass direction: pack 1 left, pack 2 right, pack 3 left, etc.
    pub fn for_pack(pack_number: u8) -> Self {
        if pack_number.is_multiple_of(2) {
            PassDirection::Left
        } else {
            PassDirection::Right
        }
    }

    /// Calculate the next seat index in this pass direction, wrapping around the pod.
    pub fn next_seat(self, current: u8, pod_size: u8) -> u8 {
        match self {
            PassDirection::Left => (current + 1) % pod_size,
            PassDirection::Right => (current + pod_size - 1) % pod_size,
        }
    }
}

/// Overall status of a draft session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DraftStatus {
    Lobby,
    Drafting,
    Paused,
    Deckbuilding,
    Pairing,
    MatchInProgress,
    RoundComplete,
    Complete,
    Abandoned,
}

/// A single card instance in a draft pack or pool.
/// Lightweight collation type — NOT engine CardFace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftCardInstance {
    pub instance_id: String,
    pub name: String,
    pub set_code: String,
    pub collector_number: String,
    pub rarity: String,
}

/// A pack of cards, newtype wrapper over Vec<DraftCardInstance>.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPack(pub Vec<DraftCardInstance>);

/// A seat in the draft pod — either a human player or a bot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DraftSeat {
    Human {
        player_id: PlayerId,
        display_name: String,
        connected: bool,
    },
    Bot {
        name: String,
    },
}

/// Actions that can be performed on a draft session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DraftAction {
    StartDraft,
    Pick {
        seat: u8,
        card_instance_id: String,
    },
    SubmitDeck {
        seat: u8,
        main_deck: Vec<String>,
    },
}

/// State changes produced by applying a DraftAction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DraftDelta {
    DraftStarted,
    CardPicked {
        seat: u8,
        card_instance_id: String,
    },
    PackPassed,
    PackExhausted {
        new_pack_number: u8,
    },
    DeckSubmitted {
        seat: u8,
    },
    TransitionedTo {
        status: DraftStatus,
    },
}

/// Errors that can occur during draft operations.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum DraftError {
    #[error("invalid transition from {from:?}: {action}")]
    InvalidTransition { from: DraftStatus, action: String },
    #[error("seat {seat} out of range for pod size {pod_size}")]
    SeatOutOfRange { seat: u8, pod_size: u8 },
    #[error("card '{card_instance_id}' not found in pack")]
    CardNotInPack { card_instance_id: String },
    #[error("seat {seat} has no pending pack")]
    NoPendingPack { seat: u8 },
    #[error("deck validation failed")]
    ValidationFailed { errors: Vec<LimitedDeckError> },
}

/// Configuration for a draft session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftConfig {
    pub set_code: String,
    pub kind: DraftKind,
    pub cards_per_pack: u8,
    pub pack_count: u8,
    pub rng_seed: u64,
}

/// A player's submitted deck for limited play.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftDeckSubmission {
    pub seat: u8,
    pub main_deck: Vec<String>,
}

/// Win/loss record for a player in the draft event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftMatchRecord {
    pub player: PlayerId,
    pub wins: u8,
    pub losses: u8,
    pub draws: u8,
    pub match_wins: u8,
    pub match_losses: u8,
}

/// Status of a pairing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairingStatus {
    Pending,
    InProgress,
    Complete,
}

/// A pairing between two players for a match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftPairing {
    pub round: u8,
    pub table: u8,
    pub players: [PlayerId; 2],
    pub match_id: String,
    pub status: PairingStatus,
}

/// The full state of a draft session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftSession {
    pub draft_code: String,
    pub set_code: String,
    pub kind: DraftKind,
    pub status: DraftStatus,
    pub config: DraftConfig,
    pub seats: Vec<DraftSeat>,
    pub current_pack_number: u8,
    pub pick_number: u8,
    pub picks_this_round: u8,
    pub pass_direction: PassDirection,
    pub packs_by_seat: Vec<Vec<DraftPack>>,
    pub current_pack: Vec<Option<DraftPack>>,
    pub pools: Vec<Vec<DraftCardInstance>>,
    pub submitted_decks: HashMap<PlayerId, DraftDeckSubmission>,
    pub match_records: HashMap<PlayerId, DraftMatchRecord>,
    pub pairings: Vec<DraftPairing>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_kind_pod_size() {
        assert_eq!(DraftKind::Quick.pod_size(), 8);
        assert_eq!(DraftKind::Premier.pod_size(), 8);
        assert_eq!(DraftKind::Traditional.pod_size(), 8);
    }

    #[test]
    fn draft_kind_human_seats() {
        assert_eq!(DraftKind::Quick.human_seats(), 1);
        assert_eq!(DraftKind::Premier.human_seats(), 8);
        assert_eq!(DraftKind::Traditional.human_seats(), 8);
    }

    #[test]
    fn draft_kind_match_config() {
        assert_eq!(
            DraftKind::Quick.match_config().match_type,
            MatchType::Bo1
        );
        assert_eq!(
            DraftKind::Premier.match_config().match_type,
            MatchType::Bo1
        );
        assert_eq!(
            DraftKind::Traditional.match_config().match_type,
            MatchType::Bo3
        );
    }

    #[test]
    fn pass_direction_for_pack() {
        assert_eq!(PassDirection::for_pack(0), PassDirection::Left);
        assert_eq!(PassDirection::for_pack(1), PassDirection::Right);
        assert_eq!(PassDirection::for_pack(2), PassDirection::Left);
        assert_eq!(PassDirection::for_pack(3), PassDirection::Right);
    }

    #[test]
    fn pass_direction_next_seat_left() {
        assert_eq!(PassDirection::Left.next_seat(0, 8), 1);
        assert_eq!(PassDirection::Left.next_seat(7, 8), 0);
        assert_eq!(PassDirection::Left.next_seat(3, 8), 4);
    }

    #[test]
    fn pass_direction_next_seat_right() {
        assert_eq!(PassDirection::Right.next_seat(0, 8), 7);
        assert_eq!(PassDirection::Right.next_seat(1, 8), 0);
        assert_eq!(PassDirection::Right.next_seat(5, 8), 4);
    }

    #[test]
    fn serde_roundtrip_draft_kind() {
        for kind in [DraftKind::Quick, DraftKind::Premier, DraftKind::Traditional] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: DraftKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn serde_roundtrip_draft_status() {
        let statuses = [
            DraftStatus::Lobby,
            DraftStatus::Drafting,
            DraftStatus::Paused,
            DraftStatus::Deckbuilding,
            DraftStatus::Pairing,
            DraftStatus::MatchInProgress,
            DraftStatus::RoundComplete,
            DraftStatus::Complete,
            DraftStatus::Abandoned,
        ];
        for status in statuses {
            let json = serde_json::to_string(&status).unwrap();
            let back: DraftStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(status, back);
        }
    }

    #[test]
    fn serde_roundtrip_pass_direction() {
        for dir in [PassDirection::Left, PassDirection::Right] {
            let json = serde_json::to_string(&dir).unwrap();
            let back: PassDirection = serde_json::from_str(&json).unwrap();
            assert_eq!(dir, back);
        }
    }
}
