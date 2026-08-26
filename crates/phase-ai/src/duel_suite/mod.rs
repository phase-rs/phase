//! AI duel suite — pinned-snapshot matchup registry and structured run output.
//!
//! This module powers the `ai-duel --suite` tooling. It decouples metagame
//! matchup definitions from `client/public/feeds/` (which rotate monthly) by
//! pinning deck snapshots in `crates/phase-ai/duel_decks/<format>/<deck>.json`.
//! Every `DeckFeatures` axis is exercised by at least one matchup; a
//! compile-time test in `tests.rs` enforces the invariant.

pub mod attribution;
pub mod compare;
pub mod harvest;
pub mod inline_decks;
pub mod perf;
pub mod run;
pub mod snapshots;
pub mod spec;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

pub use run::{run_suite, MatchupResult, SuiteReport, SuiteStatus};
pub use snapshots::{load_snapshot, resolve_deck_ref, SnapshotError};
pub use spec::{all_matchups, find_matchup, MATCHUPS};

/// Mirror of the `DeckFeatures` axes — one variant per feature module in
/// `crate::features`. When a new feature is added to `DeckFeatures`, a matching
/// variant must be added here (the `feature_kind_matches_deck_features_field_count`
/// test will fail otherwise) and the new feature must appear in at least one
/// `MatchupSpec::exercises` list (the `every_feature_kind_is_exercised` test
/// will fail otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeatureKind {
    Landfall,
    ManaRamp,
    Tribal,
    Control,
    Aristocrats,
    Artifacts,
    Enchantments,
    AggroPressure,
    TokensWide,
    PlusOneCounters,
    SpellslingerProwess,
    Reanimator,
    Equipment,
    Blink,
    Mill,
    Energy,
}

impl FeatureKind {
    /// Every variant, in declaration order. Used by the CI invariant test that
    /// ensures each feature is exercised by at least one matchup.
    pub const ALL: &'static [FeatureKind] = &[
        FeatureKind::Landfall,
        FeatureKind::ManaRamp,
        FeatureKind::Tribal,
        FeatureKind::Control,
        FeatureKind::Aristocrats,
        FeatureKind::Artifacts,
        FeatureKind::Enchantments,
        FeatureKind::AggroPressure,
        FeatureKind::TokensWide,
        FeatureKind::PlusOneCounters,
        FeatureKind::SpellslingerProwess,
        FeatureKind::Reanimator,
        FeatureKind::Equipment,
        FeatureKind::Blink,
        FeatureKind::Mill,
        FeatureKind::Energy,
    ];
}

/// Reference to a decklist. Snapshot decks live on disk as JSON
/// (`crates/phase-ai/duel_decks/<format>/<file>`); inline decks are built from
/// existing Rust functions in `ai_duel.rs`.
#[derive(Clone, Copy)]
pub enum DeckRef {
    /// Pinned JSON snapshot relative to `crates/phase-ai/duel_decks/`.
    Snapshot {
        format: &'static str,
        file: &'static str,
    },
    /// Hardcoded starter deck built by a Rust function.
    Inline {
        label: &'static str,
        build: fn() -> Vec<String>,
    },
}

impl std::fmt::Debug for DeckRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeckRef::Snapshot { format, file } => f
                .debug_struct("Snapshot")
                .field("format", format)
                .field("file", file)
                .finish(),
            DeckRef::Inline { label, .. } => {
                f.debug_struct("Inline").field("label", label).finish()
            }
        }
    }
}

/// Outcome tolerance for a matchup.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Expected {
    /// Mirror match: P0 winrate must fall within `(0.5 - tolerance, 0.5 + tolerance)`.
    Mirror { tolerance: f32 },
    /// Archetype triangle: P0 winrate must fall in `[p0_winrate_min, p0_winrate_max]`.
    Triangle {
        p0_winrate_min: f32,
        p0_winrate_max: f32,
    },
    /// No expectation — informational only, never fails.
    Open,
}

/// A single registered matchup. Static in `spec::MATCHUPS`.
#[derive(Debug, Clone, Copy)]
pub struct MatchupSpec {
    pub id: &'static str,
    pub p0_label: &'static str,
    pub p1_label: &'static str,
    pub p0: DeckRef,
    pub p1: DeckRef,
    pub exercises: &'static [FeatureKind],
    pub expected: Expected,
}
