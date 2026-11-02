use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// ─── Dungeon Identity ────────────────────────────────────────────────────────

/// CR 309: The five dungeon cards across D&D crossover sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DungeonId {
    LostMineOfPhandelver,
    DungeonOfTheMadMage,
    TombOfAnnihilation,
    Undercity,
    BaldursGateWilderness,
}

impl fmt::Display for DungeonId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LostMineOfPhandelver => write!(f, "Lost Mine of Phandelver"),
            Self::DungeonOfTheMadMage => write!(f, "Dungeon of the Mad Mage"),
            Self::TombOfAnnihilation => write!(f, "Tomb of Annihilation"),
            Self::Undercity => write!(f, "Undercity"),
            Self::BaldursGateWilderness => write!(f, "Baldur's Gate Wilderness"),
        }
    }
}

impl FromStr for DungeonId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "LostMineOfPhandelver" => Ok(Self::LostMineOfPhandelver),
            "DungeonOfTheMadMage" => Ok(Self::DungeonOfTheMadMage),
            "TombOfAnnihilation" => Ok(Self::TombOfAnnihilation),
            "Undercity" => Ok(Self::Undercity),
            "BaldursGateWilderness" => Ok(Self::BaldursGateWilderness),
            _ => Err(format!("Unknown dungeon: {s}")),
        }
    }
}
