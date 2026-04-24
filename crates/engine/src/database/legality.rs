use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

pub type CardLegalities = HashMap<LegalityFormat, LegalityStatus>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalityFormat {
    Standard,
    Commander,
    Modern,
    Pioneer,
    Legacy,
    Vintage,
    Pauper,
    Historic,
    Brawl,
    StandardBrawl,
    Timeless,
    PauperCommander,
    DuelCommander,
}

impl LegalityFormat {
    pub const ALL: [Self; 13] = [
        Self::Standard,
        Self::Commander,
        Self::Modern,
        Self::Pioneer,
        Self::Legacy,
        Self::Vintage,
        Self::Pauper,
        Self::Historic,
        Self::Brawl,
        Self::StandardBrawl,
        Self::Timeless,
        Self::PauperCommander,
        Self::DuelCommander,
    ];

    pub fn as_key(self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::Commander => "commander",
            Self::Modern => "modern",
            Self::Pioneer => "pioneer",
            Self::Legacy => "legacy",
            Self::Vintage => "vintage",
            Self::Pauper => "pauper",
            Self::Historic => "historic",
            Self::Brawl => "brawl",
            Self::StandardBrawl => "standardbrawl",
            Self::Timeless => "timeless",
            Self::PauperCommander => "paupercommander",
            Self::DuelCommander => "duel",
        }
    }

    pub fn from_key(raw: &str) -> Option<Self> {
        match normalize_key(raw).as_str() {
            "standard" => Some(Self::Standard),
            "commander" => Some(Self::Commander),
            "modern" => Some(Self::Modern),
            "pioneer" => Some(Self::Pioneer),
            "legacy" => Some(Self::Legacy),
            "vintage" => Some(Self::Vintage),
            "pauper" => Some(Self::Pauper),
            "historic" => Some(Self::Historic),
            "brawl" => Some(Self::Brawl),
            "standardbrawl" => Some(Self::StandardBrawl),
            "timeless" => Some(Self::Timeless),
            "paupercommander" => Some(Self::PauperCommander),
            "duel" => Some(Self::DuelCommander),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalityStatus {
    Legal,
    NotLegal,
    Banned,
    Restricted,
}

impl LegalityStatus {
    pub fn from_raw(raw: &str) -> Option<Self> {
        match normalize_key(raw).as_str() {
            "legal" => Some(Self::Legal),
            "notlegal" => Some(Self::NotLegal),
            "banned" => Some(Self::Banned),
            "restricted" => Some(Self::Restricted),
            _ => None,
        }
    }

    pub fn as_export_str(self) -> &'static str {
        match self {
            Self::Legal => "legal",
            Self::NotLegal => "not_legal",
            Self::Banned => "banned",
            Self::Restricted => "restricted",
        }
    }

    pub fn is_legal(self) -> bool {
        matches!(self, Self::Legal)
    }
}

pub fn normalize_legalities(raw: &HashMap<String, String>) -> CardLegalities {
    let mut legalities = HashMap::new();
    for (key, value) in raw {
        let Some(format) = LegalityFormat::from_key(key) else {
            continue;
        };
        let Some(status) = LegalityStatus::from_raw(value) else {
            continue;
        };
        legalities.insert(format, status);
    }
    legalities
}

pub fn legalities_to_export_map(legalities: &CardLegalities) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for format in LegalityFormat::ALL {
        let Some(status) = legalities.get(&format).copied() else {
            continue;
        };
        out.insert(
            format.as_key().to_string(),
            status.as_export_str().to_string(),
        );
    }
    out
}

fn normalize_key(raw: &str) -> String {
    raw.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_parsing_handles_mtgjson_and_export_forms() {
        assert_eq!(
            LegalityStatus::from_raw("Legal"),
            Some(LegalityStatus::Legal)
        );
        assert_eq!(
            LegalityStatus::from_raw("Not Legal"),
            Some(LegalityStatus::NotLegal)
        );
        assert_eq!(
            LegalityStatus::from_raw("not_legal"),
            Some(LegalityStatus::NotLegal)
        );
        assert_eq!(
            LegalityStatus::from_raw("Restricted"),
            Some(LegalityStatus::Restricted)
        );
    }

    #[test]
    fn normalize_legalities_filters_to_supported_formats() {
        let mut raw = HashMap::new();
        raw.insert("standard".to_string(), "Legal".to_string());
        raw.insert("commander".to_string(), "Banned".to_string());
        // Deliberately nonsense keys so this test remains meaningful even if
        // we later add support for any real-but-currently-unsupported format
        // like `oldschool` or `premodern`. The contract being tested is
        // "unknown keys are dropped", not "any specific format is unknown".
        raw.insert("nonexistent_fmt_a".to_string(), "Legal".to_string());
        raw.insert("nonexistent_fmt_b".to_string(), "Legal".to_string());

        let result = normalize_legalities(&raw);
        assert_eq!(
            result.get(&LegalityFormat::Standard),
            Some(&LegalityStatus::Legal)
        );
        assert_eq!(
            result.get(&LegalityFormat::Commander),
            Some(&LegalityStatus::Banned)
        );
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn export_map_uses_stable_lowercase_strings() {
        let mut legalities = HashMap::new();
        legalities.insert(LegalityFormat::Standard, LegalityStatus::Legal);
        legalities.insert(LegalityFormat::Commander, LegalityStatus::NotLegal);

        let out = legalities_to_export_map(&legalities);
        assert_eq!(out.get("standard"), Some(&"legal".to_string()));
        assert_eq!(out.get("commander"), Some(&"not_legal".to_string()));
    }
}
