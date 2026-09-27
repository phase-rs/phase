//! Pinned, streaming projection of Commander Spellbook bulk data.
//!
//! This is NOT a Comprehensive Rules artifact. Commander bracket policy is WotC
//! Commander Format Panel guidance, so no CR annotation applies here.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{BufReader, Read};
use std::sync::OnceLock;

use engine::database::{
    CardDatabase, ComboEntry, ComboFilterCounts, ComboOmission, ComboOutcome, ComboPiece,
    ComboPieceZone, ComboProvenance, ComboRelevance, ComboResource, ComboSetup, ComboTableDoc,
};
use engine::game::bracket_estimate::BracketAxis;
use flate2::read::GzDecoder;
use regex::Regex;
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const TABLE_VERSION: &str = "1.0.0";
pub const ATTRIBUTION: &str = "Commander Spellbook (commanderspellbook.com)";
/// Measured 1,022,008 B raw on the 2026-09-07 pin; the cap is ~2x that so growth
/// past it is a deliberate act.
pub const DEFAULT_MAX_BYTES: usize = 2_097_152;

/// THE PIN. A re-pin moves every field at once; `build` refuses to write when any
/// of them disagrees with the snapshot it just read.
///
/// NOT a Comprehensive Rules artifact: Commander bracket policy is WotC Commander
/// Format Panel guidance, so no `// CR` annotation applies here.
#[derive(Debug, Clone, Copy)]
pub struct PinnedComboSnapshot {
    /// The snapshot filename, spelled exactly once in the repository.
    pub file: &'static str,
    /// SHA-256 over the DECOMPRESSED bytes this generator actually parses.
    pub sha256: &'static str,
    /// The export's own top-level `timestamp` scalar.
    pub document_timestamp: &'static str,
    /// The export's own top-level generator/builder version scalar.
    pub builder: &'static str,
    /// Expected element count in `variants[]`.
    pub variant_count: u32,
}

pub const PINNED_SNAPSHOT: PinnedComboSnapshot = PinnedComboSnapshot {
    file: "variants-2026-09-07.json.gz",
    sha256: "9ca1faa56c0ab9362e195f83fefececf3272935267ad885c81b9669cf28d2f7c",
    document_timestamp: "2026-09-07T21:09:01.925143+00:00",
    builder: "6.3.3",
    variant_count: 108_535,
};

#[derive(Debug, Clone)]
pub struct BuildLimits {
    pub max_bytes: usize,
    /// The CLI supplies the first 12 hex digits of the pool file's SHA-256.
    /// Library callers without source bytes receive a deterministic face-index fingerprint.
    pub card_pool_version: Option<String>,
}

impl Default for BuildLimits {
    fn default() -> Self {
        Self {
            max_bytes: DEFAULT_MAX_BYTES,
            card_pool_version: None,
        }
    }
}

