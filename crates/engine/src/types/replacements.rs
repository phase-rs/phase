use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// All replacement event types from Forge's replacement effect registry.
/// Matched case-sensitively against Forge event strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ReplacementEvent {
    DamageDone,
    Destroy,
    Discard,
    Draw,
    LoseLife,
    GainLife,
    TurnFaceUp,
    Counter,
    ChangeZone,
    Moved,
    /// Fallback for unrecognized replacement event strings.
    Other(String),
}

impl fmt::Display for ReplacementEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ReplacementEvent::DamageDone => write!(f, "DamageDone"),
            ReplacementEvent::Destroy => write!(f, "Destroy"),
            ReplacementEvent::Discard => write!(f, "Discard"),
            ReplacementEvent::Draw => write!(f, "Draw"),
            ReplacementEvent::LoseLife => write!(f, "LoseLife"),
            ReplacementEvent::GainLife => write!(f, "GainLife"),
            ReplacementEvent::TurnFaceUp => write!(f, "TurnFaceUp"),
            ReplacementEvent::Counter => write!(f, "Counter"),
            ReplacementEvent::ChangeZone => write!(f, "ChangeZone"),
            ReplacementEvent::Moved => write!(f, "Moved"),
            ReplacementEvent::Other(s) => write!(f, "{s}"),
        }
    }
}

impl FromStr for ReplacementEvent {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let event = match s {
            "DamageDone" => ReplacementEvent::DamageDone,
            "Destroy" => ReplacementEvent::Destroy,
            "Discard" => ReplacementEvent::Discard,
            "Draw" => ReplacementEvent::Draw,
            "LoseLife" => ReplacementEvent::LoseLife,
            "GainLife" => ReplacementEvent::GainLife,
            "TurnFaceUp" => ReplacementEvent::TurnFaceUp,
            "Counter" => ReplacementEvent::Counter,
            "ChangeZone" => ReplacementEvent::ChangeZone,
            "Moved" => ReplacementEvent::Moved,
            other => ReplacementEvent::Other(other.to_string()),
        };
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_replacement_events() {
        assert_eq!(
            ReplacementEvent::from_str("DamageDone").unwrap(),
            ReplacementEvent::DamageDone
        );
        assert_eq!(
            ReplacementEvent::from_str("Destroy").unwrap(),
            ReplacementEvent::Destroy
        );
        assert_eq!(
            ReplacementEvent::from_str("Moved").unwrap(),
            ReplacementEvent::Moved
        );
    }

    #[test]
    fn parse_unknown_replacement_event() {
        assert_eq!(
            ReplacementEvent::from_str("FakeEvent").unwrap(),
            ReplacementEvent::Other("FakeEvent".to_string())
        );
    }

    #[test]
    fn display_roundtrips() {
        let events = vec![
            ReplacementEvent::DamageDone,
            ReplacementEvent::Moved,
            ReplacementEvent::Other("Custom".to_string()),
        ];
        for event in events {
            let s = event.to_string();
            assert_eq!(ReplacementEvent::from_str(&s).unwrap(), event);
        }
    }

    #[test]
    fn serde_roundtrip() {
        let events = vec![
            ReplacementEvent::DamageDone,
            ReplacementEvent::Destroy,
            ReplacementEvent::Other("Custom".to_string()),
        ];
        let json = serde_json::to_string(&events).unwrap();
        let deserialized: Vec<ReplacementEvent> = serde_json::from_str(&json).unwrap();
        assert_eq!(events, deserialized);
    }

    #[test]
    fn replacement_event_display_matches_forge_string() {
        assert_eq!(ReplacementEvent::DamageDone.to_string(), "DamageDone");
        assert_eq!(ReplacementEvent::Moved.to_string(), "Moved");
        assert_eq!(
            ReplacementEvent::Other("NewEvent".to_string()).to_string(),
            "NewEvent"
        );
    }
}
