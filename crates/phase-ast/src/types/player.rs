use std::fmt;

use serde::{Deserialize, Serialize};

/// CR 122.1b: Named player counter types tracked by the engine.
/// Poison counters route to the dedicated `poison_counters` field due to SBA rules (CR 704.5c).
/// Energy counters are excluded — they use the dedicated `energy` field and `GainEnergy` effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum PlayerCounterKind {
    Poison,
    Experience,
    Rad,
    Ticket,
}

impl fmt::Display for PlayerCounterKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Poison => write!(f, "poison"),
            Self::Experience => write!(f, "experience"),
            Self::Rad => write!(f, "rad"),
            Self::Ticket => write!(f, "ticket"),
        }
    }
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct PlayerId(pub u8);
