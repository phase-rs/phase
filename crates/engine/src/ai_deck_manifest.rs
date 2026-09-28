//! Dated, server-readable Commander AI deck inventory.
//!
//! An AI decklist is perishable against the ban list and Game Changers revisions,
//! just like `data/bracket_lists.json`. This is not a Comprehensive Rules artifact:
//! Commander brackets are WotC format guidance, so tier fields have no rules annotation.

use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::game::bracket_estimate::CommanderBracketTier;
use crate::starter_decks::DeckData;

/// Dated like `data/bracket_lists.json` (whose `version` reads "2025-09-24-wotc"):
/// an AI decklist is perishable against the ban list and the Game Changers
/// revisions. NOT a Comprehensive Rules artifact - Commander brackets are WotC
/// format guidance, so no rules annotation applies to the tier fields.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiDeckManifest {
    pub version: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub decks: Vec<AiDeckManifestEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiDeckManifestEntry {
    pub id: String,
    pub name: String,
    pub commander: Vec<String>,
    pub main_deck: Vec<String>,
    /// `None` == genuinely unlabelled. Never defaulted to `Core`: `DeckData`'s
    /// `bracket_tier` default (starter_decks.rs:36-38) is a wire-compat default,
    /// not a claim, and treating it as one is how an untagged feed deck would
    /// silently become a Bracket-2 declaration.
    #[serde(default)]
    pub declared: Option<CommanderBracketTier>,
    pub source: String,
    pub source_date: String,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("failed to read AI deck manifest: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse AI deck manifest JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("AI deck manifest version must not be empty")]
    EmptyVersion,
    #[error("AI deck manifest contains duplicate id `{0}` after case folding")]
    DuplicateId(String),
    #[error("AI deck manifest entry `{0}` has no commander")]
    EmptyCommander(String),
    #[error(
        "AI deck manifest entry `{id}` has only {count} main-deck cards; at least 60 are required"
    )]
    MainDeckTooSmall { id: String, count: usize },
}

impl AiDeckManifest {
    /// The compiled-in default. Mirrors the production `include_str!` of
    /// `crates/engine/data/oracle-subtypes.json` at parser/oracle_util.rs:1146.
    pub fn bundled() -> &'static AiDeckManifest {
        static BUNDLED: OnceLock<AiDeckManifest> = OnceLock::new();
        BUNDLED.get_or_init(|| {
            Self::from_json_str(include_str!("../data/ai_commander_decks.json")).unwrap_or_else(
                |error| panic!("compiled-in AI Commander deck manifest is invalid: {error}"),
            )
        })
    }

    pub fn from_json_str(raw: &str) -> Result<Self, ManifestError> {
        let manifest: Self = serde_json::from_str(raw)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn from_json_path(path: &Path) -> Result<Self, ManifestError> {
        Self::from_json_str(&std::fs::read_to_string(path)?)
    }

    /// Finds a deck case-insensitively by stable id or display name.
    pub fn find(&self, name: &str) -> Option<&AiDeckManifestEntry> {
        let name = name.to_lowercase();
        self.decks
            .iter()
            .find(|deck| deck.id.to_lowercase() == name || deck.name.to_lowercase() == name)
    }

    pub fn commander_decks(&self) -> impl Iterator<Item = &AiDeckManifestEntry> {
        self.decks.iter()
    }

    fn validate(&self) -> Result<(), ManifestError> {
        if self.version.trim().is_empty() {
            return Err(ManifestError::EmptyVersion);
        }

        let mut ids = HashSet::with_capacity(self.decks.len());
        for deck in &self.decks {
            if !ids.insert(deck.id.to_lowercase()) {
                return Err(ManifestError::DuplicateId(deck.id.clone()));
            }
            if deck.commander.is_empty() {
                return Err(ManifestError::EmptyCommander(deck.id.clone()));
            }
            if deck.main_deck.len() < 60 {
                return Err(ManifestError::MainDeckTooSmall {
                    id: deck.id.clone(),
                    count: deck.main_deck.len(),
                });
            }
        }
        Ok(())
    }
}

impl AiDeckManifestEntry {
    /// `bracket_tier` falls back to `CommanderBracketTier::default()` (== `Core`,
    /// bracket_estimate.rs:31-36) ONLY at this boundary, because `DeckData` has no
    /// "unknown" representation. The manifest's own `declared: None` is what the
    /// selector reads; this conversion happens after selection.
    pub fn to_deck_data(&self) -> DeckData {
        DeckData {
            commander: self.commander.clone(),
            main_deck: self.main_deck.clone(),
            bracket_tier: self.declared.unwrap_or_default(),
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": id,
            "commander": ["Commander"],
            "main_deck": vec!["Card"; 60],
            "declared": null,
            "source": "test",
            "source_date": "2026-09-27"
        })
    }

    fn manifest(decks: Vec<serde_json::Value>) -> String {
        serde_json::json!({ "version": "test", "decks": decks }).to_string()
    }

    #[test]
    fn bundled_manifest_is_validated_when_initialized() {
        assert_eq!(AiDeckManifest::bundled().decks.len(), 13);
    }

    #[test]
    fn reports_json_and_io_errors() {
        assert!(matches!(
            AiDeckManifest::from_json_str("not json"),
            Err(ManifestError::Json(_))
        ));
        let missing = std::env::temp_dir().join(format!(
            "phase-missing-ai-manifest-{}.json",
            std::process::id()
        ));
        assert!(matches!(
            AiDeckManifest::from_json_path(&missing),
            Err(ManifestError::Io(_))
        ));
    }

    #[test]
    fn reports_empty_version_and_duplicate_id_errors() {
        let empty_version = serde_json::json!({ "version": " ", "decks": [] }).to_string();
        assert!(matches!(
            AiDeckManifest::from_json_str(&empty_version),
            Err(ManifestError::EmptyVersion)
        ));

        let duplicate = manifest(vec![entry("Deck"), entry("deck")]);
        assert!(matches!(
            AiDeckManifest::from_json_str(&duplicate),
            Err(ManifestError::DuplicateId(id)) if id == "deck"
        ));
    }

    #[test]
    fn reports_empty_commander_and_small_main_deck_errors() {
        let mut empty_commander = entry("empty");
        empty_commander["commander"] = serde_json::json!([]);
        assert!(matches!(
            AiDeckManifest::from_json_str(&manifest(vec![empty_commander])),
            Err(ManifestError::EmptyCommander(id)) if id == "empty"
        ));

        let mut too_small = entry("small");
        too_small["main_deck"] = serde_json::json!(vec!["Card"; 59]);
        assert!(matches!(
            AiDeckManifest::from_json_str(&manifest(vec![too_small])),
            Err(ManifestError::MainDeckTooSmall { id, count }) if id == "small" && count == 59
        ));
    }
}
