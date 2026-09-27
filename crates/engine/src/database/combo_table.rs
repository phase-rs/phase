//! Runtime combo-table data used by the Commander bracket estimator.
//!
//! This is NOT a Comprehensive Rules artifact — the bracket system is WotC's
//! Commander Format Panel policy, not part of the CR. No rules annotation
//! belongs on anything in this module.

use std::collections::BTreeSet;

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use smallvec::SmallVec;
use thiserror::Error;

use crate::analysis::ResourceAxis;
use crate::game::bracket_estimate::BracketAxis;
use crate::game::deck_loading::PlayerDeckList;

/// Where the artifact came from and when.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboProvenance {
    pub snapshot_date: String,
    pub table_version: String,
    pub attribution: String,
    pub card_pool_version: String,
    pub filtered: ComboFilterCounts,
    pub omitted: Vec<ComboOmission>,
}

/// Counts retained rows and each deliberate filtering decision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboFilterCounts {
    pub not_commander_legal: u32,
    pub not_ok: u32,
    pub template: u32,
    pub one_card: u32,
    pub three_or_more: u32,
    pub unknown_card: u32,
    pub irrelevant: u32,
    pub kept: u32,
    pub multi_zone_pieces: u32,
}

/// Source fields and result classes deliberately omitted from the artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboOmission {
    PrerequisiteText,
    ResultText,
    UnmodeledResultClasses,
}

/// One commander-legal, template-free, exactly-two-card, relevance-passing row.
///
/// THE FACTS-ONLY GUARANTEE IS THIS STRUCT'S SHAPE: the only `String` fields are
/// the two card names inside `pieces`, and the generator drops any row whose names
/// do not resolve in `CardDatabase`. There is no free-text field, so no
/// contributor-authored expression can be carried even by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboEntry {
    pub pieces: [ComboPiece; 2],
    pub relevance: ComboRelevance,
    pub setup: ComboSetup,
    pub mana_value_needed: u16,
    pub assemble_cost: u16,
    pub popularity: u32,
    pub outcomes: BTreeSet<ComboOutcome>,
    pub axes: BTreeSet<BracketAxis>,
}

/// One resolved card in a combo row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboPiece {
    /// Lowercased key matching `CardDatabase::face_index`.
    pub key: String,
    /// Canonical printed card name.
    pub display: String,
    pub zone: ComboPieceZone,
}

/// How much of the line the two named cards carry alone.
///
/// Derived only from structured `produces[].feature.status`, never from prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboRelevance {
    Helper,
    Contextual,
    Standalone,
}

/// Whether the source records notable prerequisites for the row.
///
/// The text is never carried; this records only that the field was non-empty. A
/// named enum is used instead of a boolean, following `ModelCompleteness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboSetup {
    AsPrinted,
    NotablePrerequisites,
}

/// Where a piece must begin for the source cardinality reading.
///
/// DELIBERATELY LOSSY: the source zone value is a set and can contain both the
/// command zone and library. The runtime rule branches library-first, so this
/// total order preserves that rule; provenance counts multi-zone projections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboPieceZone {
    Library,
    CommandZone,
    Anywhere,
}

/// What the external table says a pair does, in the data layer's vocabulary.
///
/// This is NOT `ResourceAxis`: its payload-bearing variants require runtime
/// identities a static row cannot supply. It is also NOT `WinKind`: that type is
/// a rules-derived soundness certificate, and constructing it from table data
/// would launder the external assertion's provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "resource", rename_all = "snake_case")]
pub enum ComboOutcome {
    Wins,
    Unbounded(ComboResource),
}

/// A payload-free resource named by an external combo row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboResource {
    Mana,
    Damage,
    LifeLoss,
    Lifegain,
    Mill,
    Draw,
    Tokens,
    Combat,
    Turns,
}

impl ComboOutcome {
    /// One-way, explicitly lossy, display-only projection onto engine vocabulary.
    pub fn to_resource_axis(self) -> Option<ResourceAxis> {
        match self {
            // `Mana` needs a `ManaType` payload that a static row cannot supply.
            Self::Unbounded(ComboResource::Mana) => None,
            // `DamageDealt` needs a `PlayerId` payload that a static row cannot supply.
            Self::Unbounded(ComboResource::Damage) => None,
            // `Life` needs a `PlayerId`; the table also cannot preserve loss direction.
            Self::Unbounded(ComboResource::LifeLoss) => None,
            // `Life` needs a `PlayerId`; the table also cannot preserve gain direction.
            Self::Unbounded(ComboResource::Lifegain) => None,
            // `LibraryDelta` needs a `PlayerId` payload that a static row cannot supply.
            Self::Unbounded(ComboResource::Mill) => None,
            // Draw is payload-free in both vocabularies, so the display label is faithful.
            Self::Unbounded(ComboResource::Draw) => Some(ResourceAxis::CardsDrawn),
            // Tokens are payload-free in both vocabularies, so the display label is faithful.
            Self::Unbounded(ComboResource::Tokens) => Some(ResourceAxis::TokensCreated),
            // Combat phases are payload-free in both vocabularies.
            Self::Unbounded(ComboResource::Combat) => Some(ResourceAxis::CombatPhases),
            // Extra turns are payload-free in both vocabularies.
            Self::Unbounded(ComboResource::Turns) => Some(ResourceAxis::ExtraTurns),
            // `WinKind` is never constructed from externally asserted table data.
            Self::Wins => None,
        }
    }
}