#[derive(Debug, Error)]
pub enum GenError {
    #[error("snapshot digest mismatch: pinned {pinned}, read {actual}")]
    DigestMismatch { pinned: String, actual: String },
    #[error("snapshot timestamp mismatch: pinned {pinned}, read {actual}")]
    TimestampMismatch { pinned: String, actual: String },
    #[error("snapshot builder mismatch: pinned {pinned}, read {actual}")]
    BuilderMismatch { pinned: String, actual: String },
    #[error("variant count mismatch: pinned {pinned}, read {actual}")]
    VariantCountMismatch { pinned: u32, actual: u32 },
    #[error("card pool is empty — run ./scripts/gen-card-data.sh first")]
    EmptyPool,
    #[error("row {row} carries a string that is not a resolvable card name: {value:?}")]
    NonFactString { row: usize, value: String },
    #[error("projected artifact is {bytes} bytes, exceeding the {max_bytes}-byte limit")]
    TooLarge { bytes: usize, max_bytes: usize },
    #[error("row {row} has an assemble cost that does not fit in u16")]
    AssembleCostOverflow { row: usize },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

/// A pinned classifier rule. Rules are evaluated independently because one feature may
/// contribute more than one typed result.
#[derive(Debug, Clone, Copy)]
pub struct FeatureClassifierPattern {
    pub class: FeatureClass,
    pub pattern: &'static str,
    pub excluded_pattern: Option<&'static str>,
}

/// Commander Spellbook's feature-name classifier, pinned with the snapshot.
pub const FEATURE_CLASSIFIER_PATTERNS: &[FeatureClassifierPattern] = &[
    FeatureClassifierPattern {
        class: FeatureClass::MassLandDenial,
        pattern: r"(?i)mass land (?:destruction|denial|removal)",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteTurns,
        pattern: r"(?i)(?:near-)?infinite (?:extra )?turns?",
        excluded_pattern: Some(r"(?i)(?:near-)?infinite (?:extra )?turns? for .* opponent"),
    },
    FeatureClassifierPattern {
        class: FeatureClass::Lock,
        pattern: r"(?i)lock",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::SkipTurns,
        pattern: r"(?i)(?:infinite(?:ly)? )?skip (?:(?:all )?(?:your |their )|infinite )?(?:future )?turns?",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::ControlAllOpponents,
        pattern: r"(?i)you control (?:your|(?:up to )?three) opponents",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::ControlSomeOpponents,
        pattern: r"(?i)you control (?:one|an|(?:up to )?two) opponents",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::Win,
        pattern: r"(?i)\bwins? the game\b|\bloses? the game\b|lose the game",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteDamage,
        pattern: r"(?i)infinite .*damage",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteLifeLoss,
        pattern: r"(?i)infinite .*(?:life ?loss|lifedrain|life drain|drain|loses? .*life)",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteMill,
        pattern: r"(?i)infinite.*mill",
        excluded_pattern: Some(r"(?i)self"),
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteMana,
        pattern: r"(?i)infinite .*mana",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteCombat,
        pattern: r"(?i)infinite combat",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteDraw,
        pattern: r"(?i)infinite card draw",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteLifegain,
        pattern: r"(?i)infinite life ?gain",
        excluded_pattern: None,
    },
    FeatureClassifierPattern {
        class: FeatureClass::InfiniteTokens,
        pattern: r"(?i)infinite (?:creature )?tokens?",
        excluded_pattern: None,
    },
];

// The reference documents these as false positives: unlocking Rooms is not a lock,
// inability to lose at zero life is not a win, and creature-only damage is not player damage.
pub const EXCLUDED_FEATURE_NAMES: &[(FeatureClass, &str)] = &[
    (
        FeatureClass::Lock,
        "Infinite unlocking of Rooms you control",
    ),
    (
        FeatureClass::Win,
        "You can't lose the game due to having 0 or less life",
    ),
    (FeatureClass::InfiniteDamage, "Infinite damage to creatures"),
];

#[derive(Debug, Deserialize)]
struct Variant {
    #[serde(default)]
    status: String,
    #[serde(default)]
    legalities: Legalities,
    #[serde(default)]
    requires: Vec<IgnoredAny>,
    #[serde(default)]
    uses: Vec<Use>,
    #[serde(default)]
    produces: Vec<Produce>,
    #[serde(rename = "manaValueNeeded", default)]
    mana_value_needed: u64,
    #[serde(default)]
    popularity: u32,
    #[serde(rename = "notablePrerequisites", default)]
    notable_prerequisites: String,
}

#[derive(Debug, Default, Deserialize)]
struct Legalities {
    #[serde(default)]
    commander: bool,
}

#[derive(Debug, Deserialize)]
struct Use {
    card: ExternalCard,
    #[serde(rename = "zoneLocations", default)]
    zone_locations: Vec<String>,
    #[serde(rename = "mustBeCommander", default)]
    must_be_commander: bool,
}

#[derive(Debug, Deserialize)]
struct ExternalCard {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Produce {
    feature: Feature,
}

#[derive(Debug, Deserialize)]
struct Feature {
    name: String,
    #[serde(default)]
    status: String,
}

#[derive(Debug)]
struct Candidate {
    entry: ComboEntry,
    position: u32,
}

#[derive(Default)]
struct BuildState {
    timestamp: Option<String>,
    builder: Option<String>,
    variant_count: u32,
    filtered: ComboFilterCounts,
    candidates: Vec<Candidate>,
    fatal: Option<GenError>,
}

struct HashingReader<R> {
    inner: R,
    hasher: Sha256,
}

impl<R> HashingReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    fn digest_hex(&self) -> String {
        format!("{:x}", self.hasher.clone().finalize())
    }
}

impl<R: Read> Read for HashingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buf)?;
        self.hasher.update(&buf[..read]);
        Ok(read)
    }
}

