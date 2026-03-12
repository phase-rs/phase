use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::player::PlayerId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub enum MatchType {
    #[default]
    Bo1,
    Bo3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub enum MatchPhase {
    #[default]
    InGame,
    BetweenGames,
    Completed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct MatchConfig {
    pub match_type: MatchType,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct MatchScore {
    pub p0_wins: u8,
    pub p1_wins: u8,
    pub draws: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DeckCardCount {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BetweenGamesPrompt {
    pub game_number: u8,
    pub score: MatchScore,
    pub player: PlayerId,
}