/// Whether combo coverage was measured from an artifact.
///
/// This is deliberately separate from `EstimateConfidence`, which describes
/// card-name resolution rather than whether a combo artifact was present.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboCoverage {
    #[default]
    Unmeasured,
    Measured,
}

/// A combo row's per-deck cardinality reading.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComboCardinality {
    DefinitelyTwoCard,
    ArguablyTwoCard,
    More,
}

/// Which cardinality reading an early-combo floor accepts.
///
/// This is a named constant on a floor row, never a runtime tunable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EarlyComboReading {
    DefinitelyOnly,
    IncludingArguable,
}

impl EarlyComboReading {
    pub fn accepts(self, cardinality: ComboCardinality) -> bool {
        match (self, cardinality) {
            (Self::DefinitelyOnly, ComboCardinality::DefinitelyTwoCard)
            | (Self::IncludingArguable, ComboCardinality::DefinitelyTwoCard)
            | (Self::IncludingArguable, ComboCardinality::ArguablyTwoCard) => true,
            (Self::DefinitelyOnly, ComboCardinality::ArguablyTwoCard)
            | (Self::DefinitelyOnly, ComboCardinality::More)
            | (Self::IncludingArguable, ComboCardinality::More) => false,
        }
    }
}

/// Where a bracket signal originated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalSource {
    CardName,
    ComboPair,
}

/// One matched pair in one deck.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboMatch {
    pub pieces: [ComboPiece; 2],
    pub relevance: ComboRelevance,
    pub cardinality: ComboCardinality,
    pub assemble_cost: u16,
    pub popularity: u32,
    pub outcomes: BTreeSet<ComboOutcome>,
    pub axes: BTreeSet<BracketAxis>,
    pub source: SignalSource,
}

/// The serialized combo artifact document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComboTableDoc {
    pub provenance: ComboProvenance,
    pub entries: Vec<ComboEntry>,
}

/// A decoded combo artifact with a card-to-entry inverted index.
pub struct ComboTable {
    provenance: Option<ComboProvenance>,
    entries: Vec<ComboEntry>,
    by_card: FxHashMap<String, SmallVec<[u32; 4]>>,
    coverage: ComboCoverage,
}

#[derive(Debug, Error)]
pub enum ComboTableError {
    #[error("combo table JSON is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("combo table row contains the same card twice: {key}")]
    SelfPair { key: String },
    #[error("combo table contains duplicate unordered pair: {first} + {second}")]
    DuplicateEntry { first: String, second: String },
}

impl ComboTable {
    /// Decodes and structurally validates a generated combo artifact.
    pub fn from_json_str(raw: &str) -> Result<Self, ComboTableError> {
        let doc: ComboTableDoc = serde_json::from_str(raw)?;
        validate_entries(&doc.entries)?;
        Ok(Self::from_doc(doc))
    }

    /// Builds a measured table from a trusted generated document.
    pub fn from_doc(doc: ComboTableDoc) -> Self {
        let mut by_card = FxHashMap::<String, SmallVec<[u32; 4]>>::default();
        for (index, entry) in doc.entries.iter().enumerate() {
            let index = u32::try_from(index).expect("combo table contains more than u32::MAX rows");
            for piece in &entry.pieces {
                by_card.entry(piece.key.clone()).or_default().push(index);
            }
        }

        Self {
            provenance: Some(doc.provenance),
            entries: doc.entries,
            by_card,
            coverage: ComboCoverage::Measured,
        }
    }

    /// Returns the artifact provenance, or `None` when no artifact was loaded.
    pub fn provenance(&self) -> Option<&ComboProvenance> {
        self.provenance.as_ref()
    }

    pub fn coverage(&self) -> ComboCoverage {
        self.coverage
    }

    /// Returns entry indices for a key that the caller has already lowercased.
    pub fn entries_for(&self, key: &str) -> &[u32] {
        self.by_card.get(key).map_or(&[], SmallVec::as_slice)
    }

    pub fn entry(&self, index: u32) -> Option<&ComboEntry> {
        usize::try_from(index)
            .ok()
            .and_then(|index| self.entries.get(index))
    }

