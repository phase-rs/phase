//! Stable, count-only pod reports and paired-seed comparison.

use std::collections::BTreeMap;
use std::path::Path;

use engine::game::CommanderBracketTier;
use serde::{Deserialize, Serialize};

use super::{FidelityShortfall, PodMeasurement, PodObservation};
use crate::config::AiDifficulty;
use crate::duel_suite::compare::{CompareError, ReportSide};

pub const POD_REPORT_SCHEMA_VERSION: u32 = 1;
pub const COVERAGE_AUTHORITY: &str = "card_face_gaps";
const DIFFICULTY_FIELDS: [&str; 4] = [
    "seat0_difficulty",
    "seat1_difficulty",
    "seat2_difficulty",
    "seat3_difficulty",
];
const TIER_FIELDS: [&str; 4] = ["seat0_tier", "seat1_tier", "seat2_tier", "seat3_tier"];
const EFFECTIVE_TIER_FIELDS: [&str; 4] = [
    "seat0_effective_tier",
    "seat1_effective_tier",
    "seat2_effective_tier",
    "seat3_effective_tier",
];
const DECK_FIELDS: [&str; 4] = ["seat0_deck", "seat1_deck", "seat2_deck", "seat3_deck"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodSeatConfig {
    pub label: String,
    pub difficulty: AiDifficulty,
    pub tier: CommanderBracketTier,
    #[serde(default)]
    pub effective_tier: Option<CommanderBracketTier>,
}

/// Combo-table identity that can affect the estimator floor and therefore AI
/// policy activation. `PodReport::combo_table` wraps this in `Option` solely to
/// distinguish legacy reports that did not record the configuration at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "measurement", rename_all = "snake_case")]
pub enum PodComboTable {
    Unmeasured,
    Measured {
        snapshot_date: String,
        table_version: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodReport {
    pub schema_version: u32,
    pub git_sha: String,
    pub card_data_hash: String,
    pub feed: String,
    pub coverage_authority: String,
    #[serde(default)]
    pub combo_table: Option<PodComboTable>,
    pub seats: [PodSeatConfig; 4],
    pub seeds: Vec<u64>,
    pub games: Vec<PodGameRow>,
    pub totals: PodTotals,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodGameRow {
    pub seed: u64,
    pub measurement: PodMeasurement,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodTotals {
    pub games: usize,
    pub measured: usize,
    pub withheld_pre_screen: usize,
    pub withheld_runtime: usize,
    pub wins: [usize; 4],
    pub draws: usize,
    pub censored: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PodComparison {
    pub paired_games: usize,
    pub changed_measurements: usize,
}

impl PodReport {
    pub fn new(
        git_sha: String,
        card_data_hash: String,
        feed: String,
        combo_table: PodComboTable,
        seats: [PodSeatConfig; 4],
        seeds: Vec<u64>,
        games: Vec<PodGameRow>,
    ) -> Self {
        let totals = aggregate(&games);
        Self {
            schema_version: POD_REPORT_SCHEMA_VERSION,
            git_sha,
            card_data_hash,
            feed,
            coverage_authority: COVERAGE_AUTHORITY.to_string(),
            combo_table: Some(combo_table),
            seats,
            seeds,
            games,
            totals,
        }
    }
}

pub fn aggregate(games: &[PodGameRow]) -> PodTotals {
    let mut totals = PodTotals {
        games: games.len(),
        ..PodTotals::default()
    };
    for row in games {
        match &row.measurement {
            PodMeasurement::Measured { observation, .. } => {
                totals.measured += 1;
                match observation {
                    PodObservation::Decided {
                        winner: Some(winner),
                        ..
                    } => totals.wins[usize::from(winner.0)] += 1,
                    PodObservation::Decided { winner: None, .. } => totals.draws += 1,
                    PodObservation::Censored { .. } => totals.censored += 1,
                }
            }
            PodMeasurement::Withheld {
                shortfall: FidelityShortfall::PreScreen { .. },
            } => totals.withheld_pre_screen += 1,
            PodMeasurement::Withheld {
                shortfall: FidelityShortfall::Runtime { .. },
            } => totals.withheld_runtime += 1,
        }
    }
    totals
}

/// Compares reports only after guarding seeds and each seat's difficulty, tier, and label.
/// `card_data_hash` is recorded but deliberately unchecked, so changed card data can appear as
/// an unexplained measurement regression. Any automated gate using this function must explicitly
/// decide whether a hash mismatch is a refusal, a warning, or an accepted difference.
pub fn compare(baseline: &PodReport, current: &PodReport) -> Result<PodComparison, CompareError> {
    if baseline.schema_version != current.schema_version {
        return Err(CompareError::SchemaMismatch {
            baseline: baseline.schema_version,
            current: current.schema_version,
        });
    }
    guard_combo_table(&baseline.combo_table, &current.combo_table)?;
    guard(
        "seeds",
        baseline
            .seeds
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(","),
        current
            .seeds
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )?;
    for (index, (baseline_seat, current_seat)) in
        baseline.seats.iter().zip(&current.seats).enumerate()
    {
        guard(
            DIFFICULTY_FIELDS[index],
            format!("{:?}", baseline_seat.difficulty),
            format!("{:?}", current_seat.difficulty),
        )?;
        guard(
            TIER_FIELDS[index],
            baseline_seat.tier.to_string(),
            current_seat.tier.to_string(),
        )?;
        guard_optional_effective_tier(
            EFFECTIVE_TIER_FIELDS[index],
            baseline_seat.effective_tier,
            current_seat.effective_tier,
        )?;
        guard(
            DECK_FIELDS[index],
            baseline_seat.label.clone(),
            current_seat.label.clone(),
        )?;
    }

    let baseline_rows = rows_by_seed(ReportSide::Baseline, &baseline.games)?;
    let current_rows = rows_by_seed(ReportSide::Current, &current.games)?;
    let expected_seeds = joined_seeds(baseline.seeds.iter().copied());
    guard(
        "baseline_game_seeds",
        expected_seeds.clone(),
        joined_seeds(baseline.games.iter().map(|row| row.seed)),
    )?;
    guard(
        "current_game_seeds",
        expected_seeds,
        joined_seeds(current.games.iter().map(|row| row.seed)),
    )?;
    let changed_measurements = baseline
        .seeds
        .iter()
        .filter(|&&seed| {
            let baseline_json = serde_json::to_string(
                &baseline_rows
                    .get(&seed)
                    .expect("guarded baseline row")
                    .measurement,
            )
            .expect("PodMeasurement serializes");
            let current_json = serde_json::to_string(
                &current_rows
                    .get(&seed)
                    .expect("guarded current row")
                    .measurement,
            )
            .expect("PodMeasurement serializes");
            baseline_json != current_json
        })
        .count();
    Ok(PodComparison {
        paired_games: baseline.seeds.len(),
        changed_measurements,
    })
}

fn joined_seeds(seeds: impl Iterator<Item = u64>) -> String {
    seeds
        .map(|seed| seed.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn guard(field: &'static str, baseline: String, current: String) -> Result<(), CompareError> {
    if baseline == current {
        Ok(())
    } else {
        Err(CompareError::WorkloadMismatch {
            field,
            baseline,
            current,
        })
    }
}

fn guard_combo_table(
    baseline: &Option<PodComboTable>,
    current: &Option<PodComboTable>,
) -> Result<(), CompareError> {
    const FIELD: &str = "combo_table_provenance";
    let render = |value: &PodComboTable| match value {
        PodComboTable::Unmeasured => "unmeasured".to_string(),
        PodComboTable::Measured {
            snapshot_date,
            table_version,
        } => format!("snapshot={snapshot_date}, version={table_version}"),
    };
    match (baseline, current) {
        (Some(baseline), Some(current)) => guard(FIELD, render(baseline), render(current)),
        (baseline, current) => Err(CompareError::WorkloadMismatch {
            field: FIELD,
            baseline: baseline
                .as_ref()
                .map_or_else(|| "unknown (re-record the baseline)".to_string(), &render),
            current: current
                .as_ref()
                .map_or_else(|| "unknown (re-record the baseline)".to_string(), render),
        }),
    }
}

fn guard_optional_effective_tier(
    field: &'static str,
    baseline: Option<CommanderBracketTier>,
    current: Option<CommanderBracketTier>,
) -> Result<(), CompareError> {
    match (baseline, current) {
        (Some(baseline), Some(current)) => guard(field, baseline.to_string(), current.to_string()),
        (baseline, current) => Err(CompareError::WorkloadMismatch {
            field,
            baseline: baseline.map_or_else(
                || "unknown (re-record the baseline)".to_string(),
                |tier| tier.to_string(),
            ),
            current: current.map_or_else(
                || "unknown (re-record the baseline)".to_string(),
                |tier| tier.to_string(),
            ),
        }),
    }
}

fn rows_by_seed(
    side: ReportSide,
    games: &[PodGameRow],
) -> Result<BTreeMap<u64, &PodGameRow>, CompareError> {
    let mut rows = BTreeMap::new();
    for row in games {
        if rows.insert(row.seed, row).is_some() {
            return Err(CompareError::DuplicateSeed {
                side,
                matchup_id: "pod".to_string(),
                seed: row.seed,
            });
        }
    }
    Ok(rows)
}

pub fn refusal_markdown(error: &CompareError) -> String {
    crate::duel_suite::refusal_markdown(
        error,
        "Run the same ordered seed list with identical per-seat difficulty, tier, and deck labels.",
    )
}

pub fn load(path: &Path) -> Result<PodReport, CompareError> {
    let file = std::fs::File::open(path)?;
    Ok(serde_json::from_reader(std::io::BufReader::new(file))?)
}

pub fn write(path: &Path, report: &PodReport) -> Result<(), std::io::Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec_pretty(report).expect("PodReport serializes");
    bytes.push(b'\n');
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use engine::types::player::PlayerId;

    use super::*;
    use crate::pod::{PodEvidence, SeatCoverage};

    fn report(tier: CommanderBracketTier) -> PodReport {
        let seat = |label: &str, seat_tier| PodSeatConfig {
            label: label.to_string(),
            difficulty: AiDifficulty::Easy,
            tier: seat_tier,
            effective_tier: Some(seat_tier),
        };
        PodReport::new(
            "sha".to_string(),
            "hash".to_string(),
            "feed.json".to_string(),
            PodComboTable::Unmeasured,
            [
                seat("A", CommanderBracketTier::Core),
                seat("B", tier),
                seat("C", CommanderBracketTier::Core),
                seat("D", CommanderBracketTier::Core),
            ],
            vec![7],
            vec![PodGameRow {
                seed: 7,
                measurement: PodMeasurement::Measured {
                    observation: PodObservation::Decided {
                        turn: 8,
                        winner: Some(PlayerId(0)),
                    },
                    evidence: PodEvidence {
                        seat_coverage: [SeatCoverage::default(); 4],
                        touched_unimplemented: BTreeSet::new(),
                    },
                },
            }],
        )
    }

    #[test]
    fn compare_refuses_on_a_seat_tier_change() {
        let error = compare(
            &report(CommanderBracketTier::Core),
            &report(CommanderBracketTier::Cedh),
        )
        .expect_err("tier change must refuse");
        assert!(matches!(
            &error,
            CompareError::WorkloadMismatch {
                field: "seat1_tier",
                ..
            }
        ));
        assert!(!refusal_markdown(&error).is_empty());
    }

    #[test]
    fn compare_refuses_on_a_seat_effective_tier_change() {
        let baseline = report(CommanderBracketTier::Core);
        let mut current = report(CommanderBracketTier::Core);
        current.seats[1].effective_tier = Some(CommanderBracketTier::Optimized);
        let error = compare(&baseline, &current).expect_err("effective tier change must refuse");
        assert!(matches!(
            error,
            CompareError::WorkloadMismatch {
                field: "seat1_effective_tier",
                ..
            }
        ));
    }

    #[test]
    fn compare_refuses_legacy_unknown_effective_tier() {
        let mut baseline = report(CommanderBracketTier::Core);
        baseline.seats[1].effective_tier = None;
        let error = compare(&baseline, &report(CommanderBracketTier::Core))
            .expect_err("legacy effective tier must require a new baseline");
        assert!(matches!(
            &error,
            CompareError::WorkloadMismatch {
                field: "seat1_effective_tier",
                baseline,
                ..
            } if baseline.contains("re-record")
        ));
    }

    #[test]
    fn compare_refuses_legacy_unknown_combo_provenance() {
        let mut baseline = report(CommanderBracketTier::Core);
        baseline.combo_table = None;
        let error = compare(&baseline, &report(CommanderBracketTier::Core))
            .expect_err("legacy provenance must require a new baseline");
        assert!(matches!(
            &error,
            CompareError::WorkloadMismatch {
                field: "combo_table_provenance",
                baseline,
                ..
            } if baseline.contains("re-record")
        ));
    }

    #[test]
    fn compare_refuses_combo_provenance_change() {
        let baseline = report(CommanderBracketTier::Core);
        let mut current = report(CommanderBracketTier::Core);
        current.combo_table = Some(PodComboTable::Measured {
            snapshot_date: "2026-09-27".to_string(),
            table_version: "2".to_string(),
        });
        let error = compare(&baseline, &current).expect_err("provenance change must refuse");
        assert!(matches!(
            error,
            CompareError::WorkloadMismatch {
                field: "combo_table_provenance",
                ..
            }
        ));
    }

    #[test]
    fn report_wire_shape_carries_no_float() {
        fn assert_integral(value: &serde_json::Value) {
            match value {
                serde_json::Value::Array(values) => values.iter().for_each(assert_integral),
                serde_json::Value::Object(values) => values.values().for_each(assert_integral),
                serde_json::Value::Number(number) => {
                    assert!(number.as_i64().is_some() || number.as_u64().is_some());
                }
                serde_json::Value::Null
                | serde_json::Value::Bool(_)
                | serde_json::Value::String(_) => {}
            }
        }
        assert_integral(
            &serde_json::to_value(report(CommanderBracketTier::Core)).expect("serializes"),
        );
    }
}
