//! Offline labelled-deck corpus tooling for the Commander bracket estimator.
//!
//! Bracket policy is not the Comprehensive Rules, so this module carries no
//! rules annotations. Card names belong only in dated fixture data, never in
//! this file; the implementation and its synthetic tests contain no card names.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use strum::IntoEnumIterator;
use thiserror::Error;

use crate::database::set_catalog::ReleaseDate;
use crate::database::{CardDatabase, ComboTable};
use crate::game::bracket_estimate::{
    estimate_bracket, BracketAxis, BracketCheckOutcome, BracketEstimate, CommanderBracketTier,
};
use crate::game::deck_loading::PlayerDeckList;

/// Where a corpus label came from. This is the only axis that decides whether
/// a row is designed rather than population evidence.
///
/// The publishing site, URL, and terms-check date are data on [`CorpusSource`],
/// not sibling enum variants. There is deliberately no preconstructed-deck
/// variant: WotC decoupled preconstructed decks from Bracket 2 on 2025-10-21.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LabelBasis {
    RulesDerivation,
    OwnerDeclared,
    ThirdPartyPublished,
}

impl LabelBasis {
    /// Designed rows agree with the policy derivation by construction and are
    /// therefore excluded from every population rate.
    pub fn is_designed(self) -> bool {
        matches!(self, Self::RulesDerivation)
    }
}

/// A caveat serious enough to exclude a captured label from population rates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusDispute {
    pub raised_on: ReleaseDate,
    pub reason: String,
}

/// The human-labelled expected bracket and axis evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusLabel {
    pub tier: CommanderBracketTier,
    pub basis: LabelBasis,
    pub labeller: String,
    pub labelled_on: ReleaseDate,
    pub rules_copy_read: String,
    pub axis_counts: BTreeMap<BracketAxis, u8>,
    pub cards_named: BTreeMap<BracketAxis, BTreeSet<String>>,
    pub caveat: Option<String>,
    pub disputed: Option<CorpusDispute>,
}

/// Provenance for the captured deck list.
///
/// Legitimate labelled lists are repo-owned, consented user submissions, or
/// individually captured third-party publications whose terms were checked.
/// This contract is intentionally unsuitable for scraped or bulk-copied data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusSource {
    pub url: Option<String>,
    pub captured_on: ReleaseDate,
    pub terms_checked_on: Option<ReleaseDate>,
    pub capture_note: String,
}