struct DocumentSeed<'a> {
    state: &'a mut BuildState,
    pool: &'a CardDatabase,
}

impl<'de> DeserializeSeed<'de> for DocumentSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(DocumentVisitor {
            state: self.state,
            pool: self.pool,
        })
    }
}

struct DocumentVisitor<'a> {
    state: &'a mut BuildState,
    pool: &'a CardDatabase,
}

impl<'de> Visitor<'de> for DocumentVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a Commander Spellbook bulk-export object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "timestamp" => self.state.timestamp = Some(map.next_value()?),
                "version" => self.state.builder = Some(map.next_value()?),
                "variants" => map.next_value_seed(VariantsSeed {
                    state: self.state,
                    pool: self.pool,
                })?,
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

struct VariantsSeed<'a> {
    state: &'a mut BuildState,
    pool: &'a CardDatabase,
}

impl<'de> DeserializeSeed<'de> for VariantsSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(VariantsVisitor {
            state: self.state,
            pool: self.pool,
        })
    }
}

struct VariantsVisitor<'a> {
    state: &'a mut BuildState,
    pool: &'a CardDatabase,
}

impl<'de> Visitor<'de> for VariantsVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("the variants array")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(variant) = seq.next_element::<Variant>()? {
            let position = self.state.variant_count;
            self.state.variant_count = self.state.variant_count.saturating_add(1);
            if self.state.fatal.is_none() {
                if let Err(error) = project_variant(variant, position, self.pool, self.state) {
                    self.state.fatal = Some(error);
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureClass {
    /// Reference class `massLandDenial`.
    MassLandDenial,
    /// Reference class `infiniteTurns`.
    InfiniteTurns,
    /// Reference class `lock`.
    Lock,
    /// Reference class `skipTurns`.
    SkipTurns,
    /// Reference class `controlAllOpponents`.
    ControlAllOpponents,
    /// Reference class `controlSomeOpponents`.
    ControlSomeOpponents,
    /// Reference class `win`.
    Win,
    /// Reference class `infiniteDamage`.
    InfiniteDamage,
    /// Reference class `infiniteLifeLoss`.
    InfiniteLifeLoss,
    /// Reference class `infiniteMill`.
    InfiniteMill,
    /// Reference class `infiniteMana`.
    InfiniteMana,
    /// Reference class `infiniteCombat`.
    InfiniteCombat,
    /// Reference class `infiniteDraw`.
    InfiniteDraw,
    /// Reference class `infiniteLifegain`.
    InfiniteLifegain,
    /// Reference class `infiniteTokens`.
    InfiniteTokens,
}

struct CompiledPattern {
    class: FeatureClass,
    include: Regex,
    exclude: Option<Regex>,
}

fn classifier() -> &'static [CompiledPattern] {
    static CLASSIFIER: OnceLock<Vec<CompiledPattern>> = OnceLock::new();
    CLASSIFIER.get_or_init(|| {
        FEATURE_CLASSIFIER_PATTERNS
            .iter()
            .map(|rule| CompiledPattern {
                class: rule.class,
                include: Regex::new(rule.pattern).expect("pinned classifier regex must compile"),
                exclude: rule.excluded_pattern.map(|pattern| {
                    Regex::new(pattern).expect("pinned classifier exclusion regex must compile")
                }),
            })
            .collect()
    })
}

fn classify_feature(name: &str) -> impl Iterator<Item = FeatureClass> + '_ {
    classifier().iter().filter_map(move |rule| {
        let exact_exclusion = EXCLUDED_FEATURE_NAMES
            .iter()
            .any(|(class, excluded)| *class == rule.class && *excluded == name);
        (rule.include.is_match(name)
            && !rule
                .exclude
                .as_ref()
                .is_some_and(|regex| regex.is_match(name))
            && !exact_exclusion)
            .then_some(rule.class)
    })
}

fn project_variant(
    variant: Variant,
    position: u32,
    pool: &CardDatabase,
    state: &mut BuildState,
) -> Result<(), GenError> {
    if !variant.legalities.commander {
        state.filtered.not_commander_legal += 1;
        return Ok(());
    }
    if variant.status != "OK" {
        state.filtered.not_ok += 1;
        return Ok(());
    }
    if !variant.requires.is_empty() {
        state.filtered.template += 1;
        return Ok(());
    }
    match variant.uses.len() {
        2 => {}
        0 | 1 => {
            state.filtered.one_card += 1;
            return Ok(());
        }
        _ => {
            state.filtered.three_or_more += 1;
            return Ok(());
        }
    }

    let mut pieces = Vec::with_capacity(2);
    let mut piece_mana_values = Vec::with_capacity(2);
    for use_row in variant.uses {
        let Some(face) = pool.get_face_by_name(&use_row.card.name) else {
            state.filtered.unknown_card += 1;
            return Ok(());
        };
        if use_row.zone_locations.len() > 1 {
            state.filtered.multi_zone_pieces += 1;
        }
        let zone = if use_row.zone_locations.iter().any(|zone| zone == "L") {
            ComboPieceZone::Library
        } else if use_row.must_be_commander || use_row.zone_locations.iter().any(|zone| zone == "C")
        {
            ComboPieceZone::CommandZone
        } else {
            ComboPieceZone::Anywhere
        };
        pieces.push(ComboPiece {
            key: face.name.to_lowercase(),
            display: face.name.clone(),
            zone,
        });
        piece_mana_values.push(face.mana_cost.mana_value());
    }

    let relevance = if variant.produces.iter().any(|row| row.feature.status == "S") {
        ComboRelevance::Standalone
    } else if variant.produces.iter().any(|row| row.feature.status == "C") {
        ComboRelevance::Contextual
    } else {
        ComboRelevance::Helper
    };
    let mut outcomes = BTreeSet::new();
    let mut axes = BTreeSet::new();
    for feature in &variant.produces {
        for class in classify_feature(&feature.feature.name) {
            match class {
                FeatureClass::MassLandDenial => {
                    axes.insert(BracketAxis::MassLandDenial);
                }
                FeatureClass::InfiniteTurns => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Turns));
                }
                FeatureClass::Win => {
                    outcomes.insert(ComboOutcome::Wins);
                }
                FeatureClass::InfiniteDamage => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Damage));
                }
                FeatureClass::InfiniteLifeLoss => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::LifeLoss));
                }
                FeatureClass::InfiniteMill => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Mill));
                }
                FeatureClass::InfiniteMana => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Mana));
                }
                FeatureClass::InfiniteCombat => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Combat));
                }
                FeatureClass::InfiniteDraw => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Draw));
                }
                FeatureClass::InfiniteLifegain => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Lifegain));
                }
                FeatureClass::InfiniteTokens => {
                    outcomes.insert(ComboOutcome::Unbounded(ComboResource::Tokens));
                }
                FeatureClass::Lock
                | FeatureClass::SkipTurns
                | FeatureClass::ControlAllOpponents
                | FeatureClass::ControlSomeOpponents => {}
            }
        }
    }
    if relevance < ComboRelevance::Contextual && !axes.contains(&BracketAxis::MassLandDenial) {
        state.filtered.irrelevant += 1;
        return Ok(());
    }

    let mana_value_needed =
        u16::try_from(variant.mana_value_needed).map_err(|_| GenError::AssembleCostOverflow {
            row: position as usize,
        })?;
    let assemble_u32 = u32::from(mana_value_needed)
        .checked_add(piece_mana_values[0])
        .and_then(|sum| sum.checked_add(piece_mana_values[1]))
        .ok_or(GenError::AssembleCostOverflow {
            row: position as usize,
        })?;
    let assemble_cost =
        u16::try_from(assemble_u32).map_err(|_| GenError::AssembleCostOverflow {
            row: position as usize,
        })?;
    pieces.sort_by(|left, right| left.key.cmp(&right.key));
    let pieces: [ComboPiece; 2] = pieces
        .try_into()
        .expect("exactly two pieces passed the filter");
    state.candidates.push(Candidate {
        entry: ComboEntry {
            pieces,
            relevance,
            setup: if variant.notable_prerequisites.trim().is_empty() {
                ComboSetup::AsPrinted
            } else {
                ComboSetup::NotablePrerequisites
            },
            mana_value_needed,
            assemble_cost,
            popularity: variant.popularity,
            outcomes,
            axes,
        },
        position,
    });
    Ok(())
}

