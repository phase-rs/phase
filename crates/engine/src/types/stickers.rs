use serde::{Deserialize, Serialize};

/// CR 123.1: The four sticker kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum StickerKind {
    Name,
    Ability,
    PowerToughness,
    Art,
}

/// Stable identifier for one physical sticker on one sticker sheet.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StickerLocator {
    pub sheet: String,
    pub index: u8,
}

/// Sticker state carried by an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AppliedSticker {
    /// `position` stores how many words were before the sticker when it was
    /// placed (CR 123.6b / 123.6c).
    Name {
        locator: StickerLocator,
        text: String,
        position: u8,
        timestamp: u64,
    },
    Ability {
        locator: StickerLocator,
        ticket_cost: u8,
        text: String,
        timestamp: u64,
    },
    PowerToughness {
        locator: StickerLocator,
        ticket_cost: u8,
        power: i32,
        toughness: i32,
        timestamp: u64,
    },
    Art {
        locator: StickerLocator,
        label: String,
        timestamp: u64,
    },
}

impl AppliedSticker {
    pub fn kind(&self) -> StickerKind {
        match self {
            Self::Name { .. } => StickerKind::Name,
            Self::Ability { .. } => StickerKind::Ability,
            Self::PowerToughness { .. } => StickerKind::PowerToughness,
            Self::Art { .. } => StickerKind::Art,
        }
    }

    pub fn locator(&self) -> &StickerLocator {
        match self {
            Self::Name { locator, .. }
            | Self::Ability { locator, .. }
            | Self::PowerToughness { locator, .. }
            | Self::Art { locator, .. } => locator,
        }
    }
}
