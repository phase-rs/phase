use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;
use super::player::PlayerId;

/// Represents who a creature is attacking: a player, planeswalker, or battle (CR 506.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AttackTarget {
    Player(PlayerId),
    Planeswalker(ObjectId),
    Battle(ObjectId),
}
