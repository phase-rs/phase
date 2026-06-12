//! Baseline-vs-current comparison of two `SuiteReport` JSON files.
//!
//! Emits a markdown table and returns a `CompareReport` whose `any_fail()`
//! determines the process exit code. This is the CI gate for the duel suite:
//! paired-seed outcome regressions and new matchups that are already failing
//! return a non-zero status.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use super::run::{GameResult, MatchupResult, SuiteReport, SuiteStatus};
use super::{Expected, FeatureKind};

const MIRROR_AVG_TURN_WARN_DELTA: f64 = 3.0;

#[derive(Debug, Clone, Copy)]
pub struct CompareOptions;

impl Default for CompareOptions {
    fn default() -> Self {
        Self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareStatus {
    Pass,
    Warn,
    Fail,
    New,
    Removed,
}

#[derive(Debug, Clone)]
pub struct CompareRow {
    pub matchup_id: String,
    pub exercises: Vec<FeatureKind>,
    pub baseline: Option<MatchupResult>,
    pub current: Option<MatchupResult>,
    pub delta_p0_pp: Option<f32>,
    pub flipped_w_to_l: usize,
    pub flipped_l_to_w: usize,
    pub unchanged: usize,
    pub sign_test_p: Option<f64>,
    pub status: CompareStatus,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CompareReport {
    pub rows: Vec<CompareRow>,
}

impl CompareReport {
    /// True if any row regressed (PASS→FAIL, drift beyond fail threshold, or
    /// new matchup that is already failing). Drives the compare exit code.
    pub fn any_fail(&self) -> bool {
        self.rows
            .iter()
            .any(|r| matches!(r.status, CompareStatus::Fail))
    }
}

#[derive(Debug)]
pub enum CompareError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    SchemaMismatch { baseline: u32, current: u32 },
}

impl std::fmt::Display for CompareError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CompareError::Io(e) => write!(f, "compare I/O error: {e}"),
            CompareError::Parse(e) => write!(f, "compare parse error: {e}"),
            CompareError::SchemaMismatch { baseline, current } => write!(
                f,
                "schema_version mismatch: baseline={baseline} current={current}"
            ),
        }
    }
}

impl std::error::Error for CompareError {}

impl From<std::io::Error> for CompareError {
    fn from(e: std::io::Error) -> Self {
        CompareError::Io(e)
    }
}

impl From<serde_json::Error> for CompareError {
    fn from(e: serde_json::Error) -> Self {
        CompareError::Parse(e)
    }
}

/// Read a `SuiteReport` from a JSON file.
pub fn load_report(path: &Path) -> Result<SuiteReport, CompareError> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let report: SuiteReport = serde_json::from_reader(reader)?;
    Ok(report)
}

/// Core comparison entry point. Takes two reports and an options block;
/// returns a `CompareReport` whose `any_fail()` drives the exit code.
pub fn compare(
    baseline: &SuiteReport,
    current: &SuiteReport,
    options: &CompareOptions,
) -> Result<CompareReport, CompareError> {
    if baseline.schema_version != current.schema_version {
        return Err(CompareError::SchemaMismatch {
            baseline: baseline.schema_version,
            current: current.schema_version,
        });
    }

    // BTreeMap for deterministic iteration order.
    let baseline_by_id: BTreeMap<&str, &MatchupResult> = baseline
        .results
        .iter()
        .map(|r| (r.matchup_id.as_str(), r))
        .collect();
    let current_by_id: BTreeMap<&str, &MatchupResult> = current
        .results
        .iter()
        .map(|r| (r.matchup_id.as_str(), r))
        .collect();

    let mut ids: HashSet<&str> = HashSet::new();
    ids.extend(baseline_by_id.keys().copied());
    ids.extend(current_by_id.keys().copied());
    let mut ids: Vec<&str> = ids.into_iter().collect();
    ids.sort();

    let mut rows = Vec::with_capacity(ids.len());
    for id in ids {
        let baseline_row = baseline_by_id.get(id).copied();
        let current_row = current_by_id.get(id).copied();
        rows.push(classify_row(id, baseline_row, current_row, options));
    }

    Ok(CompareReport { rows })
}