    /// Returns all rows in artifact order for consumers that must evaluate
    /// table-wide policy without repeatedly walking the inverted index.
    pub fn entries(&self) -> &[ComboEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl Default for ComboTable {
    /// An absent artifact is `Unmeasured`, never a silent zero. Every lookup on
    /// a default table is empty, but consumers can distinguish absence from a
    /// measured artifact containing no matches.
    fn default() -> Self {
        Self {
            provenance: None,
            entries: Vec::new(),
            by_card: FxHashMap::default(),
            coverage: ComboCoverage::Unmeasured,
        }
    }
}

fn validate_entries(entries: &[ComboEntry]) -> Result<(), ComboTableError> {
    let mut pairs = BTreeSet::new();
    for entry in entries {
        let [first, second] = &entry.pieces;
        if first.key == second.key {
            return Err(ComboTableError::SelfPair {
                key: first.key.clone(),
            });
        }

        let pair = if first.key < second.key {
            (first.key.as_str(), second.key.as_str())
        } else {
            (second.key.as_str(), first.key.as_str())
        };
        if !pairs.insert(pair) {
            return Err(ComboTableError::DuplicateEntry {
                first: pair.0.to_owned(),
                second: pair.1.to_owned(),
            });
        }
    }
    Ok(())
}

/// Commander Spellbook's own cardinality reading, as documented by the reference
/// implementation's ADR (Decision 6): `arguable` is seeded with
/// `(notablePrerequisites ? 1 : 0) + (borderlineRelevant ? 0 : 1)`; a library
/// piece is skipped; a piece that IS this deck's commander is skipped; a
/// command-zone piece is arguable; `definitelyTwoCard = sure + arguable <= 2`,
/// `arguablyTwoCard = sure <= 2 && sure + arguable <= 3`.
///
/// UNVERIFIED PREMISE: this is a transcription of the reference's documented
/// reading of Spellbook's `variant.py`. `variant.py` was NOT read in this session.
/// The reference's own ADR flags one reading ("a piece is 'in the library' when
/// its zoneLocations INCLUDES L") as its own, not Spellbook's verbatim.
pub fn combo_cardinality(entry: &ComboEntry, commanders: &BTreeSet<String>) -> ComboCardinality {
    let mut sure = 0_u8;
    let mut arguable = match entry.setup {
        ComboSetup::AsPrinted => 0,
        ComboSetup::NotablePrerequisites => 1,
    } + match entry.relevance {
        ComboRelevance::Standalone => 0,
        ComboRelevance::Helper | ComboRelevance::Contextual => 1,
    };

    for piece in &entry.pieces {
        match piece.zone {
            ComboPieceZone::Library => {}
            ComboPieceZone::CommandZone if commanders.contains(&piece.key) => {}
            ComboPieceZone::CommandZone => arguable += 1,
            ComboPieceZone::Anywhere if commanders.contains(&piece.key) => {}
            ComboPieceZone::Anywhere => sure += 1,
        }
    }

    if sure + arguable <= 2 {
        ComboCardinality::DefinitelyTwoCard
    } else if sure <= 2 && sure + arguable <= 3 {
        ComboCardinality::ArguablyTwoCard
    } else {
        ComboCardinality::More
    }
}

/// Pure and deterministic combo detection over Commander-family deck sections.
pub fn detect_combos(deck: &PlayerDeckList, table: &ComboTable) -> Vec<ComboMatch> {
    // Keep this exhaustive so new deck sections must be explicitly classified.
    let PlayerDeckList {
        commander,
        main_deck,
        companion,
        signature_spell,
        sideboard,
        attraction_deck,
        planar_deck,
        scheme_deck,
        contraption_deck,
        sticker_sheets,
        bracket_tier: _,
        combo_declaration: _,
    } = deck;
    let _ = (
        sideboard,
        attraction_deck,
        planar_deck,
        scheme_deck,
        contraption_deck,
        sticker_sheets,
    );

    let commanders: BTreeSet<String> = commander.iter().map(|name| name.to_lowercase()).collect();
    let deck_keys: BTreeSet<String> = commander
        .iter()
        .chain(main_deck)
        .chain(companion)
        .chain(signature_spell)
        .map(|name| name.to_lowercase())
        .collect();
    let candidate_indices: BTreeSet<u32> = deck_keys
        .iter()
        .flat_map(|key| table.entries_for(key).iter().copied())
        .collect();

    candidate_indices
        .into_iter()
        .filter_map(|index| {
            let entry = table.entry(index)?;
            entry
                .pieces
                .iter()
                .all(|piece| deck_keys.contains(&piece.key))
                .then(|| ComboMatch {
                    pieces: entry.pieces.clone(),
                    relevance: entry.relevance,
                    cardinality: combo_cardinality(entry, &commanders),
                    assemble_cost: entry.assemble_cost,
                    popularity: entry.popularity,
                    outcomes: entry.outcomes.clone(),
                    axes: entry.axes.clone(),
                    source: SignalSource::ComboPair,
                })
        })
        .collect()
}
