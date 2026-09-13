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
const DECK_FIELDS: [&str; 4] = ["seat0_deck", "seat1_deck", "seat2_deck", "seat3_deck"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PodSeatConfig {
    pub label: String,
    pub difficulty: AiDifficulty,
    pub tier: CommanderBracketTier,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PodReport {
    pub schema_version: u32,
    pub git_sha: String,
    pub card_data_hash: String,
    pub feed: String,
    pub coverage_authority: String,
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

pub fn compare(baseline: &PodReport, current: &PodReport) -> Result<PodComparison, CompareError> {
    if baseline.schema_version != current.schema_version {
        return Err(CompareError::SchemaMismatch {
            baseline: baseline.schema_version,
            current: current.schema_version,
        });
    }
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
        };
        PodReport::new(
            "sha".to_string(),
            "hash".to_string(),
            "feed.json".to_string(),
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
