use serde::{Deserialize, Serialize};

use super::phase::Phase;
use super::player::{Player, PlayerId};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameState {
    pub turn_number: u32,
    pub active_player: PlayerId,
    pub phase: Phase,
    pub players: Vec<Player>,
    pub priority_player: PlayerId,
}

impl Default for GameState {
    fn default() -> Self {
        GameState {
            turn_number: 0,
            active_player: PlayerId(0),
            phase: Phase::Untap,
            players: vec![
                Player {
                    id: PlayerId(0),
                    life: 20,
                    ..Player::default()
                },
                Player {
                    id: PlayerId(1),
                    life: 20,
                    ..Player::default()
                },
            ],
            priority_player: PlayerId(0),
        }
    }
}
