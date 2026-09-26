//! Freeze `estimate_bracket` over the projected bundled deck catalog and emit
//! the Commander base-floor gate measurement used by later phase 61 work.
//!
//! Bracket policy is WotC Commander format guidance, not the Comprehensive
//! Rules, so no CR annotations apply here.
//!
//! Usage: `bracket-baseline <card-data-root-or-json>`

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine::database::bracket_lists::BracketLists;
use engine::database::CardDatabase;
use engine::game::bracket_estimate::{estimate_bracket, BracketEstimate, CommanderBracketTier};
use engine::game::deck_loading::PlayerDeckList;
use engine::game::BracketAxis;
use serde::{Deserialize, Serialize};

const DECK_CATALOG_PATH: &str = "client/public/decks.json";
const BUNDLED_CEDH_PATH: &str = "data/bundled_cedh_decks.json";
const BRACKET_LISTS_PATH: &str = "data/bracket_lists.json";
const VINTAGE_PATH: &str = "crates/engine/data/mtgjson-vintage";
const BASELINE_DIRECTORY: &str = "data/baselines";

fn main() -> ExitCode {
    match run(std::env::args_os().skip(1), io::stderr().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("bracket-baseline: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(
    mut args: impl Iterator<Item = OsString>,
    mut diagnostics: impl Write,
) -> Result<(), String> {
    let (Some(card_data_argument), None) = (args.next(), args.next()) else {
        return Err("Usage: bracket-baseline <card-data-root-or-json>".to_string());
    };

    let card_data_argument = PathBuf::from(card_data_argument);
    let card_data_path = resolve_card_data_path(&card_data_argument);
    let bracket_lists = BracketLists::from_json_path(Path::new(BRACKET_LISTS_PATH))
        .map_err(|error| format!("could not load {BRACKET_LISTS_PATH}: {error}"))?;
    let db = CardDatabase::from_export(&card_data_path)
        .map_err(|error| format!("could not load {}: {error}", card_data_path.display()))?
        .with_bracket_lists(bracket_lists.clone());

    let projected_decks = load_catalog(Path::new(DECK_CATALOG_PATH)).map_err(|error| {
        format!("{error}; {DECK_CATALOG_PATH} is generated data, so run the data pipeline first")
    })?;
    let bundled_cedh = match load_optional_catalog(Path::new(BUNDLED_CEDH_PATH))? {
        OptionalCatalog::Present(decks) => decks,
        OptionalCatalog::Absent => {
            writeln!(
                diagnostics,
                "bracket-baseline: note: {BUNDLED_CEDH_PATH} is absent; continuing without the deferred cEDH population"
            )
            .map_err(|error| format!("could not write diagnostic: {error}"))?;
            BTreeMap::new()
        }
    };

    let vintage = read_trimmed(Path::new(VINTAGE_PATH))?;
    if vintage.is_empty() {
        return Err(format!("{VINTAGE_PATH} is empty"));
    }

    let mut rows = Vec::with_capacity(projected_decks.len() + bundled_cedh.len());
    let mut gate = GateAccumulator::default();
    for (deck_identifier, deck) in projected_decks {
        let row = baseline_row(
            deck_identifier,
            BaselineDeckSource::ProjectedDeckCatalog,
            deck,
            &db,
        );
        gate.record(&row)?;
        rows.push(row);
    }
    for (deck_identifier, deck) in bundled_cedh {
        let row = baseline_row(deck_identifier, BaselineDeckSource::BundledCedh, deck, &db);
        gate.record(&row)?;
        rows.push(row);
    }

    let unresolved_curated_names = bracket_lists
        .all_names()
        .filter(|name| db.get_face_by_name(name).is_none())
        .map(str::to_string)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let gate_report = gate.finish(
        vintage.clone(),
        bracket_lists.version.clone(),
        unresolved_curated_names,
    )?;
    let baseline = BracketBaselineFile {
        mtgjson_vintage: vintage.clone(),
        bracket_lists_version: bracket_lists.version,
        rows,
    };

    let output_directory = Path::new(BASELINE_DIRECTORY);
    std::fs::create_dir_all(output_directory)
        .map_err(|error| format!("could not create {}: {error}", output_directory.display()))?;
    let baseline_path = output_directory.join(format!("bracket-estimate-{vintage}.json"));
    let gate_path = output_directory.join(format!("bracket-gate-{vintage}.json"));
    write_json(&baseline_path, &baseline)?;
    write_json(&gate_path, &gate_report)?;

    let basis_points = exhibition_basis_points(
        gate_report.exhibition_decks,
        gate_report.commander_deck_population,
    )?;
    let verdict = gate_verdict(basis_points);
    writeln!(
        diagnostics,
        "bracket-baseline: gate verdict {verdict} ({}/{} Exhibition decks, {basis_points} basis points)",
        gate_report.exhibition_decks, gate_report.commander_deck_population
    )
    .map_err(|error| format!("could not write gate verdict: {error}"))?;
    let designed_deck_count = gate_report
        .designed_population
        .get(&BaselineDeckSource::BundledCedh)
        .map_or(0, |population| population.deck_count);
    writeln!(
        diagnostics,
        "bracket-baseline: designed population bundled_cedh ({designed_deck_count} decks; excluded from sampled gate rate)"
    )
    .map_err(|error| format!("could not write designed population summary: {error}"))?;
    Ok(())
}

fn resolve_card_data_path(argument: &Path) -> PathBuf {
    if argument.is_dir() {
        argument.join("card-data.json")
    } else {
        argument.to_path_buf()
    }
}

fn read_trimmed(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|contents| contents.trim().to_string())
        .map_err(|error| format!("could not read {}: {error}", path.display()))
}

fn load_catalog(path: &Path) -> Result<BTreeMap<String, CatalogDeck>, String> {
    let input = std::fs::read(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_slice(&input)
        .map_err(|error| format!("invalid deck catalog in {}: {error}", path.display()))
}

enum OptionalCatalog {
    Present(BTreeMap<String, CatalogDeck>),
    Absent,
}

fn load_optional_catalog(path: &Path) -> Result<OptionalCatalog, String> {
    match std::fs::read(path) {
        Ok(input) => serde_json::from_slice(&input)
            .map(OptionalCatalog::Present)
            .map_err(|error| format!("invalid deck catalog in {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(OptionalCatalog::Absent),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("could not serialize {}: {error}", path.display()))?;
    bytes.push(b'\n');
    std::fs::write(path, bytes)
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CatalogDeck {
    #[serde(default)]
    code: String,
    name: String,
    #[serde(rename = "type")]
    deck_type: String,
    #[serde(default)]
    commander: Vec<CatalogCard>,
    #[serde(default)]
    main_board: Vec<CatalogCard>,
    #[serde(default)]
    side_board: Vec<CatalogCard>,
}

#[derive(Debug, Deserialize)]
struct CatalogCard {
    name: String,
    #[serde(default = "one")]
    count: u32,
}

const fn one() -> u32 {
    1
}

fn expand_cards(entries: &[CatalogCard]) -> Vec<String> {
    entries
        .iter()
        .flat_map(|entry| std::iter::repeat_n(entry.name.clone(), entry.count as usize))
        .collect()
}

impl CatalogDeck {
    fn to_player_deck_list(&self) -> PlayerDeckList {
        PlayerDeckList {
            commander: expand_cards(&self.commander),
            main_deck: expand_cards(&self.main_board),
            sideboard: expand_cards(&self.side_board),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BaselineDeckSource {
    ProjectedDeckCatalog,
    BundledCedh,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum NotEstimatedReason {
    EmptyCommander,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum BaselineEstimateResult {
    Estimated { estimate: BracketEstimate },
    NotEstimated { reason: NotEstimatedReason },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BracketBaselineRow {
    deck_identifier: String,
    source: BaselineDeckSource,
    code: String,
    name: String,
    deck_type: String,
    result: BaselineEstimateResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BracketBaselineFile {
    mtgjson_vintage: String,
    bracket_lists_version: String,
    rows: Vec<BracketBaselineRow>,
}

fn baseline_row(
    deck_identifier: String,
    source: BaselineDeckSource,
    deck: CatalogDeck,
    db: &CardDatabase,
) -> BracketBaselineRow {
    let deck_list = deck.to_player_deck_list();
    let result = match estimate_bracket(&deck_list, db) {
        Some(estimate) => BaselineEstimateResult::Estimated { estimate },
        None => BaselineEstimateResult::NotEstimated {
            reason: NotEstimatedReason::EmptyCommander,
        },
    };
    BracketBaselineRow {
        deck_identifier,
        source,
        code: deck.code,
        name: deck.name,
        deck_type: deck.deck_type,
        result,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BracketGateReport {
    mtgjson_vintage: String,
    bracket_lists_version: String,
    commander_deck_population: u64,
    game_changer_histogram: BTreeMap<u8, u64>,
    tier_histogram: BTreeMap<u8, u64>,
    exhibition_decks: u64,
    contributing_card_name_frequency: BTreeMap<String, u64>,
    unresolved_curated_names: Vec<String>,
    designed_population: BTreeMap<BaselineDeckSource, DesignedPopulationReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct DesignedPopulationReport {
    deck_count: u64,
    tier_histogram: BTreeMap<u8, u64>,
}

#[derive(Default)]
struct GateAccumulator {
    commander_deck_population: u64,
    game_changer_histogram: BTreeMap<u8, u64>,
    tier_histogram: BTreeMap<u8, u64>,
    exhibition_decks: u64,
    contributing_card_name_frequency: BTreeMap<String, u64>,
    designed_population: BTreeMap<BaselineDeckSource, DesignedPopulationReport>,
}

impl GateAccumulator {
    fn record(&mut self, row: &BracketBaselineRow) -> Result<(), String> {
        let BaselineEstimateResult::Estimated { estimate } = &row.result else {
            return Ok(());
        };

        // The projected catalog is a sampled population used for the gate rate.
        // Bundled cEDH decks are hand-authored designs, so mixing them into that
        // rate would conflate designed fixtures with sampled population data.
        match row.source {
            BaselineDeckSource::ProjectedDeckCatalog => self.record_sampled(estimate),
            BaselineDeckSource::BundledCedh => self.record_designed(row.source, estimate),
        }
    }

    fn record_sampled(&mut self, estimate: &BracketEstimate) -> Result<(), String> {
        self.commander_deck_population = self
            .commander_deck_population
            .checked_add(1)
            .ok_or_else(|| "commander deck population overflowed".to_string())?;
        increment(
            &mut self.game_changer_histogram,
            estimate.axes[&BracketAxis::GameChangers].count,
        )?;
        increment(&mut self.tier_histogram, estimate.tier.as_u8())?;
        if estimate.tier == CommanderBracketTier::Exhibition {
            self.exhibition_decks = self
                .exhibition_decks
                .checked_add(1)
                .ok_or_else(|| "Exhibition deck count overflowed".to_string())?;
        }
        for reading in estimate.axes.values() {
            for name in &reading.contributing {
                increment(&mut self.contributing_card_name_frequency, name.clone())?;
            }
        }
        Ok(())
    }

    fn record_designed(
        &mut self,
        source: BaselineDeckSource,
        estimate: &BracketEstimate,
    ) -> Result<(), String> {
        let population =
            self.designed_population
                .entry(source)
                .or_insert_with(|| DesignedPopulationReport {
                    deck_count: 0,
                    tier_histogram: BTreeMap::new(),
                });
        population.deck_count = population
            .deck_count
            .checked_add(1)
            .ok_or_else(|| "designed deck population overflowed".to_string())?;
        increment(&mut population.tier_histogram, estimate.tier.as_u8())
    }

    fn finish(
        self,
        mtgjson_vintage: String,
        bracket_lists_version: String,
        unresolved_curated_names: Vec<String>,
    ) -> Result<BracketGateReport, String> {
        if self.commander_deck_population == 0 {
            return Err(format!(
                "{DECK_CATALOG_PATH} contains no decks that can be bracket-estimated"
            ));
        }
        Ok(BracketGateReport {
            mtgjson_vintage,
            bracket_lists_version,
            commander_deck_population: self.commander_deck_population,
            game_changer_histogram: self.game_changer_histogram,
            tier_histogram: self.tier_histogram,
            exhibition_decks: self.exhibition_decks,
            contributing_card_name_frequency: self.contributing_card_name_frequency,
            unresolved_curated_names,
            designed_population: self.designed_population,
        })
    }
}

fn increment<K: Ord>(counts: &mut BTreeMap<K, u64>, key: K) -> Result<(), String> {
    let count = counts.entry(key).or_default();
    *count = count
        .checked_add(1)
        .ok_or_else(|| "histogram count overflowed".to_string())?;
    Ok(())
}

fn exhibition_basis_points(exhibition_decks: u64, population: u64) -> Result<u64, String> {
    if population == 0 {
        return Err("cannot calculate the gate verdict for an empty population".to_string());
    }
    exhibition_decks
        .checked_mul(10_000)
        .map(|scaled| scaled / population)
        .ok_or_else(|| "basis-point calculation overflowed".to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GateVerdict {
    Confirm,
    ConfirmNarrowed,
    Kill,
}

fn gate_verdict(exhibition_basis_points: u64) -> GateVerdict {
    match exhibition_basis_points {
        2_500.. => GateVerdict::Confirm,
        500..=2_499 => GateVerdict::ConfirmNarrowed,
        0..=499 => GateVerdict::Kill,
    }
}

impl std::fmt::Display for GateVerdict {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::Confirm => "CONFIRM",
            Self::ConfirmNarrowed => "CONFIRM-NARROWED",
            Self::Kill => "KILL",
        };
        formatter.write_str(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::{AxisReading, BracketCoverage, EstimateConfidence};
    use strum::IntoEnumIterator;

    fn catalog_deck(commander: Vec<CatalogCard>) -> CatalogDeck {
        CatalogDeck {
            code: "TST".to_string(),
            name: "Test deck".to_string(),
            deck_type: "Test".to_string(),
            commander,
            main_board: Vec::new(),
            side_board: Vec::new(),
        }
    }

    fn estimated_row(source: BaselineDeckSource, tier: CommanderBracketTier) -> BracketBaselineRow {
        BracketBaselineRow {
            deck_identifier: "test".to_string(),
            source,
            code: "TST".to_string(),
            name: "Test deck".to_string(),
            deck_type: "Test".to_string(),
            result: BaselineEstimateResult::Estimated {
                estimate: BracketEstimate {
                    tier,
                    axes: BracketAxis::iter()
                        .map(|axis| (axis, AxisReading::default()))
                        .collect(),
                    checks: Vec::new(),
                    coverage: BracketCoverage {
                        counted: 0,
                        resolved: 0,
                        unresolved: Vec::new(),
                        confidence: EstimateConfidence::Complete,
                    },
                    data_version: "test".to_string(),
                },
            },
        }
    }

    #[test]
    fn expands_card_counts_without_deduplicating() {
        let cards = vec![CatalogCard {
            name: "Persistent Petitioners".to_string(),
            count: 4,
        }];
        assert_eq!(
            expand_cards(&cards),
            vec!["Persistent Petitioners".to_string(); 4]
        );
    }

    #[test]
    fn gate_thresholds_are_inclusive_at_exact_boundaries() {
        assert_eq!(
            gate_verdict(exhibition_basis_points(1, 4).unwrap()),
            GateVerdict::Confirm
        );
        assert_eq!(gate_verdict(2_499), GateVerdict::ConfirmNarrowed);
        assert_eq!(
            gate_verdict(exhibition_basis_points(1, 20).unwrap()),
            GateVerdict::ConfirmNarrowed
        );
        assert_eq!(
            gate_verdict(exhibition_basis_points(499, 10_000).unwrap()),
            GateVerdict::Kill
        );
    }

    #[test]
    fn empty_commander_is_recorded_as_not_estimated() {
        let row = baseline_row(
            "empty".to_string(),
            BaselineDeckSource::ProjectedDeckCatalog,
            catalog_deck(Vec::new()),
            &CardDatabase::default(),
        );
        assert_eq!(
            row.result,
            BaselineEstimateResult::NotEstimated {
                reason: NotEstimatedReason::EmptyCommander
            }
        );
    }

    #[test]
    fn bundled_cedh_does_not_contribute_to_sampled_gate_counts() {
        let mut gate = GateAccumulator::default();
        gate.record(&estimated_row(
            BaselineDeckSource::BundledCedh,
            CommanderBracketTier::Exhibition,
        ))
        .unwrap();

        assert_eq!(gate.commander_deck_population, 0);
        assert_eq!(gate.exhibition_decks, 0);
    }

    #[test]
    fn bundled_cedh_contributes_to_designed_population_tier_histogram() {
        let mut gate = GateAccumulator::default();
        gate.record(&estimated_row(
            BaselineDeckSource::ProjectedDeckCatalog,
            CommanderBracketTier::Core,
        ))
        .unwrap();
        gate.record(&estimated_row(
            BaselineDeckSource::BundledCedh,
            CommanderBracketTier::Optimized,
        ))
        .unwrap();

        let report = gate
            .finish("test".to_string(), "test".to_string(), Vec::new())
            .unwrap();
        let designed = report
            .designed_population
            .get(&BaselineDeckSource::BundledCedh)
            .unwrap();
        assert_eq!(designed.deck_count, 1);
        assert_eq!(designed.tier_histogram.get(&4), Some(&1));
    }
}
