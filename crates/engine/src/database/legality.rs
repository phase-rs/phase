use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::game::bracket_estimate::CommanderBracketTier;
use crate::game::deck_loading::PlayerDeckPayload;

pub type CardLegalities = HashMap<LegalityFormat, LegalityStatus>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegalityFormat {
    Standard,
    Commander,
    Modern,
    Premodern,
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
    pub const ALL: [Self; 14] = [
        Self::Standard,
        Self::Commander,
        Self::Modern,
        Self::Premodern,
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
            Self::Premodern => "premodern",
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
            "premodern" => Some(Self::Premodern),
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

// ---------------------------------------------------------------------------
// cEDH bracket validation
// ---------------------------------------------------------------------------

/// Error returned when one or more decks in a cEDH table are not declared as
/// `CommanderBracketTier::Cedh`.
///
/// cEDH is a manual-declaration-only tier: the bracket estimator never
/// returns `Cedh` algorithmically (see `bracket_estimate.rs:18-27` and the
/// `estimator_never_returns_cedh` test). Validation is therefore a simple tag
/// check — every deck at a cEDH table must carry the explicit declaration.
///
/// Kept as an enum (rather than a struct) to accommodate anticipated future
/// variants once Phase 8 wires this validation into the game-init boundary —
/// e.g., `AllDecksUnconfigured` (table has no bracket declarations at all) or
/// `TableSizeMismatch` (seat count doesn't match expected player count).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CedhBracketError {
    /// The deck at `seat_index` was not declared as `Cedh`.
    DeckNotCedh {
        seat_index: u8,
        actual_tier: CommanderBracketTier,
    },
}

impl std::fmt::Display for CedhBracketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DeckNotCedh {
                seat_index,
                actual_tier,
            } => write!(
                f,
                "seat {seat_index} is not declared cEDH (actual tier: {actual_tier})"
            ),
        }
    }
}

impl std::error::Error for CedhBracketError {}

/// Validate that every deck in a proposed cEDH table is explicitly declared as
/// `CommanderBracketTier::Cedh`.
///
/// Returns `Ok(())` when all decks pass (including the vacuously-true empty
/// slice). Returns `Err(CedhBracketError::DeckNotCedh)` for the first
/// offending deck (lowest seat index).
///
/// This is a **tag check only** — the estimator never assigns `Cedh`
/// algorithmically, so the declaration must come from the player at deck
/// submission time.
pub fn validate_cedh_bracket(decks: &[&PlayerDeckPayload]) -> Result<(), CedhBracketError> {
    for (idx, deck) in decks.iter().enumerate() {
        if deck.bracket_tier != CommanderBracketTier::Cedh {
            return Err(CedhBracketError::DeckNotCedh {
                // Commander has at most 4 seats; cast is always safe.
                seat_index: idx as u8,
                actual_tier: deck.bracket_tier,
            });
        }
    }
    Ok(())
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
        raw.insert("premodern".to_string(), "Legal".to_string());
        // Deliberately nonsense keys so this test remains meaningful even if
        // we later add support for any real-but-currently-unsupported format
        // like `oldschool` or `futurecasual`. The contract being tested is
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
        assert_eq!(
            result.get(&LegalityFormat::Premodern),
            Some(&LegalityStatus::Legal)
        );
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn export_map_uses_stable_lowercase_strings() {
        let mut legalities = HashMap::new();
        legalities.insert(LegalityFormat::Standard, LegalityStatus::Legal);
        legalities.insert(LegalityFormat::Premodern, LegalityStatus::Banned);
        legalities.insert(LegalityFormat::Commander, LegalityStatus::NotLegal);

        let out = legalities_to_export_map(&legalities);
        assert_eq!(out.get("standard"), Some(&"legal".to_string()));
        assert_eq!(out.get("premodern"), Some(&"banned".to_string()));
        assert_eq!(out.get("commander"), Some(&"not_legal".to_string()));
    }
}

#[cfg(test)]
mod cedh_bracket_tests {
    use super::{validate_cedh_bracket, CedhBracketError};
    use crate::game::bracket_estimate::CommanderBracketTier;
    use crate::game::deck_loading::PlayerDeckPayload;

    fn cedh_deck() -> PlayerDeckPayload {
        PlayerDeckPayload {
            bracket_tier: CommanderBracketTier::Cedh,
            ..Default::default()
        }
    }

    fn non_cedh_deck(tier: CommanderBracketTier) -> PlayerDeckPayload {
        PlayerDeckPayload {
            bracket_tier: tier,
            ..Default::default()
        }
    }

    #[test]
    fn validate_accepts_all_cedh_decks() {
        let d0 = cedh_deck();
        let d1 = cedh_deck();
        let d2 = cedh_deck();
        let d3 = cedh_deck();
        let result = validate_cedh_bracket(&[&d0, &d1, &d2, &d3]);
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn validate_rejects_a_non_cedh_deck() {
        let d0 = cedh_deck();
        let d1 = non_cedh_deck(CommanderBracketTier::Optimized);
        let d2 = cedh_deck();
        let result = validate_cedh_bracket(&[&d0, &d1, &d2]);
        assert_eq!(
            result,
            Err(CedhBracketError::DeckNotCedh {
                seat_index: 1,
                actual_tier: CommanderBracketTier::Optimized,
            })
        );
    }

    #[test]
    fn validate_accepts_empty_input() {
        // Vacuously true: no decks means no violations.
        let result = validate_cedh_bracket(&[]);
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn validate_returns_first_offender_by_seat_index() {
        // When multiple decks fail, the first by seat index is reported.
        let d0 = non_cedh_deck(CommanderBracketTier::Core);
        let d1 = non_cedh_deck(CommanderBracketTier::Upgraded);
        let d2 = cedh_deck();
        let result = validate_cedh_bracket(&[&d0, &d1, &d2]);
        assert_eq!(
            result,
            Err(CedhBracketError::DeckNotCedh {
                seat_index: 0,
                actual_tier: CommanderBracketTier::Core,
            })
        );
    }

    #[test]
    fn cedh_bracket_error_display_includes_seat_and_tier() {
        let err = CedhBracketError::DeckNotCedh {
            seat_index: 2,
            actual_tier: CommanderBracketTier::Upgraded,
        };
        let msg = err.to_string();
        assert!(
            msg.contains("seat 2"),
            "expected seat index in message: {msg}"
        );
        assert!(
            msg.contains("Upgraded"),
            "expected tier name in message: {msg}"
        );
    }
}