fn classify_row(
    id: &str,
    baseline: Option<&MatchupResult>,
    current: Option<&MatchupResult>,
    _options: &CompareOptions,
) -> CompareRow {
    match (baseline, current) {
        (None, None) => unreachable!("id must appear in at least one report"),
        (Some(b), None) => CompareRow {
            matchup_id: id.to_string(),
            exercises: b.exercises.clone(),
            baseline: Some(b.clone()),
            current: None,
            delta_p0_pp: None,
            flipped_w_to_l: 0,
            flipped_l_to_w: 0,
            unchanged: 0,
            sign_test_p: None,
            status: CompareStatus::Removed,
            reason: Some("matchup removed from current report".to_string()),
        },
        (None, Some(c)) => {
            let (status, reason) = match c.status {
                SuiteStatus::Fail => (
                    CompareStatus::Fail,
                    Some(format!(
                        "new matchup is already failing: {}",
                        c.fail_reason.as_deref().unwrap_or("no reason")
                    )),
                ),
                _ => (CompareStatus::New, Some("matchup is new".to_string())),
            };
            CompareRow {
                matchup_id: id.to_string(),
                exercises: c.exercises.clone(),
                baseline: None,
                current: Some(c.clone()),
                delta_p0_pp: None,
                flipped_w_to_l: 0,
                flipped_l_to_w: 0,
                unchanged: 0,
                sign_test_p: None,
                status,
                reason,
            }
        }
        (Some(b), Some(c)) => {
            let b_rate = winrate(b);
            let c_rate = winrate(c);
            let delta_pp = (c_rate - b_rate) * 100.0;
            let paired = paired_seed_shift(b, c);
            let avg_turn_delta = c.avg_turns - b.avg_turns;

            let (status, reason) = if paired.flipped_w_to_l > paired.flipped_l_to_w
                && paired.sign_test_p.is_some_and(|p| p < 0.05)
            {
                (
                    CompareStatus::Fail,
                    Some(format!(
                        "paired regression: W→L={} L→W={} sign-test p={:.4}",
                        paired.flipped_w_to_l,
                        paired.flipped_l_to_w,
                        paired.sign_test_p.unwrap_or(1.0),
                    )),
                )
            } else if paired.flipped_w_to_l != paired.flipped_l_to_w {
                (
                    CompareStatus::Warn,
                    Some(format!(
                        "paired shift: W→L={} L→W={} sign-test p={:.4}",
                        paired.flipped_w_to_l,
                        paired.flipped_l_to_w,
                        paired.sign_test_p.unwrap_or(1.0),
                    )),
                )
            } else if matches!(c.expected, Expected::Mirror { .. })
                && avg_turn_delta.abs() > MIRROR_AVG_TURN_WARN_DELTA
            {
                (
                    CompareStatus::Warn,
                    Some(format!("mirror avg-turn drift {avg_turn_delta:+.1} turns")),
                )
            } else {
                (CompareStatus::Pass, None)
            };

            CompareRow {
                matchup_id: id.to_string(),
                exercises: c.exercises.clone(),
                baseline: Some(b.clone()),
                current: Some(c.clone()),
                delta_p0_pp: Some(delta_pp),
                flipped_w_to_l: paired.flipped_w_to_l,
                flipped_l_to_w: paired.flipped_l_to_w,
                unchanged: paired.unchanged,
                sign_test_p: paired.sign_test_p,
                status,
                reason,
            }
        }
    }
}

struct PairedSeedShift {
    flipped_w_to_l: usize,
    flipped_l_to_w: usize,
    unchanged: usize,
    sign_test_p: Option<f64>,
}

fn paired_seed_shift(baseline: &MatchupResult, current: &MatchupResult) -> PairedSeedShift {
    let current_by_seed: BTreeMap<u64, &GameResult> =
        current.games.iter().map(|game| (game.seed, game)).collect();
    let mut flipped_w_to_l = 0;
    let mut flipped_l_to_w = 0;
    let mut unchanged = 0;

    for baseline_game in &baseline.games {
        let Some(current_game) = current_by_seed.get(&baseline_game.seed) else {
            continue;
        };
        match (baseline_game.winner, current_game.winner) {
            (Some(0), Some(1)) => flipped_w_to_l += 1,
            (Some(1), Some(0)) => flipped_l_to_w += 1,
            _ => unchanged += 1,
        }
    }

    let flips = flipped_w_to_l + flipped_l_to_w;
    let sign_test_p =
        (flips > 0).then(|| sign_test_mid_p_upper_tail(flips, flipped_w_to_l.max(flipped_l_to_w)));

    PairedSeedShift {
        flipped_w_to_l,
        flipped_l_to_w,
        unchanged,
        sign_test_p,
    }
}