/// One validated labelled-deck fixture.
#[derive(Debug, Clone)]
pub struct CorpusFixture {
    pub id: String,
    pub name: String,
    pub label: CorpusLabel,
    pub source: CorpusSource,
    pub decklist: PlayerDeckList,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCorpusDispute {
    raised_on: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCorpusLabel {
    tier: CommanderBracketTier,
    basis: LabelBasis,
    labeller: String,
    labelled_on: String,
    rules_copy_read: String,
    axis_counts: BTreeMap<BracketAxis, u8>,
    cards_named: BTreeMap<BracketAxis, BTreeSet<String>>,
    #[serde(default)]
    caveat: Option<String>,
    #[serde(default)]
    disputed: Option<RawCorpusDispute>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCorpusSource {
    #[serde(default)]
    url: Option<String>,
    captured_on: String,
    #[serde(default)]
    terms_checked_on: Option<String>,
    capture_note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCorpusFixture {
    id: String,
    name: String,
    label: RawCorpusLabel,
    source: RawCorpusSource,
    decklist: PlayerDeckList,
}

/// Validation failures for one fixture, including its raw JSON shape.
#[derive(Debug, Error)]
pub enum CorpusFixtureError {
    #[error("invalid fixture JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{field} must not be empty")]
    EmptyField { field: &'static str },
    #[error("{field} is not a valid YYYY-MM-DD date: {value}")]
    InvalidDate { field: &'static str, value: String },
    #[error("third_party_published labels require source.url and source.terms_checked_on")]
    ThirdPartyTermsRequired,
    #[error("source.terms_checked_on is required whenever source.url is present")]
    UrlTermsRequired,
    #[error("decklist.commander must contain at least one entry")]
    EmptyCommander,
}

fn parse_date(field: &'static str, value: String) -> Result<ReleaseDate, CorpusFixtureError> {
    ReleaseDate::parse(&value).ok_or(CorpusFixtureError::InvalidDate { field, value })
}

impl TryFrom<RawCorpusFixture> for CorpusFixture {
    type Error = CorpusFixtureError;

    fn try_from(raw: RawCorpusFixture) -> Result<Self, Self::Error> {
        if raw.id.trim().is_empty() {
            return Err(CorpusFixtureError::EmptyField { field: "id" });
        }
        if raw.label.labeller.trim().is_empty() {
            return Err(CorpusFixtureError::EmptyField { field: "labeller" });
        }
        if raw.source.capture_note.trim().is_empty() {
            return Err(CorpusFixtureError::EmptyField {
                field: "capture_note",
            });
        }
        if raw.decklist.commander.is_empty() {
            return Err(CorpusFixtureError::EmptyCommander);
        }
        if raw.source.url.is_some() && raw.source.terms_checked_on.is_none() {
            return Err(CorpusFixtureError::UrlTermsRequired);
        }
        if raw.label.basis == LabelBasis::ThirdPartyPublished
            && (raw.source.url.is_none() || raw.source.terms_checked_on.is_none())
        {
            return Err(CorpusFixtureError::ThirdPartyTermsRequired);
        }

        let disputed = raw
            .label
            .disputed
            .map(|dispute| -> Result<CorpusDispute, CorpusFixtureError> {
                Ok(CorpusDispute {
                    raised_on: parse_date("label.disputed.raised_on", dispute.raised_on)?,
                    reason: dispute.reason,
                })
            })
            .transpose()?;
        let cards_named = raw
            .label
            .cards_named
            .into_iter()
            .map(|(axis, names)| {
                let names = names.into_iter().map(|name| name.to_lowercase()).collect();
                (axis, names)
            })
            .collect();
        let terms_checked_on = raw
            .source
            .terms_checked_on
            .map(|date| parse_date("source.terms_checked_on", date))
            .transpose()?;

        Ok(Self {
            id: raw.id,
            name: raw.name,
            label: CorpusLabel {
                tier: raw.label.tier,
                basis: raw.label.basis,
                labeller: raw.label.labeller,
                labelled_on: parse_date("label.labelled_on", raw.label.labelled_on)?,
                rules_copy_read: raw.label.rules_copy_read,
                axis_counts: raw.label.axis_counts,
                cards_named,
                caveat: raw.label.caveat,
                disputed,
            },
            source: CorpusSource {
                url: raw.source.url,
                captured_on: parse_date("source.captured_on", raw.source.captured_on)?,
                terms_checked_on,
                capture_note: raw.source.capture_note,
            },
            decklist: raw.decklist,
        })
    }
}

/// Parse and validate one fixture without touching the filesystem.
pub fn parse_corpus_fixture(json: &str) -> Result<CorpusFixture, CorpusFixtureError> {
    serde_json::from_str::<RawCorpusFixture>(json)?.try_into()
}

/// Directory-level loading failures.
#[derive(Debug, Error)]
pub enum CorpusLoadError {
    #[error("failed to read corpus directory {path}: {source}")]
    ReadDirectory {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read corpus fixture {path}: {source}")]
    ReadFixture {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid corpus fixture {path}: {source}")]
    InvalidFixture {
        path: PathBuf,
        #[source]
        source: CorpusFixtureError,
    },
    #[error("duplicate corpus fixture id {id:?} in {path}")]
    DuplicateId { id: String, path: PathBuf },
}

/// Load every direct-child `*.json` fixture except `expectations.json`, in
/// filename order, validating each row and rejecting duplicate ids.
pub fn load_corpus_dir(dir: &Path) -> Result<Vec<CorpusFixture>, CorpusLoadError> {
    let entries = fs::read_dir(dir).map_err(|source| CorpusLoadError::ReadDirectory {
        path: dir.to_path_buf(),
        source,
    })?;
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| CorpusLoadError::ReadDirectory {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
            && path
                .file_name()
                .is_some_and(|name| name != "expectations.json")
        {
            paths.push(path);
        }
    }
    paths.sort_by(|left, right| left.file_name().cmp(&right.file_name()));

    let mut ids = BTreeSet::new();
    let mut fixtures = Vec::with_capacity(paths.len());
    for path in paths {
        let json = fs::read_to_string(&path).map_err(|source| CorpusLoadError::ReadFixture {
            path: path.clone(),
            source,
        })?;
        let fixture =
            parse_corpus_fixture(&json).map_err(|source| CorpusLoadError::InvalidFixture {
                path: path.clone(),
                source,
            })?;
        if !ids.insert(fixture.id.clone()) {
            return Err(CorpusLoadError::DuplicateId {
                id: fixture.id,
                path,
            });
        }
        fixtures.push(fixture);
    }
    Ok(fixtures)
}

/// Find fixture card names the database cannot resolve, sorted and deduplicated.
///
/// Unknown names otherwise look exactly like genuinely clean cards because the
/// database returns empty bracket signals for both. Resolution deliberately uses
/// `CardDatabase::get_face_by_name`, the same lookup authority as the estimator.
pub fn unresolved_names(fixture: &CorpusFixture, db: &CardDatabase) -> Vec<String> {
    let mut missing: Vec<String> = fixture
        .decklist
        .commander
        .iter()
        .chain(fixture.decklist.main_deck.iter())
        .filter(|name| db.get_face_by_name(name).is_none())
        .cloned()
        .collect();
    missing.sort();
    missing.dedup();
    missing
}

pub const HELD_OUT_MODULUS: usize = 4;
pub const HELD_OUT_RESIDUE: usize = 3;
/// After designed rows are removed and remaining ids sorted, a row is held out
/// when its zero-based index modulo 4 equals 3.
pub const HELD_OUT_RULE_SENTENCE: &str = "After designed rows are removed and remaining ids sorted, a row is held out when its zero-based index modulo 4 equals 3.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CorpusSplit {
    Train,
    HeldOut,
    Designed,
}

/// Assign deterministic splits from ids and label basis.
///
/// Sorting makes input order irrelevant, and appending an id after all existing
/// ids leaves every earlier split unchanged. Inserting an id in the middle of
/// the sorted order can shift later rows; this index-modulo scheme does not
/// promise stability for middle insertions. Designed rows never occupy a slot.
pub fn held_out_split(fixtures: &[CorpusFixture]) -> BTreeMap<String, CorpusSplit> {
    let mut result = BTreeMap::new();
    let mut population_ids = Vec::new();
    for fixture in fixtures {
        if fixture.label.basis.is_designed() {
            result.insert(fixture.id.clone(), CorpusSplit::Designed);
        } else {
            population_ids.push(fixture.id.clone());
        }
    }
    population_ids.sort();
    for (index, id) in population_ids.into_iter().enumerate() {
        let split = if index % HELD_OUT_MODULUS == HELD_OUT_RESIDUE {
            CorpusSplit::HeldOut
        } else {
            CorpusSplit::Train
        };
        result.insert(id, split);
    }
    result
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FiredState {
    Fired,
    NotFired,
}

/// The estimator output reduced to exactly what the corpus grades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusReading {
    pub tier: CommanderBracketTier,
    pub observed: BTreeMap<BracketAxis, u8>,
    pub evidence: BTreeMap<BracketAxis, BTreeSet<String>>,
    pub fired: BTreeMap<(BracketAxis, u8), FiredState>,
}

/// Adapt an estimator result into a corpus reading.
///
/// This is the only function in the corpus module that reads
/// [`BracketEstimate`]'s shape. Estimator output changes are isolated here.
pub fn reading_from_estimate(estimate: &BracketEstimate) -> CorpusReading {
    let observed = estimate
        .axes
        .iter()
        .map(|(axis, reading)| (*axis, reading.count))
        .collect();
    let evidence = estimate
        .axes
        .iter()
        .map(|(axis, reading)| {
            (
                *axis,
                reading
                    .contributing
                    .iter()
                    .map(|name| name.to_lowercase())
                    .collect(),
            )
        })
        .collect();
    let fired = estimate
        .checks
        .iter()
        .map(|check| {
            let state = match check.outcome {
                BracketCheckOutcome::Clear { .. } => FiredState::NotFired,
                BracketCheckOutcome::Fired => FiredState::Fired,
            };
            ((check.axis, check.threshold), state)
        })
        .collect();
    CorpusReading {
        tier: estimate.tier,
        observed,
        evidence,
        fired,
    }
}

/// Which policy axes may gate corpus results rather than merely report them.
///
/// Game Changers is armed because it comes from WotC's authoritative dated
/// list. Mass Land Denial is reported because its names are assembled from the
/// separately dated `mass_land_sweepers` and `mass_mana_denial` lists in
/// `data/bracket_lists.json` version `2026-02-09-wotc`; completeness is what the
/// corpus measures. Extra Turns is likewise curated and reported. Efficient
/// Tutors remains evidence-only because WotC retired that restriction.
pub const ARMED_RULES: &[BracketAxis] = &[BracketAxis::GameChangers];

/// An exact rate. Floating point is exposed only by [`Self::as_f64`] for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CorpusFraction {
    pub numerator: u32,
    pub denominator: u32,
}

impl CorpusFraction {
    /// Convert to a display-only float, or `None` for a zero denominator.
    pub fn as_f64(self) -> Option<f64> {
        (self.denominator != 0).then(|| f64::from(self.numerator) / f64::from(self.denominator))
    }

    /// Exact `self >= other` comparison by `u64` cross multiplication.
    /// Returns false if either fraction has a zero denominator.
    pub fn at_least(self, other: Self) -> bool {
        self.denominator != 0
            && other.denominator != 0
            && u64::from(self.numerator) * u64::from(other.denominator)
                >= u64::from(other.numerator) * u64::from(self.denominator)
    }
}

/// Rule of three: with zero observed errors in n trials, the 95% upper bound on
/// the error rate is 3/n, so n = 60 bounds a per-axis error at at most 5%.
pub const MIN_AXIS_POPULATION_N: usize = 60;
/// Band agreement over four classes resolves to roughly +/- 10pp at n = 100.
/// Below this, the fraction is reported with its denominator and gates nothing.
pub const MIN_BAND_POPULATION_N: usize = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateRegime {
    ReportOnly { n: usize, min_n: usize },
    Gated { n: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorpusRuleStatus {
    Armed,
    Reported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AxisMiss {
    Count { labelled: u8, observed: u8 },
    MissingEvidence { names: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusMiss {
    pub id: String,
    pub axis: BracketAxis,
    pub miss: AxisMiss,
}

/// A tier disagreement, kept separate from axis count/evidence misses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BandMiss {
    pub id: String,
    pub labelled: CommanderBracketTier,
    pub observed: CommanderBracketTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AxisScore {
    pub axis: BracketAxis,
    pub status: CorpusRuleStatus,
    pub agreement: CorpusFraction,
    pub regime: GateRegime,
    /// Train + held-out + designed equals the total number of erroneous rows.
    pub errors_by_split: BTreeMap<CorpusSplit, u32>,
    /// Observed count to `(fired, not_fired)`, across every floor-rule row.
    pub fired_by_count: BTreeMap<u8, (u32, u32)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorpusScore {
    pub band_agreement: CorpusFraction,
    pub band_regime: GateRegime,
    pub per_axis: BTreeMap<BracketAxis, AxisScore>,
    /// `(label tier, engine tier)` as numeric B1..=B5 keys. The tier enum does
    /// not implement `Ord`, and this report must not widen its shared contract.
    pub confusion: BTreeMap<(u8, u8), u32>,
    pub misses: Vec<CorpusMiss>,
    pub band_misses: Vec<BandMiss>,
    pub held_out_ids: Vec<String>,
    pub excluded_from_population: Vec<String>,
    pub disputed_ids: Vec<String>,
}

fn regime(n: usize, min_n: usize) -> GateRegime {
    if n >= min_n {
        GateRegime::Gated { n }
    } else {
        GateRegime::ReportOnly { n, min_n }
    }
}

fn empty_error_splits() -> BTreeMap<CorpusSplit, u32> {
    [
        (CorpusSplit::Train, 0),
        (CorpusSplit::HeldOut, 0),
        (CorpusSplit::Designed, 0),
    ]
    .into_iter()
    .collect()
}

/// Score all fixtures while excluding designed and disputed rows from rates.
/// Miss reports and fired distributions still include every row.
pub fn score_corpus(fixtures: &[CorpusFixture], db: &CardDatabase) -> CorpusScore {
    struct AxisAccumulator {
        agreements: u32,
        errors_by_split: BTreeMap<CorpusSplit, u32>,
        fired_by_count: BTreeMap<u8, (u32, u32)>,
    }

    let splits = held_out_split(fixtures);
    let mut accumulators: BTreeMap<BracketAxis, AxisAccumulator> = BracketAxis::iter()
        .map(|axis| {
            (
                axis,
                AxisAccumulator {
                    agreements: 0,
                    errors_by_split: empty_error_splits(),
                    fired_by_count: BTreeMap::new(),
                },
            )
        })
        .collect();
    let population_n = fixtures
        .iter()
        .filter(|fixture| !fixture.label.basis.is_designed() && fixture.label.disputed.is_none())
        .count();
    let mut band_agreements = 0_u32;
    let mut confusion = BTreeMap::new();
    let mut misses = Vec::new();
    let mut band_misses = Vec::new();
    let mut excluded_from_population = Vec::new();
    let mut disputed_ids = Vec::new();

    for fixture in fixtures {
        // Combo floors are unmeasured on this path by design (goldens are ratcheted).
        let Some(estimate) = estimate_bracket(&fixture.decklist, db, &ComboTable::default()) else {
            continue;
        };
        let reading = reading_from_estimate(&estimate);
        let in_population = !fixture.label.basis.is_designed() && fixture.label.disputed.is_none();
        let split = splits
            .get(&fixture.id)
            .copied()
            .unwrap_or(CorpusSplit::Train);

        if !in_population {
            excluded_from_population.push(fixture.id.clone());
        }
        if fixture.label.disputed.is_some() {
            disputed_ids.push(fixture.id.clone());
        }

        if reading.tier == fixture.label.tier {
            if in_population {
                band_agreements = band_agreements.saturating_add(1);
            }
        } else {
            band_misses.push(BandMiss {
                id: fixture.id.clone(),
                labelled: fixture.label.tier,
                observed: reading.tier,
            });
        }
        if in_population {
            *confusion
                .entry((fixture.label.tier.as_u8(), reading.tier.as_u8()))
                .or_default() += 1;
        }

        for axis in BracketAxis::iter() {
            let labelled = fixture.label.axis_counts.get(&axis).copied().unwrap_or(0);
            let observed = reading.observed.get(&axis).copied().unwrap_or(0);
            let expected_names = fixture
                .label
                .cards_named
                .get(&axis)
                .cloned()
                .unwrap_or_default();
            let observed_names = reading.evidence.get(&axis).cloned().unwrap_or_default();
            let missing_names: Vec<String> = expected_names
                .difference(&observed_names)
                .cloned()
                .collect();
            let count_matches = labelled == observed;
            let evidence_matches = missing_names.is_empty();

            if !count_matches {
                misses.push(CorpusMiss {
                    id: fixture.id.clone(),
                    axis,
                    miss: AxisMiss::Count { labelled, observed },
                });
            }
            if !evidence_matches {
                misses.push(CorpusMiss {
                    id: fixture.id.clone(),
                    axis,
                    miss: AxisMiss::MissingEvidence {
                        names: missing_names,
                    },
                });
            }

            let Some(accumulator) = accumulators.get_mut(&axis) else {
                unreachable!("every BracketAxis accumulator is initialized");
            };
            if count_matches && evidence_matches {
                if in_population {
                    accumulator.agreements = accumulator.agreements.saturating_add(1);
                }
            } else {
                *accumulator.errors_by_split.entry(split).or_default() += 1;
            }

            for ((rule_axis, _threshold), fired) in &reading.fired {
                if *rule_axis != axis {
                    continue;
                }
                let counts = accumulator.fired_by_count.entry(observed).or_default();
                match fired {
                    FiredState::Fired => counts.0 = counts.0.saturating_add(1),
                    FiredState::NotFired => counts.1 = counts.1.saturating_add(1),
                }
            }
        }
    }

    let denominator = u32::try_from(population_n).unwrap_or(u32::MAX);
    let per_axis = accumulators
        .into_iter()
        .map(|(axis, accumulator)| {
            let status = if ARMED_RULES.contains(&axis) {
                CorpusRuleStatus::Armed
            } else {
                CorpusRuleStatus::Reported
            };
            (
                axis,
                AxisScore {
                    axis,
                    status,
                    agreement: CorpusFraction {
                        numerator: accumulator.agreements,
                        denominator,
                    },
                    regime: regime(population_n, MIN_AXIS_POPULATION_N),
                    errors_by_split: accumulator.errors_by_split,
                    fired_by_count: accumulator.fired_by_count,
                },
            )
        })
        .collect();
    let mut held_out_ids: Vec<String> = splits
        .iter()
        .filter_map(|(id, split)| (*split == CorpusSplit::HeldOut).then_some(id.clone()))
        .collect();
    held_out_ids.sort();
    excluded_from_population.sort();
    excluded_from_population.dedup();
    disputed_ids.sort();
    disputed_ids.dedup();

    CorpusScore {
        band_agreement: CorpusFraction {
            numerator: band_agreements,
            denominator,
        },
        band_regime: regime(population_n, MIN_BAND_POPULATION_N),
        per_axis,
        confusion,
        misses,
        band_misses,
        held_out_ids,
        excluded_from_population,
        disputed_ids,
    }
}

/// One floor-rule golden, represented as a JSON-friendly typed row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ExpectationFiredRow {
    pub axis: BracketAxis,
    pub threshold: u8,
    pub state: FiredState,
}

/// The golden estimator reading for one fixture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub struct ExpectationRow {
    pub tier: CommanderBracketTier,
    pub observed: BTreeMap<BracketAxis, u8>,
    pub evidence: BTreeMap<BracketAxis, BTreeSet<String>>,
    pub fired: Vec<ExpectationFiredRow>,
}

/// One validated expectation-history entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RatchetEntry {
    pub engine_version: String,
    pub recorded_on: ReleaseDate,
    pub rows_digest: String,
    pub changelog_note: String,
}

/// Parsed golden rows and their release-gated history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectationsFile {
    pub rows: BTreeMap<String, ExpectationRow>,
    pub ratchet_history: Vec<RatchetEntry>,
    pub band_agreement_floor: Option<CorpusFraction>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRatchetEntry {
    engine_version: String,
    recorded_on: String,
    rows_digest: String,
    changelog_note: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExpectationsFile {
    rows: BTreeMap<String, ExpectationRow>,
    ratchet_history: Vec<RawRatchetEntry>,
    #[serde(default)]
    band_agreement_floor: Option<CorpusFraction>,
}

/// Expectation-file parsing and I/O failures.
#[derive(Debug, Error)]
pub enum ExpectationsError {
    #[error("failed to read expectations file {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid expectations JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid ratchet date {value:?}; expected YYYY-MM-DD")]
    InvalidDate { value: String },
    #[error("invalid engine version {value:?}; expected numeric x.y.z")]
    InvalidVersion { value: String },
}

fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let mut components = version.split('.');
    let major = components.next()?.parse().ok()?;
    let minor = components.next()?.parse().ok()?;
    let patch = components.next()?.parse().ok()?;
    components.next().is_none().then_some((major, minor, patch))
}

/// Parse and validate an expectations document without filesystem access.
pub fn parse_expectations(json: &str) -> Result<ExpectationsFile, ExpectationsError> {
    let raw: RawExpectationsFile = serde_json::from_str(json)?;
    let ratchet_history = raw
        .ratchet_history
        .into_iter()
        .map(|entry| {
            if parse_version(&entry.engine_version).is_none() {
                return Err(ExpectationsError::InvalidVersion {
                    value: entry.engine_version,
                });
            }
            let recorded_on = ReleaseDate::parse(&entry.recorded_on).ok_or_else(|| {
                ExpectationsError::InvalidDate {
                    value: entry.recorded_on.clone(),
                }
            })?;
            Ok(RatchetEntry {
                engine_version: entry.engine_version,
                recorded_on,
                rows_digest: entry.rows_digest,
                changelog_note: entry.changelog_note,
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ExpectationsFile {
        rows: raw.rows,
        ratchet_history,
        band_agreement_floor: raw.band_agreement_floor,
    })
}

/// Load and validate an expectations document.
pub fn load_expectations(path: &Path) -> Result<ExpectationsFile, ExpectationsError> {
    let json = fs::read_to_string(path).map_err(|source| ExpectationsError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    parse_expectations(&json)
}

/// Convert a corpus reading into its serializable golden representation.
pub fn expectation_row_from_reading(reading: &CorpusReading) -> ExpectationRow {
    ExpectationRow {
        tier: reading.tier,
        observed: reading.observed.clone(),
        evidence: reading.evidence.clone(),
        fired: reading
            .fired
            .iter()
            .map(|((axis, threshold), state)| ExpectationFiredRow {
                axis: *axis,
                threshold: *threshold,
                state: *state,
            })
            .collect(),
    }
}

/// SHA-256 over `serde_json::to_string` of the key-ordered golden rows.
pub fn rows_digest(rows: &BTreeMap<String, ExpectationRow>) -> String {
    let canonical = match serde_json::to_string(rows) {
        Ok(json) => json,
        Err(error) => panic!("ExpectationRow serialization is infallible: {error}"),
    };
    let digest = Sha256::digest(canonical.as_bytes());
    format!("sha256:{digest:x}")
}

/// Validate all six release-ratchet invariants, returning every breach at once.
pub fn assert_ratchet_history(
    expectations: &ExpectationsFile,
    current_version: &str,
) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    if expectations.ratchet_history.is_empty() {
        errors.push("ratchet_history must not be empty".to_string());
    }

    let parsed_versions: Vec<Option<(u64, u64, u64)>> = expectations
        .ratchet_history
        .iter()
        .map(|entry| {
            let parsed = parse_version(&entry.engine_version);
            if parsed.is_none() {
                errors.push(format!(
                    "invalid engine version {:?}; expected numeric x.y.z",
                    entry.engine_version
                ));
            }
            parsed
        })
        .collect();
    for (index, versions) in parsed_versions.windows(2).enumerate() {
        if matches!(versions, [Some(previous), Some(next)] if next <= previous) {
            errors.push(format!(
                "ratchet versions must strictly increase at entries {index} and {}",
                index + 1
            ));
        }
    }
    for (index, dates) in expectations.ratchet_history.windows(2).enumerate() {
        if dates[1].recorded_on < dates[0].recorded_on {
            errors.push(format!(
                "ratchet dates decrease at entries {index} and {}",
                index + 1
            ));
        }
    }
    for (index, entry) in expectations.ratchet_history.iter().enumerate() {
        if entry.changelog_note.trim().is_empty() {
            errors.push(format!("ratchet entry {index} has an empty changelog_note"));
        }
    }

    if let Some(last) = expectations.ratchet_history.last() {
        let actual_digest = rows_digest(&expectations.rows);
        if last.rows_digest != actual_digest {
            errors.push(format!(
                "last rows_digest is {:?}, expected {actual_digest:?}",
                last.rows_digest
            ));
        }
        match (
            parse_version(&last.engine_version),
            parse_version(current_version),
        ) {
            (Some(last_version), Some(current)) if last_version > current => errors.push(format!(
                "last ratchet version {} is above current engine version {current_version}",
                last.engine_version
            )),
            (_, None) => errors.push(format!(
                "current engine version {current_version:?} is not numeric x.y.z"
            )),
            _ => {}
        }
    } else if parse_version(current_version).is_none() {
        errors.push(format!(
            "current engine version {current_version:?} is not numeric x.y.z"
        ));
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// A band floor is legal only after the population reaches the band minimum.
pub fn assert_floor_matches_population(
    floor: Option<CorpusFraction>,
    population_n: usize,
) -> Result<(), String> {
    match floor {
        None => Ok(()),
        Some(_) if population_n >= MIN_BAND_POPULATION_N => Ok(()),
        Some(_) => Err(format!(
            "band_agreement_floor requires population n >= {MIN_BAND_POPULATION_N}, got {population_n}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::{BracketCardClass, BracketLists};

    fn fixture_json(basis: &str, extra: &str) -> String {
        format!(
            r#"{{
                "id":"row-1",
                "name":"",
                "label":{{
                    "tier":"core",
                    "basis":"{basis}",
                    "labeller":"owner",
                    "labelled_on":"2026-01-02",
                    "rules_copy_read":"2026-01-01",
                    "axis_counts":{{}},
                    "cards_named":{{}}
                }},
                "source":{{
                    "captured_on":"2026-01-03",
                    "capture_note":"consented"{extra}
                }},
                "decklist":{{"commander":[""],"main_deck":[]}}
            }}"#
        )
    }

    fn synthetic_fixture(id: &str, basis: LabelBasis) -> CorpusFixture {
        let mut fixture = parse_corpus_fixture(&fixture_json("owner_declared", "")).unwrap();
        fixture.id = id.to_string();
        fixture.label.basis = basis;
        fixture
    }

    fn empty_rows() -> BTreeMap<String, ExpectationRow> {
        BTreeMap::new()
    }

    fn expectations_with_history(history: Vec<RatchetEntry>) -> ExpectationsFile {
        ExpectationsFile {
            rows: empty_rows(),
            ratchet_history: history,
            band_agreement_floor: None,
        }
    }

    fn ratchet(version: &str, date: &str, digest: String, note: &str) -> RatchetEntry {
        RatchetEntry {
            engine_version: version.to_string(),
            recorded_on: ReleaseDate::parse(date).unwrap(),
            rows_digest: digest,
            changelog_note: note.to_string(),
        }
    }

    #[test]
    fn fixture_parse_happy_path() {
        let json = fixture_json(
            "third_party_published",
            ",\"url\":\"https://invalid.example\",\"terms_checked_on\":\"2026-01-03\"",
        );
        let fixture = parse_corpus_fixture(&json).unwrap();
        assert_eq!(fixture.id, "row-1");
        assert_eq!(fixture.label.basis, LabelBasis::ThirdPartyPublished);
        assert!(fixture.label.cards_named.is_empty());
    }

    #[test]
    fn fixture_rejects_unknown_field() {
        let json = fixture_json("owner_declared", "").replace(
            "\"capture_note\":\"consented\"",
            "\"capture_note\":\"consented\",\"unknown\":true",
        );
        assert!(matches!(
            parse_corpus_fixture(&json),
            Err(CorpusFixtureError::Json(_))
        ));
    }

    #[test]
    fn third_party_fixture_requires_terms_date() {
        assert!(matches!(
            parse_corpus_fixture(&fixture_json(
                "third_party_published",
                ",\"url\":\"https://invalid.example\""
            )),
            Err(CorpusFixtureError::UrlTermsRequired)
        ));
    }

    #[test]
    fn directory_loader_sorts_files_skips_expectations_and_rejects_duplicate_ids() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join("b.json"),
            fixture_json("owner_declared", "").replace("row-1", "row-b"),
        )
        .unwrap();
        fs::write(
            directory.path().join("a.json"),
            fixture_json("owner_declared", "").replace("row-1", "row-a"),
        )
        .unwrap();
        fs::write(
            directory.path().join("expectations.json"),
            "not fixture JSON",
        )
        .unwrap();

        let fixtures = load_corpus_dir(directory.path()).unwrap();
        assert_eq!(
            fixtures
                .iter()
                .map(|fixture| fixture.id.as_str())
                .collect::<Vec<_>>(),
            ["row-a", "row-b"]
        );

        fs::write(
            directory.path().join("c.json"),
            fixture_json("owner_declared", "").replace("row-1", "row-a"),
        )
        .unwrap();
        assert!(matches!(
            load_corpus_dir(directory.path()),
            Err(CorpusLoadError::DuplicateId { .. })
        ));
    }

    #[test]
    fn unresolved_names_are_sorted_and_deduplicated() {
        let mut fixture = synthetic_fixture("resolution", LabelBasis::OwnerDeclared);
        fixture.decklist.commander.push(String::new());
        fixture.decklist.main_deck.push(String::new());
        assert_eq!(unresolved_names(&fixture, &CardDatabase::default()), [""]);
    }

    #[test]
    fn held_out_split_is_deterministic_order_insensitive_and_excludes_designed() {
        let fixtures = vec![
            synthetic_fixture("p-4", LabelBasis::OwnerDeclared),
            synthetic_fixture("p-2", LabelBasis::OwnerDeclared),
            synthetic_fixture("d-1", LabelBasis::RulesDerivation),
            synthetic_fixture("p-1", LabelBasis::OwnerDeclared),
            synthetic_fixture("p-3", LabelBasis::OwnerDeclared),
        ];
        let mut reversed = fixtures.clone();
        reversed.reverse();
        let split = held_out_split(&fixtures);
        assert_eq!(split, held_out_split(&reversed));
        assert_eq!(split["d-1"], CorpusSplit::Designed);
        assert_eq!(split["p-4"], CorpusSplit::HeldOut);
        assert!(HELD_OUT_RULE_SENTENCE.contains(&HELD_OUT_MODULUS.to_string()));
        assert!(HELD_OUT_RULE_SENTENCE.contains(&HELD_OUT_RESIDUE.to_string()));

        let mut appended = fixtures;
        appended.push(synthetic_fixture("z-last", LabelBasis::OwnerDeclared));
        let appended_split = held_out_split(&appended);
        for (id, original) in split {
            assert_eq!(appended_split[&id], original);
        }
    }

    #[test]
    fn fraction_comparison_is_exact_and_zero_denominators_are_undefined() {
        let cases = [
            ((1, 3), (33_333, 100_000), true),
            ((33_333, 100_000), (1, 3), false),
            ((2, 4), (1, 2), true),
            ((0, 1), (0, 1), true),
            ((0, 0), (0, 1), false),
            ((0, 1), (0, 0), false),
        ];
        for ((left_n, left_d), (right_n, right_d), expected) in cases {
            let left = CorpusFraction {
                numerator: left_n,
                denominator: left_d,
            };
            let right = CorpusFraction {
                numerator: right_n,
                denominator: right_d,
            };
            assert_eq!(left.at_least(right), expected);
        }
        assert_eq!(
            CorpusFraction {
                numerator: 0,
                denominator: 0
            }
            .as_f64(),
            None
        );
    }

    #[test]
    fn rows_digest_is_key_order_stable_and_changes_with_content() {
        let reading = CorpusReading {
            tier: CommanderBracketTier::Core,
            observed: BTreeMap::new(),
            evidence: BTreeMap::new(),
            fired: BTreeMap::new(),
        };
        let row = expectation_row_from_reading(&reading);
        let rows_a = BTreeMap::from([
            ("a".to_string(), row.clone()),
            ("b".to_string(), row.clone()),
        ]);
        let mut rows_b = BTreeMap::new();
        rows_b.insert("b".to_string(), row.clone());
        rows_b.insert("a".to_string(), row);
        assert_eq!(rows_digest(&rows_a), rows_digest(&rows_b));

        rows_b.get_mut("a").unwrap().tier = CommanderBracketTier::Upgraded;
        assert_ne!(rows_digest(&rows_a), rows_digest(&rows_b));
    }

    #[test]
    fn ratchet_history_rejects_each_invalid_condition() {
        assert!(assert_ratchet_history(&expectations_with_history(vec![]), "1.0.0").is_err());
        let digest = rows_digest(&empty_rows());

        let non_increasing = expectations_with_history(vec![
            ratchet("1.0.0", "2026-01-01", digest.clone(), "first"),
            ratchet("1.0.0", "2026-01-02", digest.clone(), "second"),
        ]);
        assert!(assert_ratchet_history(&non_increasing, "1.0.0").is_err());

        let decreasing_dates = expectations_with_history(vec![
            ratchet("0.9.0", "2026-01-02", digest.clone(), "first"),
            ratchet("1.0.0", "2026-01-01", digest.clone(), "second"),
        ]);
        assert!(assert_ratchet_history(&decreasing_dates, "1.0.0").is_err());

        let empty_note =
            expectations_with_history(vec![ratchet("1.0.0", "2026-01-01", digest.clone(), " ")]);
        assert!(assert_ratchet_history(&empty_note, "1.0.0").is_err());

        let bad_digest = expectations_with_history(vec![ratchet(
            "1.0.0",
            "2026-01-01",
            "sha256:bad".to_string(),
            "note",
        )]);
        assert!(assert_ratchet_history(&bad_digest, "1.0.0").is_err());

        let future =
            expectations_with_history(vec![ratchet("2.0.0", "2026-01-01", digest, "note")]);
        assert!(assert_ratchet_history(&future, "1.9.9").is_err());
    }

    #[test]
    fn expectations_parser_validates_raw_ratchet_fields() {
        let digest = rows_digest(&empty_rows());
        let json = format!(
            r#"{{
                "rows":{{}},
                "ratchet_history":[{{
                    "engine_version":"0.78.0",
                    "recorded_on":"2026-01-01",
                    "rows_digest":"{digest}",
                    "changelog_note":"initial"
                }}],
                "band_agreement_floor":null
            }}"#
        );
        let expectations = parse_expectations(&json).unwrap();
        assert!(assert_ratchet_history(&expectations, "0.78.0").is_ok());

        let invalid_version = json.replace("0.78.0", "0.78");
        assert!(matches!(
            parse_expectations(&invalid_version),
            Err(ExpectationsError::InvalidVersion { .. })
        ));
    }

    #[test]
    fn floor_requires_minimum_population() {
        let floor = CorpusFraction {
            numerator: 9,
            denominator: 10,
        };
        assert!(assert_floor_matches_population(Some(floor), MIN_BAND_POPULATION_N - 1).is_err());
        assert!(assert_floor_matches_population(None, 0).is_ok());
        assert!(assert_floor_matches_population(Some(floor), MIN_BAND_POPULATION_N).is_ok());
    }

    #[test]
    fn reading_maps_both_game_changer_rule_rows() {
        let db = CardDatabase::default().with_bracket_lists(BracketLists::from_pairs(
            "test",
            &[(BracketCardClass::GameChangers, &[""])],
        ));
        let deck = PlayerDeckList {
            commander: vec![String::new()],
            main_deck: vec![String::new(), String::new(), String::new()],
            ..Default::default()
        };
        // Combo floors are unmeasured on this path by design (goldens are ratcheted).
        let estimate = estimate_bracket(&deck, &db, &ComboTable::default()).unwrap();
        let reading = reading_from_estimate(&estimate);
        assert_eq!(reading.observed[&BracketAxis::GameChangers], 4);
        assert_eq!(
            reading.fired[&(BracketAxis::GameChangers, 1)],
            FiredState::Fired
        );
        assert_eq!(
            reading.fired[&(BracketAxis::GameChangers, 4)],
            FiredState::Fired
        );
    }

    #[test]
    fn scoring_excludes_designed_rates_but_reports_designed_misses() {
        let db = CardDatabase::default().with_bracket_lists(BracketLists::from_pairs(
            "test",
            &[(BracketCardClass::GameChangers, &[""])],
        ));
        let mut designed = synthetic_fixture("designed", LabelBasis::RulesDerivation);
        designed.label.tier = CommanderBracketTier::Core;
        let mut population = synthetic_fixture("population", LabelBasis::OwnerDeclared);
        population.label.tier = CommanderBracketTier::Upgraded;
        population
            .label
            .axis_counts
            .insert(BracketAxis::GameChangers, 1);

        let score = score_corpus(&[designed, population], &db);
        assert_eq!(score.band_agreement.denominator, 1);
        assert_eq!(score.band_agreement.numerator, 1);
        assert_eq!(score.excluded_from_population, ["designed"]);
        assert_eq!(
            score.per_axis[&BracketAxis::GameChangers]
                .agreement
                .denominator,
            1
        );
        assert!(score.misses.iter().any(|miss| miss.id == "designed"));
        assert!(score.band_misses.iter().any(|miss| miss.id == "designed"));
    }
}
