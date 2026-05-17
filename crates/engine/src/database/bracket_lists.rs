//! Bracket list data — hand-curated card names that drive Commander bracket
//! estimator axes not supplied by MTGJSON (Mass Land Denial / Extra Turns /
//! Efficient Tutors). Game Changers come from MTGJSON `isGameChanger`.
//!
//! This is NOT a Comprehensive Rules artifact — the bracket system is
//! WotC's Commander Format Panel policy, not part of the CR.

use std::collections::HashSet;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Per-card bracket signal flags. Stamped onto every `CardExportEntry`
/// during `oracle-gen data` so the runtime engine and frontend both see
/// the signals without re-reading `bracket_lists.json`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BracketSignals {
    #[serde(default)]
    pub game_changer: bool,
    #[serde(default)]
    pub mass_land_denial: bool,
    #[serde(default)]
    pub extra_turn: bool,
    #[serde(default)]
    pub efficient_tutor: bool,
}

impl BracketSignals {
    pub fn is_clean(self) -> bool {
        !self.game_changer && !self.mass_land_denial && !self.extra_turn && !self.efficient_tutor
    }
}

/// The non-MTGJSON name lists + a version tag, loaded from `data/bracket_lists.json`.
/// Names are stored lowercased for case-insensitive lookup.
#[derive(Debug, Clone, Default)]
pub struct BracketLists {
    pub version: String,
    pub source: String,
    pub mass_land_denial: HashSet<String>,
    pub extra_turns: HashSet<String>,
    pub efficient_tutors: HashSet<String>,
}

#[derive(Debug, Deserialize)]
struct RawBracketLists {
    version: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    mass_land_denial: Vec<String>,
    #[serde(default)]
    extra_turns: Vec<String>,
    #[serde(default)]
    efficient_tutors: Vec<String>,
}

impl BracketLists {
    /// Load from a JSON file. Names are lowercased into `HashSet`s for O(1)
    /// case-insensitive lookup.
    pub fn from_json_path(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let raw = std::fs::read_to_string(path)?;
        Self::from_json_str(&raw)
    }

    pub fn from_json_str(raw: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let parsed: RawBracketLists = serde_json::from_str(raw)?;
        Ok(Self {
            version: parsed.version,
            source: parsed.source,
            mass_land_denial: parsed
                .mass_land_denial
                .into_iter()
                .map(|s| s.to_lowercase())
                .collect(),
            extra_turns: parsed
                .extra_turns
                .into_iter()
                .map(|s| s.to_lowercase())
                .collect(),
            efficient_tutors: parsed
                .efficient_tutors
                .into_iter()
                .map(|s| s.to_lowercase())
                .collect(),
        })
    }

    /// Look up the bracket signals for a card name (case-insensitive).
    /// Returns an all-false `BracketSignals` for unknown names.
    pub fn signals_for(&self, name: &str) -> BracketSignals {
        let key = name.to_lowercase();
        BracketSignals {
            game_changer: false,
            mass_land_denial: self.mass_land_denial.contains(&key),
            extra_turn: self.extra_turns.contains(&key),
            efficient_tutor: self.efficient_tutors.contains(&key),
        }
    }

    /// Iterate every distinct card name across all four lists, used by the
    /// export pipeline to warn on names that don't match any printed card.
    pub fn all_names(&self) -> impl Iterator<Item = &str> {
        let mut seen = HashSet::new();
        self.mass_land_denial
            .iter()
            .chain(self.extra_turns.iter())
            .chain(self.efficient_tutors.iter())
            .filter(move |s| seen.insert(s.as_str()))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "version": "test-1",
        "source": "test",
        "mass_land_denial": ["Armageddon"],
        "extra_turns": ["Time Warp"],
        "efficient_tutors": ["Demonic Tutor", "Vampiric Tutor"]
    }"#;

    #[test]
    fn parses_version_and_lists() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert_eq!(lists.version, "test-1");
        assert_eq!(lists.efficient_tutors.len(), 2);
    }

    #[test]
    fn signals_for_known_card() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        let sig = lists.signals_for("Demonic Tutor");
        assert!(sig.efficient_tutor);
        assert!(!sig.game_changer);
        assert!(!sig.mass_land_denial);
        assert!(!sig.extra_turn);
    }

    #[test]
    fn signals_for_game_changer_is_not_curated() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        let sig = lists.signals_for("Sol Ring");
        assert!(!sig.game_changer);
        assert!(!sig.efficient_tutor);
    }

    #[test]
    fn signals_are_case_insensitive() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        assert!(lists.signals_for("DEMONIC TUTOR").efficient_tutor);
        assert!(lists.signals_for("demonic tutor").efficient_tutor);
    }

    #[test]
    fn signals_for_unknown_card_is_clean() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        let sig = lists.signals_for("Llanowar Elves");
        assert!(sig.is_clean());
    }

    #[test]
    fn all_names_iterates_every_list() {
        let lists = BracketLists::from_json_str(SAMPLE).unwrap();
        let count = lists.all_names().count();
        assert_eq!(count, 4);
    }
}
