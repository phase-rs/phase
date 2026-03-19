use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// All static ability modes from Forge's static ability registry.
/// Matched case-sensitively against Forge mode strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum StaticMode {
    Continuous,
    CantAttack,
    CantBlock,
    CantBeTargeted,
    CantBeCast,
    CantBeActivated,
    CastWithFlash,
    ReduceCost,
    RaiseCost,
    CantGainLife,
    CantLoseLife,
    MustAttack,
    MustBlock,
    CantDraw,
    Panharmonicon,
    IgnoreHexproof,
    /// CR 509.1a + CR 509.1b: This creature can block additional creatures.
    /// `None` = any number, `Some(n)` = n additional creatures beyond the default 1.
    ExtraBlockers {
        count: Option<u32>,
    },
    /// Fallback for unrecognized static mode strings.
    Other(String),
}

impl fmt::Display for StaticMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StaticMode::Continuous => write!(f, "Continuous"),
            StaticMode::CantAttack => write!(f, "CantAttack"),
            StaticMode::CantBlock => write!(f, "CantBlock"),
            StaticMode::CantBeTargeted => write!(f, "CantBeTargeted"),
            StaticMode::CantBeCast => write!(f, "CantBeCast"),
            StaticMode::CantBeActivated => write!(f, "CantBeActivated"),
            StaticMode::CastWithFlash => write!(f, "CastWithFlash"),
            StaticMode::ReduceCost => write!(f, "ReduceCost"),
            StaticMode::RaiseCost => write!(f, "RaiseCost"),
            StaticMode::CantGainLife => write!(f, "CantGainLife"),
            StaticMode::CantLoseLife => write!(f, "CantLoseLife"),
            StaticMode::MustAttack => write!(f, "MustAttack"),
            StaticMode::MustBlock => write!(f, "MustBlock"),
            StaticMode::CantDraw => write!(f, "CantDraw"),
            StaticMode::Panharmonicon => write!(f, "Panharmonicon"),
            StaticMode::IgnoreHexproof => write!(f, "IgnoreHexproof"),
            StaticMode::ExtraBlockers { count } => match count {
                None => write!(f, "ExtraBlockers(any)"),
                Some(n) => write!(f, "ExtraBlockers({n})"),
            },
            StaticMode::Other(s) => write!(f, "{s}"),
        }
    }
}

impl FromStr for StaticMode {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mode = match s {
            "Continuous" => StaticMode::Continuous,
            "CantAttack" => StaticMode::CantAttack,
            "CantBlock" => StaticMode::CantBlock,
            "CantBeTargeted" => StaticMode::CantBeTargeted,
            "CantBeCast" => StaticMode::CantBeCast,
            "CantBeActivated" => StaticMode::CantBeActivated,
            "CastWithFlash" => StaticMode::CastWithFlash,
            "ReduceCost" => StaticMode::ReduceCost,
            "RaiseCost" => StaticMode::RaiseCost,
            "CantGainLife" => StaticMode::CantGainLife,
            "CantLoseLife" => StaticMode::CantLoseLife,
            "MustAttack" => StaticMode::MustAttack,
            "MustBlock" => StaticMode::MustBlock,
            "CantDraw" => StaticMode::CantDraw,
            "Panharmonicon" => StaticMode::Panharmonicon,
            "IgnoreHexproof" => StaticMode::IgnoreHexproof,
            other => StaticMode::Other(other.to_string()),
        };
        Ok(mode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_static_modes() {
        assert_eq!(
            StaticMode::from_str("Continuous").unwrap(),
            StaticMode::Continuous
        );
        assert_eq!(
            StaticMode::from_str("CantAttack").unwrap(),
            StaticMode::CantAttack
        );
        assert_eq!(
            StaticMode::from_str("Panharmonicon").unwrap(),
            StaticMode::Panharmonicon
        );
        assert_eq!(
            StaticMode::from_str("IgnoreHexproof").unwrap(),
            StaticMode::IgnoreHexproof
        );
    }

    #[test]
    fn parse_unknown_static_mode() {
        assert_eq!(
            StaticMode::from_str("FakeMode").unwrap(),
            StaticMode::Other("FakeMode".to_string())
        );
    }

    #[test]
    fn display_roundtrips() {
        let modes = vec![
            StaticMode::Continuous,
            StaticMode::CantAttack,
            StaticMode::Other("Custom".to_string()),
        ];
        for mode in modes {
            let s = mode.to_string();
            assert_eq!(StaticMode::from_str(&s).unwrap(), mode);
        }
    }

    #[test]
    fn serde_roundtrip() {
        let modes = vec![
            StaticMode::Continuous,
            StaticMode::CantBeTargeted,
            StaticMode::Other("Custom".to_string()),
        ];
        let json = serde_json::to_string(&modes).unwrap();
        let deserialized: Vec<StaticMode> = serde_json::from_str(&json).unwrap();
        assert_eq!(modes, deserialized);
    }

    #[test]
    fn static_mode_equality_with_string_comparison() {
        // Verify Display output matches the expected Forge string
        assert_eq!(StaticMode::Continuous.to_string(), "Continuous");
        assert_eq!(StaticMode::CantBlock.to_string(), "CantBlock");
        assert_eq!(
            StaticMode::Other("NewMode".to_string()).to_string(),
            "NewMode"
        );
    }
}