fn candidate_is_better(candidate: &Candidate, incumbent: &Candidate) -> bool {
    candidate
        .entry
        .relevance
        .cmp(&incumbent.entry.relevance)
        .then_with(|| {
            incumbent
                .entry
                .assemble_cost
                .cmp(&candidate.entry.assemble_cost)
        })
        .then_with(|| candidate.entry.popularity.cmp(&incumbent.entry.popularity))
        .then_with(|| incumbent.position.cmp(&candidate.position))
        == Ordering::Greater
}

fn deduplicate(state: &mut BuildState) -> Vec<ComboEntry> {
    let mut by_pair: BTreeMap<(String, String), Candidate> = BTreeMap::new();
    for candidate in state.candidates.drain(..) {
        let pair = (
            candidate.entry.pieces[0].key.clone(),
            candidate.entry.pieces[1].key.clone(),
        );
        match by_pair.entry(pair) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(candidate);
            }
            std::collections::btree_map::Entry::Occupied(mut slot) => {
                state.filtered.duplicate_pair += 1;
                if candidate_is_better(&candidate, slot.get()) {
                    slot.insert(candidate);
                }
            }
        }
    }
    let entries: Vec<_> = by_pair
        .into_values()
        .map(|candidate| candidate.entry)
        .collect();
    state.filtered.kept = u32::try_from(entries.len()).unwrap_or(u32::MAX);
    entries
}