pub fn sign_test_mid_p_upper_tail(n: usize, k: usize) -> f64 {
    ((k + 1)..=n)
        .map(|i| binomial_probability(n, i))
        .sum::<f64>()
        + (binomial_probability(n, k) / 2.0)
}

fn binomial_probability(n: usize, k: usize) -> f64 {
    binomial_coefficient(n, k) as f64 / 2_f64.powi(n as i32)
}

fn binomial_coefficient(n: usize, k: usize) -> u128 {
    let k = k.min(n - k);
    (0..k).fold(1u128, |acc, i| acc * (n - i) as u128 / (i + 1) as u128)
}

fn winrate(r: &MatchupResult) -> f32 {
    let total = r.p0_wins + r.p1_wins + r.draws;
    if total == 0 {
        0.0
    } else {
        r.p0_wins as f32 / total as f32
    }
}

fn status_str(s: CompareStatus) -> &'static str {
    match s {
        CompareStatus::Pass => "PASS",
        CompareStatus::Warn => "WARN",
        CompareStatus::Fail => "FAIL",
        CompareStatus::New => "NEW",
        CompareStatus::Removed => "REMOVED",
    }
}

/// Render a markdown table of the comparison to stdout + emit a summary line.
pub fn print_markdown(report: &CompareReport) {
    println!();
    println!("| matchup | exercises | baseline p0% | current p0% | flips W→L | flips L→W | sign p | status |");
    println!("|---------|-----------|--------------|-------------|-----------|-----------|--------|--------|");
    for row in &report.rows {
        let exercises: Vec<String> = row.exercises.iter().map(|f| format!("{f:?}")).collect();
        let baseline_cell = match &row.baseline {
            Some(b) => format!("{:.0}%", winrate(b) * 100.0),
            None => "—".to_string(),
        };
        let current_cell = match &row.current {
            Some(c) => format!("{:.0}%", winrate(c) * 100.0),
            None => "—".to_string(),
        };
        let sign_p_cell = match row.sign_test_p {
            Some(p) => format!("{p:.4}"),
            None => "—".to_string(),
        };
        println!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |",
            row.matchup_id,
            exercises.join(", "),
            baseline_cell,
            current_cell,
            row.flipped_w_to_l,
            row.flipped_l_to_w,
            sign_p_cell,
            status_str(row.status),
        );
        if let Some(reason) = &row.reason {
            if !matches!(row.status, CompareStatus::Pass) {
                println!("|  ↳ _{reason}_ | | | | | | | |");
            }
        }
    }

    let mut pass = 0usize;
    let mut warn = 0usize;
    let mut fail = 0usize;
    let mut new = 0usize;
    let mut removed = 0usize;
    for row in &report.rows {
        match row.status {
            CompareStatus::Pass => pass += 1,
            CompareStatus::Warn => warn += 1,
            CompareStatus::Fail => fail += 1,
            CompareStatus::New => new += 1,
            CompareStatus::Removed => removed += 1,
        }
    }
    println!("\ncompare: {fail} FAIL, {warn} WARN, {pass} PASS, {new} NEW, {removed} REMOVED");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duel_suite::run::{MatchupResult, SuiteReport, SuiteStatus};
    use crate::duel_suite::{Expected, FeatureKind};

    fn mk_report(results: Vec<MatchupResult>) -> SuiteReport {
        SuiteReport {
            schema_version: 2,
            git_sha: None,
            card_data_hash: None,
            unix_timestamp_secs: 0,
            difficulty: "Easy".into(),
            games_per_matchup: 10,
            base_seed: 0,
            results,
        }
    }

    fn mk_result(id: &str, p0_wins: usize, total: usize, status: SuiteStatus) -> MatchupResult {
        let total = total.max(p0_wins);
        let p1_wins = total - p0_wins;
        let games = (0..total)
            .map(|idx| GameResult {
                seed: idx as u64,
                winner: Some(if idx < p0_wins { 0 } else { 1 }),
                turns: 7,
            })
            .collect();
        MatchupResult {
            matchup_id: id.into(),
            exercises: vec![FeatureKind::AggroPressure],
            p0_label: "A".into(),
            p1_label: "B".into(),
            expected: Expected::Mirror { tolerance: 0.15 },
            p0_wins,
            p1_wins,
            draws: 0,
            games,
            total_turns: 0,
            total_duration_ms: 0,
            avg_turns: 10.0,
            avg_duration_ms: 1000.0,
            status,
            fail_reason: if matches!(status, SuiteStatus::Fail) {
                Some("mock fail".into())
            } else {
                None
            },
            attribution: None,
        }
    }

    #[test]
    fn compare_identity_is_pass() {
        let report = mk_report(vec![mk_result("red-mirror", 5, 10, SuiteStatus::Pass)]);
        let result = compare(&report, &report, &CompareOptions).unwrap();
        assert!(!result.any_fail());
        assert_eq!(result.rows.len(), 1);
        assert_eq!(result.rows[0].status, CompareStatus::Pass);
    }

    #[test]
    fn compare_regression_pass_to_fail_flags_fail() {
        let baseline = mk_report(vec![mk_result("red-mirror", 10, 10, SuiteStatus::Pass)]);
        let current = mk_report(vec![mk_result("red-mirror", 0, 10, SuiteStatus::Fail)]);
        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert!(result.any_fail());
        assert_eq!(result.rows[0].status, CompareStatus::Fail);
        assert!(result.rows[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("paired regression"));
    }

    #[test]
    fn compare_paired_regression_flags_fail() {
        let baseline = mk_report(vec![mk_result("m", 10, 10, SuiteStatus::Pass)]);
        let current = mk_report(vec![mk_result("m", 0, 10, SuiteStatus::Pass)]);
        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert!(result.any_fail());
        assert_eq!(result.rows[0].status, CompareStatus::Fail);
    }

    #[test]
    fn compare_paired_shift_without_significance_warns() {
        let baseline = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);
        let current = mk_report(vec![mk_result("m", 7, 10, SuiteStatus::Pass)]);
        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert!(!result.any_fail());
        assert_eq!(result.rows[0].status, CompareStatus::Warn);
    }

    #[test]
    fn sign_test_mid_p_matches_quick_gate_threshold() {
        let p = sign_test_mid_p_upper_tail(10, 8);

        assert!((p - 0.032_714_843_75).abs() < f64::EPSILON);
        assert!(p < 0.05);
    }

    #[test]
    fn mirror_avg_turn_drift_warns_without_outcome_flips() {
        let baseline_result = mk_result("mirror", 5, 10, SuiteStatus::Pass);
        let mut current_result = baseline_result.clone();
        current_result.avg_turns += MIRROR_AVG_TURN_WARN_DELTA + 0.1;
        let baseline = mk_report(vec![baseline_result]);
        let current = mk_report(vec![current_result]);

        let result = compare(&baseline, &current, &CompareOptions).unwrap();

        assert!(!result.any_fail());
        assert_eq!(result.rows[0].status, CompareStatus::Warn);
        assert!(result.rows[0]
            .reason
            .as_ref()
            .unwrap()
            .contains("avg-turn drift"));
    }

    #[test]
    fn compare_new_matchup_flagged_as_new() {
        let baseline = mk_report(vec![]);
        let current = mk_report(vec![mk_result("x", 5, 10, SuiteStatus::Pass)]);
        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert_eq!(result.rows[0].status, CompareStatus::New);
        assert!(!result.any_fail());
    }

    #[test]
    fn compare_new_failing_matchup_flagged_as_fail() {
        let baseline = mk_report(vec![]);
        let current = mk_report(vec![mk_result("x", 0, 10, SuiteStatus::Fail)]);
        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert_eq!(result.rows[0].status, CompareStatus::Fail);
        assert!(result.any_fail());
    }

    #[test]
    fn compare_removed_matchup_is_informational() {
        let baseline = mk_report(vec![mk_result("gone", 5, 10, SuiteStatus::Pass)]);
        let current = mk_report(vec![]);
        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert_eq!(result.rows[0].status, CompareStatus::Removed);
        assert!(!result.any_fail());
    }

    #[test]
    fn compare_schema_mismatch_returns_error() {
        let mut baseline = mk_report(vec![]);
        baseline.schema_version = 1;
        let current = mk_report(vec![]);
        let err = compare(&baseline, &current, &CompareOptions).unwrap_err();
        assert!(matches!(err, CompareError::SchemaMismatch { .. }));
    }
}