fn fallback_pool_version(pool: &CardDatabase) -> String {
    let mut rows: Vec<_> = pool
        .faces_in_scan_order()
        .map(|face| (&face.name, face.mana_cost.mana_value()))
        .collect();
    rows.sort_unstable();
    let mut hasher = Sha256::new();
    for (name, mana_value) in rows {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(mana_value.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())[..12].to_owned()
}

/// Streams `variants[]`, projects bounded rows, and returns nothing unless all gates pass.
pub fn build_combo_table(
    src: impl Read,
    pool: &CardDatabase,
    pin: &PinnedComboSnapshot,
    limits: &BuildLimits,
) -> Result<ComboTableDoc, GenError> {
    if pool.faces_in_scan_order().next().is_none() {
        return Err(GenError::EmptyPool);
    }
    let decoder = GzDecoder::new(BufReader::new(src));
    let hashing_reader = HashingReader::new(decoder);
    let mut buffered_reader = BufReader::new(hashing_reader);
    let mut state = BuildState::default();
    {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut buffered_reader);
        DocumentSeed {
            state: &mut state,
            pool,
        }
        .deserialize(&mut deserializer)?;
        deserializer.end()?;
    }
    let hashing_reader = buffered_reader.into_inner();
    let actual_digest = hashing_reader.digest_hex();
    if actual_digest != pin.sha256 {
        return Err(GenError::DigestMismatch {
            pinned: pin.sha256.to_owned(),
            actual: actual_digest,
        });
    }
    let actual_timestamp = state.timestamp.take().unwrap_or_default();
    if actual_timestamp != pin.document_timestamp {
        return Err(GenError::TimestampMismatch {
            pinned: pin.document_timestamp.to_owned(),
            actual: actual_timestamp,
        });
    }
    let actual_builder = state.builder.take().unwrap_or_default();
    if actual_builder != pin.builder {
        return Err(GenError::BuilderMismatch {
            pinned: pin.builder.to_owned(),
            actual: actual_builder,
        });
    }
    if state.variant_count != pin.variant_count {
        return Err(GenError::VariantCountMismatch {
            pinned: pin.variant_count,
            actual: state.variant_count,
        });
    }
    if let Some(error) = state.fatal.take() {
        return Err(error);
    }
    let entries = deduplicate(&mut state);
    let snapshot_date = pin
        .document_timestamp
        .split_once('T')
        .map_or(pin.document_timestamp, |(date, _)| date)
        .to_owned();
    let doc = ComboTableDoc {
        provenance: ComboProvenance {
            snapshot_date,
            table_version: TABLE_VERSION.to_owned(),
            attribution: ATTRIBUTION.to_owned(),
            card_pool_version: limits
                .card_pool_version
                .clone()
                .unwrap_or_else(|| fallback_pool_version(pool)),
            filtered: state.filtered,
            omitted: vec![
                ComboOmission::PrerequisiteText,
                ComboOmission::ResultText,
                ComboOmission::UnmodeledResultClasses,
            ],
        },
        entries,
    };
    assert_facts_only(&doc, pool)?;
    let bytes = serde_json::to_vec(&doc)?.len();
    if bytes > limits.max_bytes {
        return Err(GenError::TooLarge {
            bytes,
            max_bytes: limits.max_bytes,
        });
    }
    Ok(doc)
}

/// Secondary serialized-document gate; the struct shape is the primary facts-only guarantee.
pub fn assert_facts_only(doc: &ComboTableDoc, pool: &CardDatabase) -> Result<(), GenError> {
    let value = serde_json::to_value(doc)?;
    let fixed: BTreeSet<&str> = [
        doc.provenance.snapshot_date.as_str(),
        doc.provenance.table_version.as_str(),
        doc.provenance.attribution.as_str(),
        doc.provenance.card_pool_version.as_str(),
        "prerequisite_text",
        "result_text",
        "unmodeled_result_classes",
        "helper",
        "contextual",
        "standalone",
        "as_printed",
        "notable_prerequisites",
        "library",
        "command_zone",
        "anywhere",
        "wins",
        "unbounded",
        "mana",
        "damage",
        "life_loss",
        "lifegain",
        "mill",
        "draw",
        "tokens",
        "combat",
        "turns",
        "mass_land_denial",
    ]
    .into_iter()
    .collect();
    let mut strings = Vec::new();
    collect_json_strings(&value, &mut strings);
    for (row, string) in strings.into_iter().enumerate() {
        if fixed.contains(string.as_str()) {
            continue;
        }
        let Some(face) = pool.get_face_by_name(&string) else {
            return Err(GenError::NonFactString { row, value: string });
        };
        if string != face.name && string != face.name.to_lowercase() {
            return Err(GenError::NonFactString { row, value: string });
        }
    }
    Ok(())
}

fn collect_json_strings(value: &serde_json::Value, strings: &mut Vec<String>) {
    match value {
        serde_json::Value::String(string) => strings.push(string.clone()),
        serde_json::Value::Array(values) => values
            .iter()
            .for_each(|value| collect_json_strings(value, strings)),
        serde_json::Value::Object(values) => values
            .values()
            .for_each(|value| collect_json_strings(value, strings)),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

pub fn sha256_file_prefix(path: &std::path::Path) -> Result<String, std::io::Error> {
    let mut file = BufReader::new(std::fs::File::open(path)?);
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut HashWriter(&mut hasher))?;
    Ok(format!("{:x}", hasher.finalize())[..12].to_owned())
}

struct HashWriter<'a>(&'a mut Sha256);

impl std::io::Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
