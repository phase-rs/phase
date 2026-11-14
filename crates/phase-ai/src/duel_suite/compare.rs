//! Baseline-vs-current comparison of two `SuiteReport` JSON files.
//!
//! Emits a markdown table and returns a `CompareReport` whose `any_fail()`
//! determines the process exit code. This is the CI gate for the duel suite:
//! paired-seed outcome regressions and new matchups that are already failing
//! return a non-zero status.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use super::run::{GameResult, MatchupResult, SuiteReport, SuiteStatus};
use super::{refusal_markdown, Expected, FeatureKind};

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
    /// `current.avg_turns - baseline.avg_turns`. Carried on the row, not only inside a reason
    /// string: the verdict chain is first-match-wins, so whichever arm fires suppresses every
    /// other arm's message. Every axis the chain can decide on therefore gets a column of its
    /// own, and this was the last one without.
    pub avg_turn_delta: Option<f64>,
    /// `Some((baseline, current))` when the matchup's own suite verdict changed, `None` when it
    /// held. The third axis this chain decides on, and like the other two it needs a surface of
    /// its own — a row that fails on paired outcomes prints only that reason.
    pub suite_status_shift: Option<(SuiteStatus, SuiteStatus)>,
    pub flipped_w_to_l: usize,
    pub flipped_l_to_w: usize,
    /// `Some(_) → None` — games that stopped resolving. See `PairedSeedShift`.
    pub decisive_to_draw: usize,
    /// `None → Some(_)` — games that started resolving.
    pub draw_to_decisive: usize,
    pub unchanged: usize,
    /// Baseline samples the comparison could not examine, and current samples it never
    /// visited. Carried on the row for the same reason every other axis is: the verdict
    /// chain is first-match-wins, so whichever arm fires suppresses every other arm's
    /// message, and a column is the only surface that survives that suppression.
    pub unpaired_baseline: usize,
    pub unpaired_current: usize,
    pub sign_test_p: Option<f64>,
    pub draw_sign_test_p: Option<f64>,
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

/// Which of the two reports a refusal is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportSide {
    Baseline,
    Current,
}

impl std::fmt::Display for ReportSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            ReportSide::Baseline => "baseline",
            ReportSide::Current => "current",
        })
    }
}

#[derive(Debug)]
pub enum CompareError {
    Io(std::io::Error),
    Parse(serde_json::Error),
    SchemaMismatch {
        baseline: u32,
        current: u32,
    },
    /// One report is internally malformed: pairing keys on the seed number, so a repeated seed
    /// has no single partner. Some games would be counted twice and others never visited.
    DuplicateSeed {
        side: ReportSide,
        matchup_id: String,
        seed: u64,
    },
    /// A configuration that defines what a seed *means* differs between the reports, so
    /// pairing by seed number would compare two unrelated games and call the difference
    /// drift. One parameterized variant rather than a sibling per field, mirroring
    /// `PerfCompareError::WorkloadMismatch`, which already solves this for the perf
    /// comparator.
    WorkloadMismatch {
        field: &'static str,
        baseline: String,
        current: String,
    },
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
            CompareError::WorkloadMismatch {
                field,
                baseline,
                current,
            } => write!(
                f,
                "{field} mismatch: baseline={baseline} current={current} — \
                 the reports describe different workloads and cannot be paired by seed"
            ),
            CompareError::DuplicateSeed {
                side,
                matchup_id,
                seed,
            } => write!(
                f,
                "{side} report repeats seed {seed} in matchup {matchup_id} — the pairing is \
                 ambiguous and some games would go uncompared"
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

/// The first matchup in `report` that lists a seed twice, with that seed.
///
/// `run_suite` derives seeds as `base_seed + matchup_idx * 1000 + game_idx`, so a real report
/// cannot hit this; it fires on a hand-edited or corrupted one.
fn first_duplicate_seed(report: &SuiteReport) -> Option<(&str, u64)> {
    report.results.iter().find_map(|matchup| {
        let mut seen = HashSet::new();
        matchup
            .games
            .iter()
            .find(|game| !seen.insert(game.seed))
            .map(|game| (matchup.matchup_id.as_str(), game.seed))
    })
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

    // Everything below pairs games by seed NUMBER. That is only meaningful while both
    // reports derive their seeds the same way and play them under the same AI, so the two
    // inputs that define what a seed means are checked before any row is classified.
    //
    // `games_per_matchup` IS here, even though it changes how MANY seeds exist rather than
    // what a seed means, so the samples that do pair really are comparable. Counting the
    // unpaired remainder and Warning is not enough: a `Warn` leaves `any_fail` false, so the
    // gate exits 0, so the nightly step SUCCEEDS, and the step that publishes the report is
    // guarded by `if: steps.gate.outcome == 'failure'`. The counters would be written to a file
    // nobody reads — a diagnostic written where nothing is listening is not a surfaced
    // diagnostic. A verdict covering a tenth of the evidence must not be able to Pass, so this
    // is an incompatibility, and the diagnostics survive it: `render_error_markdown` puts the
    // two values on stdout, which is what the workflow captures as the report body, so the
    // failure opens a drift issue that says exactly which knob to turn.
    //
    // The columns stay. A mismatch of this field is now refused, but two reports built at the
    // same `games_per_matchup` can still fail to pair — a crashed matchup, a filter change —
    // and that remainder is still counted rather than skipped.
    //
    // `card_data_hash` is also not here: the committed baseline's hash matches no card-data
    // present on any current checkout, so gating it would fail every run immediately. The
    // perf comparator reaches the same conclusion — it carries card-data hashes as
    // informational report fields rather than as a guard.
    if baseline.base_seed != current.base_seed {
        return Err(CompareError::WorkloadMismatch {
            field: "base_seed",
            baseline: baseline.base_seed.to_string(),
            current: current.base_seed.to_string(),
        });
    }
    if baseline.difficulty != current.difficulty {
        return Err(CompareError::WorkloadMismatch {
            field: "difficulty",
            baseline: baseline.difficulty.clone(),
            current: current.difficulty.clone(),
        });
    }
    if baseline.games_per_matchup != current.games_per_matchup {
        return Err(CompareError::WorkloadMismatch {
            field: "games_per_matchup",
            baseline: baseline.games_per_matchup.to_string(),
            current: current.games_per_matchup.to_string(),
        });
    }

    // After the workload guards: incomparability is the more fundamental refusal, and a report
    // that repeats a seed is malformed rather than incomparable. Both sides are checked from one
    // array so neither can be forgotten.
    for (side, report) in [
        (ReportSide::Baseline, baseline),
        (ReportSide::Current, current),
    ] {
        if let Some((matchup_id, seed)) = first_duplicate_seed(report) {
            return Err(CompareError::DuplicateSeed {
                side,
                matchup_id: matchup_id.to_string(),
                seed,
            });
        }
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
            avg_turn_delta: None,
            suite_status_shift: None,
            flipped_w_to_l: 0,
            flipped_l_to_w: 0,
            decisive_to_draw: 0,
            draw_to_decisive: 0,
            unchanged: 0,
            unpaired_baseline: 0,
            unpaired_current: 0,
            sign_test_p: None,
            draw_sign_test_p: None,
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
                avg_turn_delta: None,
                suite_status_shift: None,
                flipped_w_to_l: 0,
                flipped_l_to_w: 0,
                decisive_to_draw: 0,
                draw_to_decisive: 0,
                unchanged: 0,
                unpaired_baseline: 0,
                unpaired_current: 0,
                sign_test_p: None,
                draw_sign_test_p: None,
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

            // TIER ORDER IS LOAD-BEARING. A precondition enumerated over today's axes is
            // silently falsified by tomorrow's, so each clause below is stated to its exact edge
            // and explicitly scoped to the axes that exist NOW:
            //
            //   0. Every claim below presupposes two COMPARABLE reports. `compare` rejects any
            //      pair whose workload fields disagree, and any report that repeats a seed,
            //      before a row is classified — so such a pair yields no verdict at all,
            //      INCLUDING pairs that would otherwise have produced a Fail. That is intended:
            //      a verdict built by pairing seed numbers across two different workloads was
            //      never meaningful, it merely looked like one.
            //   1. Given comparable reports, nothing that reaches Fail today changes verdict. The
            //      W/L Fail arm is still first and its counters are byte-identical to before, so
            //      it wins every input it used to win.
            //   2. A row whose draw counters are EQUAL, whose seed sets match EXACTLY, and where
            //      NEITHER report's status is `Fail` takes precisely the pre-change path. All
            //      three conjuncts are required, one per guard added since: every new arm below
            //      needs unequal draw counters, or a `Fail` on one side, or an unmatched seed, so
            //      with all three absent none can fire. This is what keeps identity comparisons
            //      and every unaffected regression bit-for-bit unchanged.
            //   3. A W/L *Warn* DOES escalate to Fail — via the draw Fail arm when the draw axis
            //      is significantly negative, and via the status Fail arm when the matchup newly
            //      fails its own suite check. Both escalations are intended: those are the
            //      failures this gate exists to catch, and suppressing either because the win/loss
            //      axis also wobbled insignificantly would reintroduce the same blindness one case
            //      narrower. Pinned by `draw_regression_escalates_an_insignificant_win_loss_warn`
            //      and `status_regression_escalates_an_insignificant_win_loss_warn` — one per
            //      escalating arm, because a claim about an arm that no test exercises is an
            //      unpinned claim.
            //   4. A row that previously Passed with unmatched seeds now Warns. This is the
            //      false-green the unpaired arm exists to close: two reports sharing no seeds at
            //      all scored zero on every counter and passed. Pinned per direction —
            //      `an_unmatched_baseline_sample_warns_instead_of_passing` and
            //      `an_extra_current_sample_warns_instead_of_passing` — because one fixture
            //      covering both directions would let either direction rot unnoticed.
            //
            // Do not reorder to group same-axis arms together: moving either Fail arm below the
            // W/L Warn arm silently demotes its clause-3 escalation back to Warn. Both reorders
            // are covered by the two tests named above.
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
            } else if paired.decisive_to_draw > paired.draw_to_decisive
                && paired.draw_sign_test_p.is_some_and(|p| p < 0.05)
            {
                // Games that used to resolve stopped resolving: the signature of a stalled or
                // looping AI. Asymmetric BY CONSTRUCTION — this arm requires decisive→draw to
                // dominate, so the improvement direction can never reach Fail through it.
                (
                    CompareStatus::Fail,
                    Some(format!(
                        "paired draw regression: decisive→draw={} draw→decisive={} sign-test p={:.4}",
                        paired.decisive_to_draw,
                        paired.draw_to_decisive,
                        paired.draw_sign_test_p.unwrap_or(1.0),
                    )),
                )
            } else if b.status != SuiteStatus::Fail && c.status == SuiteStatus::Fail {
                // The matchup newly fails its OWN suite check (mirror imbalance, expectation
                // violation). Without this arm the comparison read the paired game outcomes and
                // nothing else, so a run could carry `status: "Fail"` on an existing matchup and
                // still exit 0 — measured on `.ab/noC-1.json`, whose enchantress-mirror row is
                // `status: "Fail"` while its compare section reported `0 FAIL, 0 WARN, 3 PASS`.
                //
                // Keyed on `Fail` specifically, not on any status change, because `Fail` is the
                // only status this file already acts on: the new-matchup arm above matches
                // `SuiteStatus::Fail` and treats `Pass`/`Open` alike. Same authority, same
                // vocabulary, extended from new matchups to existing ones.
                (
                    CompareStatus::Fail,
                    Some(format!(
                        "matchup status regressed {:?} → {:?}: {}",
                        b.status,
                        c.status,
                        c.fail_reason.as_deref().unwrap_or("no reason"),
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
            } else if paired.decisive_to_draw != paired.draw_to_decisive {
                // Any imbalance on the draw axis is reported, in either direction. The improvement
                // direction (draw→decisive dominating) lands HERE and never above: a comparator
                // that stayed silent about games that started resolving would be hiding a
                // behavior change, which is the same reason the win/loss tier warns on L→W too.
                (
                    CompareStatus::Warn,
                    Some(format!(
                        "paired draw shift: decisive→draw={} draw→decisive={} sign-test p={:.4}",
                        paired.decisive_to_draw,
                        paired.draw_to_decisive,
                        paired.draw_sign_test_p.unwrap_or(1.0),
                    )),
                )
            } else if b.status == SuiteStatus::Fail {
                // The baseline already recorded this matchup as failing. One arm, two messages,
                // rather than two sibling arms: the axis is `b.status == Fail` and `c.status` is
                // the parameter.
                //
                // Neither case can reach Fail — the arm above requires the regression direction —
                // but neither may be silent either:
                //   * Recovery is a behavior change, and a comparator that hid it would be as
                //     wrong as one that hid a regression (the same reason the W/L tier warns on
                //     L→W and the draw tier warns on draw→decisive).
                //   * STILL failing is reported every run rather than passing quietly. Nothing
                //     revalidates a committed baseline when it is loaded, so a baseline that
                //     already sanctions a failure keeps sanctioning it and that matchup exits 0
                //     forever — a baseline blessed before, or hand-edited around, any guard on the
                //     `--refresh-baseline` write path stays reachable however that path is
                //     guarded.
                //
                // It is a Warn and not a Fail deliberately: the exit code answers "did this change
                // make things worse", and the baseline — however it got that way — already
                // sanctions this state. Making a baseline-sanctioned failure red is a policy call
                // about whether a baseline may bless a failure at all, which belongs with the
                // `--refresh-baseline` guard in `bin/ai_gate.rs`, not here.
                let reason = if c.status == SuiteStatus::Fail {
                    format!(
                        "matchup still failing (baseline also Fail): {}",
                        c.fail_reason.as_deref().unwrap_or("no reason")
                    )
                } else {
                    format!("matchup status recovered {:?} → {:?}", b.status, c.status)
                };
                (CompareStatus::Warn, Some(reason))
            } else if matches!(c.expected, Expected::Mirror { .. })
                && avg_turn_delta.abs() > MIRROR_AVG_TURN_WARN_DELTA
            {
                (
                    CompareStatus::Warn,
                    Some(format!("mirror avg-turn drift {avg_turn_delta:+.1} turns")),
                )
            } else if paired.unpaired_baseline > 0 || paired.unpaired_current > 0 {
                // LAST of the Warn arms, and the position is deliberate in both directions.
                //
                // Not higher: every arm above reports measured drift, which is what the gate
                // exists to find. This one reports reduced COVERAGE — it says the other
                // numbers rest on fewer samples than the reports contain, not that anything
                // regressed. The nightly runs `--games 100` against a 10-game baseline, so
                // this condition is true on every row every night; ranking it above the drift
                // arms would replace every real headline with a coverage notice.
                //
                // Not absent: without it, two reports sharing no seeds at all produce zero on
                // every counter and return Pass, which is the false-green this arm exists to
                // close. Both counters share one arm because the remedy is identical — align
                // the workloads — and splitting them would be two siblings differing only in
                // a direction label, with the direction already carried by its own column.
                (
                    CompareStatus::Warn,
                    Some(format!(
                        "incomplete pairing: {} baseline sample(s) unmatched, {} current sample(s) never compared",
                        paired.unpaired_baseline, paired.unpaired_current,
                    )),
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
                avg_turn_delta: Some(avg_turn_delta),
                suite_status_shift: (b.status != c.status).then_some((b.status, c.status)),
                flipped_w_to_l: paired.flipped_w_to_l,
                flipped_l_to_w: paired.flipped_l_to_w,
                decisive_to_draw: paired.decisive_to_draw,
                draw_to_decisive: paired.draw_to_decisive,
                unchanged: paired.unchanged,
                unpaired_baseline: paired.unpaired_baseline,
                unpaired_current: paired.unpaired_current,
                sign_test_p: paired.sign_test_p,
                draw_sign_test_p: paired.draw_sign_test_p,
                status,
                reason,
            }
        }
    }
}

struct PairedSeedShift {
    flipped_w_to_l: usize,
    flipped_l_to_w: usize,
    /// `Some(_) → None`: a game that used to resolve no longer does. The regression signal —
    /// the signature of a stalled or looping AI.
    decisive_to_draw: usize,
    /// `None → Some(_)`: a game that used to stall now resolves. The improvement signal.
    draw_to_decisive: usize,
    unchanged: usize,
    /// Baseline games whose seed is absent from the current report — samples the comparison
    /// could not examine. Previously a bare `continue`: the sample vanished and every
    /// counter below stayed silent about it, so a pair of reports sharing no seeds at all
    /// scored zero on every axis and returned Pass.
    unpaired_baseline: usize,
    /// Current games whose seed is absent from the baseline. Pairing walks the baseline, so
    /// these were never visited at all — a strictly larger current run could add any number
    /// of losses and no counter would move.
    unpaired_current: usize,
    sign_test_p: Option<f64>,
    /// Sign test on the draw axis, computed exactly like `sign_test_p` is on the win/loss axis.
    draw_sign_test_p: Option<f64>,
}

fn paired_seed_shift(baseline: &MatchupResult, current: &MatchupResult) -> PairedSeedShift {
    let current_by_seed: BTreeMap<u64, &GameResult> =
        current.games.iter().map(|game| (game.seed, game)).collect();
    let mut flipped_w_to_l = 0;
    let mut flipped_l_to_w = 0;
    let mut decisive_to_draw = 0;
    let mut draw_to_decisive = 0;
    let mut unchanged = 0;
    let mut unpaired_baseline = 0;

    // Seeds present on both sides. Counting the intersection lets the current-side leftover
    // be derived by subtraction instead of walked a second time.
    let mut paired = 0usize;

    for baseline_game in &baseline.games {
        let Some(current_game) = current_by_seed.get(&baseline_game.seed) else {
            // NOT a bare `continue` any more. A skipped sample is a sample the comparison
            // could not examine, and staying silent about it is how a partial comparison
            // reported Pass with every counter at zero.
            unpaired_baseline += 1;
            continue;
        };
        paired += 1;
        // EXHAUSTIVE, no `_` fallback. The wildcard this replaces is how the decisive→draw class
        // became invisible: it swept `Some(_) → None` into `unchanged`, so a matchup could lose
        // most of its winners and the comparison would report no movement at all. Listing every
        // shape means a future `winner` representation breaks the build instead of silently
        // rejoining `unchanged`.
        match (baseline_game.winner, current_game.winner) {
            (Some(0), Some(1)) => flipped_w_to_l += 1,
            (Some(1), Some(0)) => flipped_l_to_w += 1,
            (Some(_), None) => decisive_to_draw += 1,
            (None, Some(_)) => draw_to_decisive += 1,
            // Same winner, or drawn on both sides. Non-0/1 seat pairs land here as they always
            // have — the duel suite is two-player, and changing that classification is out of
            // this change's scope.
            (Some(_), Some(_)) | (None, None) => unchanged += 1,
        }
    }

    let flips = flipped_w_to_l + flipped_l_to_w;
    let sign_test_p =
        (flips > 0).then(|| sign_test_mid_p_upper_tail(flips, flipped_w_to_l.max(flipped_l_to_w)));

    let draw_flips = decisive_to_draw + draw_to_decisive;
    let draw_sign_test_p = (draw_flips > 0)
        .then(|| sign_test_mid_p_upper_tail(draw_flips, decisive_to_draw.max(draw_to_decisive)));

    // Current games never visited by the loop above, because pairing walks the baseline.
    // `compare` refuses a report that repeats a seed, so `paired` cannot exceed the map's
    // length and this subtraction is exact; `saturating_sub` keeps the function total for the
    // unit tests that drive it on unvalidated input.
    let unpaired_current = current_by_seed.len().saturating_sub(paired);

    PairedSeedShift {
        flipped_w_to_l,
        flipped_l_to_w,
        decisive_to_draw,
        draw_to_decisive,
        unchanged,
        unpaired_baseline,
        unpaired_current,
        sign_test_p,
        draw_sign_test_p,
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

/// The column headers, in order. Named once so tests can assert that every axis the verdict chain
/// decides on has a surface here — see `COLUMNS` usage in `render_markdown` and the invariant below.
const COLUMNS: &[&str] = &[
    "matchup",
    "exercises",
    "baseline p0%",
    "current p0%",
    "flips W→L",
    "flips L→W",
    "sign p",
    "dec→draw",
    "draw→dec",
    "draw sign p",
    "unpaired base",
    "unpaired cur",
    "Δ avg turns",
    "suite status",
    "status",
];

/// Build the markdown table as a string.
///
/// INVARIANT: every axis the verdict chain can decide on has a column here. The chain is
/// first-match-wins, so whichever arm fires suppresses every other arm's reason string — a row that
/// warns on the win/loss axis printed only the W/L reason and hid its draw movement, and a row that
/// warns on the draw axis hid its mirror avg-turn drift the same way. Columns are the only surface
/// that survives that suppression, so each axis owns one: W/L flips + sign p, draw counters + draw
/// sign p, avg-turn delta, and suite status. **Adding a verdict arm means adding a column.**
///
/// Separated from `print_markdown` so that invariant is enforced by tests rather than asserted in a
/// comment: `markdown_has_a_column_for_every_verdict_axis` and `markdown_rows_are_rectangular` read
/// this string. When it was inlined in a `println!`, both new columns could be deleted with the
/// whole suite still green.
fn render_markdown(report: &CompareReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("| {} |\n", COLUMNS.join(" | ")));
    out.push_str(&format!(
        "|{}|\n",
        COLUMNS
            .iter()
            .map(|c| "-".repeat(c.chars().count() + 2))
            .collect::<Vec<_>>()
            .join("|")
    ));
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
        let draw_sign_p_cell = match row.draw_sign_test_p {
            Some(p) => format!("{p:.4}"),
            None => "—".to_string(),
        };
        let avg_turn_cell = match row.avg_turn_delta {
            Some(d) => format!("{d:+.1}"),
            None => "—".to_string(),
        };
        // Falls back to the CURRENT status when nothing shifted, so the column is populated on
        // every row that has a current report — including new matchups, whose verdict is decided
        // from `c.status` but which have no shift to show. A blank cell under a status-decided
        // verdict would break the invariant above.
        let suite_status_cell = match (row.suite_status_shift, row.current.as_ref()) {
            (Some((before, after)), _) => format!("{before:?}→{after:?}"),
            (None, Some(c)) => format!("{:?}", c.status),
            (None, None) => "—".to_string(),
        };
        let cells = [
            md_cell(&row.matchup_id),
            md_cell(&exercises.join(", ")),
            baseline_cell,
            current_cell,
            row.flipped_w_to_l.to_string(),
            row.flipped_l_to_w.to_string(),
            sign_p_cell,
            row.decisive_to_draw.to_string(),
            row.draw_to_decisive.to_string(),
            draw_sign_p_cell,
            row.unpaired_baseline.to_string(),
            row.unpaired_current.to_string(),
            avg_turn_cell,
            suite_status_cell,
            status_str(row.status).to_string(),
        ];
        debug_assert_eq!(cells.len(), COLUMNS.len());
        out.push_str(&format!("| {} |\n", cells.join(" | ")));
        if let Some(reason) = &row.reason {
            if !matches!(row.status, CompareStatus::Pass) {
                // Reason spans one labelled cell plus blanks for the rest, so the row stays
                // rectangular no matter how many columns the table has.
                out.push_str(&format!(
                    "|  ↳ _{}_ |{}\n",
                    md_cell(reason),
                    " |".repeat(COLUMNS.len() - 1)
                ));
            }
        }
    }
    out
}

/// Encode a report-provided string for one markdown table cell.
///
/// Every cell below that originates in a report rather than in this file goes through here.
/// Report fields are free-form and arrive from JSON — `fail_reason` is whatever the suite
/// wrote, and `ai_duel compare` will read whatever file it is handed — so a `|` silently adds
/// a column to that row and a newline ends the row early. Either way the table stops being
/// rectangular exactly when a matchup is already failing, which is the moment the diagnostics
/// are actually read.
///
/// Applied uniformly rather than only to the fields that look risky today: deciding per field
/// means re-deciding every time a field is added, and one of those decisions will be wrong.
fn md_cell(text: &str) -> String {
    // ORDER IS LOAD-BEARING: backslashes first, then pipes. The reverse turns the input `\|`
    // into `\\|`, and a markdown parser reads that as an escaped backslash followed by a LIVE
    // separator, so the very input that looks pre-escaped is the one that breaks the row.
    // Escaping backslashes first makes every backslash run even before any `\|` is introduced,
    // so no emitted `|` can ever be preceded by an odd run.
    text.replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\n', '\r'], " ")
}

/// Everything a successful comparison writes to stdout: the table, plus the counts line.
fn render_stdout(report: &CompareReport) -> String {
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
    format!(
        "\n{}\ncompare: {fail} FAIL, {warn} WARN, {pass} PASS, {new} NEW, {removed} REMOVED\n",
        render_markdown(report)
    )
}

/// Render the comparison table to stdout + emit a summary line.
pub fn print_markdown(report: &CompareReport) {
    print!("{}", render_stdout(report));
}

/// The report body for a comparison that could not be made at all.
///
/// A refused comparison is the case most likely to be read by someone who did not run it: the
/// nightly captures stdout into `target/ai-gate-report.md` and posts it as a drift issue.
///
/// The workflow's empty-file abort is unreachable on this path: `run_suite` prints the suite's
/// own table to stdout (`run.rs`, `print_markdown_table`) before the baseline is ever loaded, so
/// the file always has content. What a stderr-only refusal produces is worse to read than an
/// empty file — a red job whose issue body is a table of PASSing matchups and no statement of
/// what failed.
pub fn render_error_markdown(err: &CompareError) -> String {
    let remedy = match err {
        CompareError::WorkloadMismatch {
            field,
            baseline,
            current,
        } => format!(
            "The two reports were produced under different `{field}`, so their seeds do not \
             denote the same games and no verdict can be built by pairing them. This is not \
             drift: nothing was measured.\n\n\
             Two remedies. Pick the workload this gate should measure at, then make EVERY \
             invocation that reads this baseline use it — the baseline file is shared, so \
             re-recording it to suit one job starts failing this same check in every other job \
             that compares against it at a different workload.\n\n\
             1. **Move the baseline to `{current}`** — re-record it under the workload this run \
             used (`cargo ai-gate --refresh-baseline` with this run's flags), AND align every \
             other invocation that compares against it.\n\
             2. **Move this run to `{baseline}`** — invoke the gate under the workload the \
             baseline was recorded at (for `games_per_matchup`, the `--games` flag). Touches \
             nothing else.\n\n\
             Until one of them is done this gate fails every run, by design: a verdict built \
             from a fraction of the sample is not a verdict."
        ),
        CompareError::DuplicateSeed {
            side,
            matchup_id,
            seed,
        } => format!(
            "The {side} report lists seed `{seed}` more than once in matchup `{matchup_id}`. \
             Pairing keys on the seed number, so a repeated seed has no single partner: one \
             game would be compared twice and another never visited at all. Nothing was \
             measured — this is not drift.\n\n\
             Regenerate the report rather than editing it: `cargo ai-gate --refresh-baseline` \
             for a baseline, a fresh gate run for a current report."
        ),
        CompareError::SchemaMismatch { .. } => "The baseline predates the current report format. \
             Re-record it with `cargo ai-gate --refresh-baseline`."
            .to_string(),
        // "report", not "baseline": `ai-duel compare` now renders this arm for a failure on
        // EITHER input, so naming the baseline would send the reader to the wrong file half the
        // time. The side and the exact path are on stderr, where they are known.
        CompareError::Io(_) | CompareError::Parse(_) => {
            "The report could not be read. Check the path named on stderr, and that the file is \
             the JSON a previous `--refresh-baseline` wrote."
                .to_string()
        }
    };
    refusal_markdown(err, &remedy)
}

/// What the gate prints on stdout and what it exits with, decided together.
///
/// These two are one decision, not two: the nightly opens its drift issue only when the exit is
/// non-zero, and aborts with "failed without a drift report" when stdout was empty, so a change
/// that satisfies either alone silently disables the other. Returning both from one function is
/// what lets a test bind the pair — `main` only prints and exits.
pub fn gate_verdict(comparison: &Result<CompareReport, CompareError>) -> (String, i32) {
    match comparison {
        Ok(report) if report.any_fail() => (render_stdout(report), 1),
        Ok(report) => (render_stdout(report), 0),
        Err(err) => (render_error_markdown(err), 2),
    }
}

/// Write the gate's stdout body and return the process exit code.
///
/// Every binary that compares two suite reports ends in these same two statements, which a unit
/// test on `gate_verdict` cannot see: a `main` that printed to stderr, or exited 0 on a refusal,
/// would revert the whole fix with the suite green. Both halves live here so that surface is one
/// shared function instead of one copy per binary — and `tests/gate_cli.rs` drives it through a
/// real process, so the pairing is bound at the boundary CI actually redirects, not just at the
/// library call below it.
pub fn emit_gate_verdict(comparison: &Result<CompareReport, CompareError>) -> i32 {
    let (body, code) = gate_verdict(comparison);
    print!("{body}");
    code
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

    /// Build a matchup from explicit `(seed, winner, turns)` rows, so a fixture can carry a
    /// REAL recorded run instead of the synthetic win/loss ladder `mk_result` generates.
    fn mk_result_from_games(id: &str, rows: &[(u64, Option<u8>, u32)]) -> MatchupResult {
        let games: Vec<GameResult> = rows
            .iter()
            .map(|(seed, winner, turns)| GameResult {
                seed: *seed,
                winner: *winner,
                turns: *turns,
            })
            .collect();
        let p0_wins = games.iter().filter(|g| g.winner == Some(0)).count();
        let p1_wins = games.iter().filter(|g| g.winner == Some(1)).count();
        let draws = games.iter().filter(|g| g.winner.is_none()).count();
        MatchupResult {
            matchup_id: id.into(),
            exercises: vec![FeatureKind::AggroPressure],
            p0_label: "A".into(),
            p1_label: "B".into(),
            expected: Expected::Mirror { tolerance: 0.15 },
            p0_wins,
            p1_wins,
            draws,
            games,
            total_turns: 0,
            total_duration_ms: 0,
            avg_turns: 10.0,
            avg_duration_ms: 1000.0,
            status: SuiteStatus::Pass,
            fail_reason: None,
            attribution: None,
        }
    }

    /// **P2 — the paired-seed shift is blind to decisive→draw.**
    ///
    /// HISTORICAL, not synthetic. These ten rows are the committed `suite-baseline.json`'s
    /// `enchantress-mirror` games paired by seed against a real recorded gate run
    /// (`.ab/noC-1.json`, the A+B+D leg of #6969). Eight of the ten stopped having a winner —
    /// baseline 4 p0 / 6 p1 / 0 draws became 1 / 1 / **8**.
    ///
    /// `paired_seed_shift` only recognizes `Some(0)→Some(1)` and `Some(1)→Some(0)`; every
    /// `Some(_)→None` falls into the `_` arm and is tallied as UNCHANGED. So `flips == 0`,
    /// `sign_test_p == None`, and the comparison PASSES a matchup in which 80% of the games
    /// stopped resolving. That is the exact signature of a stalled or looping AI, and a branch
    /// that drew every game would pass this gate.
    #[test]
    fn paired_seed_shift_counts_decisive_to_draw_as_a_shift() {
        let baseline = mk_result_from_games(
            "enchantress-mirror",
            &[
                (10593729, Some(1), 10),
                (10593730, Some(0), 18),
                (10593731, Some(1), 12),
                (10593732, Some(1), 14),
                (10593733, Some(1), 11),
                (10593734, Some(0), 15),
                (10593735, Some(0), 15),
                (10593736, Some(0), 14),
                (10593737, Some(1), 18),
                (10593738, Some(1), 19),
            ],
        );
        let current = mk_result_from_games(
            "enchantress-mirror",
            &[
                (10593729, None, 8),
                (10593730, None, 12),
                (10593731, Some(1), 12),
                (10593732, None, 11),
                (10593733, None, 9),
                (10593734, None, 12),
                (10593735, Some(0), 15),
                (10593736, None, 13),
                (10593737, None, 17),
                (10593738, None, 9),
            ],
        );

        // PREMISE: the fixture really carries the shift it claims (8 lost winners, 2 kept).
        assert_eq!(
            baseline.draws, 0,
            "premise: the baseline matchup had no draws"
        );
        assert_eq!(current.draws, 8, "premise: the recorded run drew 8 of 10");

        let shift = paired_seed_shift(&baseline, &current);

        // Before this change these eight landed in a `_ => unchanged` arm and `unchanged` read 10.
        assert_eq!(
            shift.unchanged, 2,
            "eight games stopped having a winner; they are a SHIFT, not 'unchanged'"
        );
        assert_eq!(
            (shift.decisive_to_draw, shift.draw_to_decisive),
            (8, 0),
            "the shift is eight decisive→draw, none back"
        );
        // The W/L axis is genuinely silent here, and that is CORRECT — no game changed which
        // player won. Pinned so the fix is read as adding a second axis, not as repairing the
        // first: a fix that started reporting W→L flips for these rows would be wrong.
        assert_eq!(
            (shift.flipped_w_to_l, shift.flipped_l_to_w),
            (0, 0),
            "PIN: no directional win/loss flip occurred; the shift is entirely decisive→draw"
        );
    }

    /// The end-to-end consequence: the matchup must NOT pass.
    ///
    /// This is the claim that matters to a reviewer — not that a private counter is wrong, but
    /// that `cargo ai-gate`'s comparison reported no problem for a run in which 8 of 10 games
    /// stopped resolving. Both matchups are marked `Pass` in their own right (the suite-status
    /// half is a separate check), so this row isolates the COMPARISON's verdict.
    ///
    /// Measured before the fix: `CompareStatus::Pass`.
    #[test]
    fn compare_fails_a_matchup_that_lost_eight_of_ten_winners() {
        let rows_before: &[(u64, Option<u8>, u32)] = &[
            (10593729, Some(1), 10),
            (10593730, Some(0), 18),
            (10593731, Some(1), 12),
            (10593732, Some(1), 14),
            (10593733, Some(1), 11),
            (10593734, Some(0), 15),
            (10593735, Some(0), 15),
            (10593736, Some(0), 14),
            (10593737, Some(1), 18),
            (10593738, Some(1), 19),
        ];
        let rows_after: &[(u64, Option<u8>, u32)] = &[
            (10593729, None, 8),
            (10593730, None, 12),
            (10593731, Some(1), 12),
            (10593732, None, 11),
            (10593733, None, 9),
            (10593734, None, 12),
            (10593735, Some(0), 15),
            (10593736, None, 13),
            (10593737, None, 17),
            (10593738, None, 9),
        ];
        let baseline = mk_report(vec![mk_result_from_games(
            "enchantress-mirror",
            rows_before,
        )]);
        let current = mk_report(vec![mk_result_from_games("enchantress-mirror", rows_after)]);

        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert_eq!(result.rows.len(), 1, "premise: exactly one paired matchup");

        // n=8, k=8 ⇒ mid-p = 1/512 ≈ 0.00195 < 0.05, so this reaches Fail, not merely Warn.
        assert_eq!(
            result.rows[0].status,
            CompareStatus::Fail,
            "8 of 10 games stopped having a winner — the comparison must not report Pass; reason={:?}",
            result.rows[0].reason
        );
        // The counters and their ORDER are pinned, not just the word "draw": transposing them in
        // the reason string would report the recorded incident as an improvement.
        assert!(
            result.rows[0]
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("decisive→draw=8 draw→decisive=0")),
            "reason={:?}",
            result.rows[0].reason
        );
        assert!(result.any_fail(), "the compare exit code must reflect it");
    }

    /// TRIVIALIZE control. A fix that failed on ANY draw-axis movement, ignoring direction and
    /// significance, would pass every other test in this module — including both demonstration
    /// rows — and would be wrong. Games that STARTED resolving are an improvement.
    ///
    /// Same magnitude as the regression row (8 of 10), opposite direction. Must be Warn: reported
    /// because any imbalance is worth surfacing, never Fail.
    #[test]
    fn draw_to_decisive_improvement_warns_but_never_fails() {
        let stalled: &[(u64, Option<u8>, u32)] = &[
            (1, None, 8),
            (2, None, 12),
            (3, Some(1), 12),
            (4, None, 11),
            (5, None, 9),
            (6, None, 12),
            (7, Some(0), 15),
            (8, None, 13),
            (9, None, 17),
            (10, None, 9),
        ];
        let resolving: &[(u64, Option<u8>, u32)] = &[
            (1, Some(1), 10),
            (2, Some(0), 18),
            (3, Some(1), 12),
            (4, Some(1), 14),
            (5, Some(1), 11),
            (6, Some(0), 15),
            (7, Some(0), 15),
            (8, Some(0), 14),
            (9, Some(1), 18),
            (10, Some(1), 19),
        ];
        let baseline = mk_report(vec![mk_result_from_games("enchantress-mirror", stalled)]);
        let current = mk_report(vec![mk_result_from_games("enchantress-mirror", resolving)]);

        let result = compare(&baseline, &current, &CompareOptions).unwrap();
        assert_eq!(
            result.rows[0].draw_to_decisive, 8,
            "premise: this fixture really is the improvement direction"
        );
        assert_eq!(
            result.rows[0].decisive_to_draw, 0,
            "premise: nothing regressed on the draw axis"
        );
        // The statistic is computed on `max(decisive_to_draw, draw_to_decisive)`, so it must read
        // the DOMINANT direction whichever one that is. Replacing the `max` with `decisive_to_draw`
        // leaves the verdict correct (the Fail arm's dominance guard protects it) but prints
        // 0.9980 instead of 0.0020 in the reason string and the `draw sign p` column.
        assert!(
            result.rows[0].draw_sign_test_p.is_some_and(|p| p < 0.05),
            "the reported statistic must describe the 8-0 shift, not its complement; got {:?}",
            result.rows[0].draw_sign_test_p
        );
        assert_eq!(
            result.rows[0].status,
            CompareStatus::Warn,
            "improvement is reported, never failed; reason={:?}",
            result.rows[0].reason
        );
        assert!(!result.any_fail(), "an improvement must not fail the gate");
    }

    /// **The escalation the tier order produces.** The draw Fail arm sits above the win/loss
    /// Warn arm, so a draw regression escalates a verdict the win/loss axis alone would only
    /// have warned on.
    ///
    /// The escalation is intended — 8 of 11 games ceasing to resolve is the failure this gate
    /// exists to catch, and suppressing it because the win/loss axis also wobbled insignificantly
    /// would reintroduce the same blindness one case narrower.
    ///
    /// The asserts below are what attribute the Fail to the draw axis: `sign_test_p > 0.05` means
    /// the W/L Fail arm cannot fire, `flipped_w_to_l != flipped_l_to_w` means the W/L axis reaches
    /// its Warn arm, and the status Fail arm is out because both fixtures report
    /// `SuiteStatus::Pass` — which the `reason` assert then confirms by name.
    #[test]
    fn draw_regression_escalates_an_insignificant_win_loss_warn() {
        let before: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(0), 10),
            (3, Some(0), 10),
            (4, Some(1), 10),
            (5, Some(1), 10),
            (6, Some(1), 10),
            (7, Some(1), 10),
            (8, Some(1), 10),
            (9, Some(1), 10),
            (10, Some(1), 10),
            (11, Some(1), 10),
        ];
        let after: &[(u64, Option<u8>, u32)] = &[
            (1, Some(1), 10),
            (2, Some(1), 10),
            (3, Some(1), 10),
            (4, None, 10),
            (5, None, 10),
            (6, None, 10),
            (7, None, 10),
            (8, None, 10),
            (9, None, 10),
            (10, None, 10),
            (11, None, 10),
        ];
        let baseline = mk_report(vec![mk_result_from_games("m", before)]);
        let current = mk_report(vec![mk_result_from_games("m", after)]);
        let row = &compare(&baseline, &current, &CompareOptions).unwrap().rows[0];

        // PREMISE 1: the W/L Fail arm cannot be what fired — 3 flips one way is p=0.0625.
        assert_eq!((row.flipped_w_to_l, row.flipped_l_to_w), (3, 0));
        assert!(
            row.sign_test_p.is_some_and(|p| p > 0.05),
            "premise: the win/loss axis is INSIGNIFICANT, so the W/L Fail arm is out; got {:?}",
            row.sign_test_p
        );
        // PREMISE 2: the old chain therefore reached the W/L Warn arm (`w2l != l2w`).
        assert_ne!(
            row.flipped_w_to_l, row.flipped_l_to_w,
            "premise: this input used to land on the win/loss Warn arm"
        );

        assert_eq!(
            row.status,
            CompareStatus::Fail,
            "a significant draw regression escalates a W/L Warn to Fail; reason={:?}",
            row.reason
        );
        assert!(
            row.reason.as_deref().is_some_and(|r| r.contains("draw")),
            "the draw arm won, not the W/L Warn arm below it; reason={:?}",
            row.reason
        );
    }

    /// First-match-wins means a firing arm suppresses every other arm's reason string. Here the
    /// draw Warn arm shadows the mirror avg-turn Warn arm: same status, different message, and
    /// the drift magnitude would vanish entirely if the row did not carry it.
    ///
    /// That is why `avg_turn_delta` is a field and a column rather than only a reason string —
    /// the same argument that put the draw counters in the table.
    #[test]
    fn mirror_drift_magnitude_survives_a_shadowing_reason() {
        let before: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(1), 10),
            (3, Some(0), 10),
            (4, Some(1), 10),
        ];
        let after: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(1), 10),
            (3, Some(0), 10),
            (4, None, 22),
        ];
        let baseline = mk_report(vec![mk_result_from_games("mirror", before)]);
        let mut current_result = mk_result_from_games("mirror", after);
        current_result.avg_turns = 16.0; // baseline is 10.0 → +6.0, past MIRROR_AVG_TURN_WARN_DELTA
        let current = mk_report(vec![current_result]);
        let row = &compare(&baseline, &current, &CompareOptions).unwrap().rows[0];

        // PREMISE: the mirror avg-turn arm WOULD have fired — it is genuinely shadowed, not absent.
        assert!(matches!(
            row.current.as_ref().unwrap().expected,
            Expected::Mirror { .. }
        ));
        assert_eq!(row.avg_turn_delta, Some(6.0));
        assert!(
            row.avg_turn_delta.unwrap().abs() > MIRROR_AVG_TURN_WARN_DELTA,
            "premise: the drift is past the warn threshold"
        );
        // PREMISE: one game stopped resolving, but not significantly (n=1 ⇒ p=0.25).
        assert_eq!((row.decisive_to_draw, row.draw_to_decisive), (1, 0));
        assert!(row.draw_sign_test_p.is_some_and(|p| p > 0.05));

        assert_eq!(row.status, CompareStatus::Warn);
        assert!(
            row.reason
                .as_deref()
                .is_some_and(|r| r.contains("decisive→draw=1 draw→decisive=0")),
            "the draw arm shadows the avg-turn arm's message, with its counters in order; reason={:?}",
            row.reason
        );
        assert!(
            !row.reason.as_deref().unwrap().contains("avg-turn"),
            "premise of this test: the avg-turn message really is suppressed"
        );
    }

    /// The no-draw-shift control: with the draw counters equal, both draw guards (`>` and `!=`)
    /// are false and the chain falls through as it did before this change.
    ///
    /// Scope: the fixture is an identity comparison, so ALL axes are flat, and it therefore pins
    /// only the draw guards' inertness — not the full precondition of invariant 2, and not the
    /// tier order, which `draw_regression_escalates_an_insignificant_win_loss_warn` pins.
    #[test]
    fn compare_without_draw_shift_is_unaffected() {
        let rows: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(0), 11),
            (3, Some(1), 12),
            (4, Some(1), 13),
        ];
        let report = mk_report(vec![mk_result_from_games("red-mirror", rows)]);
        let result = compare(&report, &report, &CompareOptions).unwrap();

        assert_eq!(
            (
                result.rows[0].decisive_to_draw,
                result.rows[0].draw_to_decisive
            ),
            (0, 0),
            "premise: the draw axis is flat, so the new tiers must be inert"
        );
        assert_eq!(result.rows[0].draw_sign_test_p, None);
        assert_eq!(result.rows[0].status, CompareStatus::Pass);
        assert!(!result.any_fail());
    }

    /// **The status axis, isolated.** An existing matchup that newly fails its OWN suite check
    /// must fail the comparison. Before this arm existed, `classify_row`'s paired branch read the
    /// game outcomes and nothing else, so a run could carry `status: "Fail"` on an existing
    /// matchup and still exit 0.
    ///
    /// Every outcome axis is held FLAT here (premise-asserted below), so the status axis is the
    /// only thing that can produce a verdict — which is what makes this the drop-mutant's target.
    #[test]
    fn status_regression_to_fail_flags_a_matchup_with_unchanged_outcomes() {
        let games: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(1), 10),
            (3, Some(0), 10),
            (4, Some(1), 10),
        ];
        let baseline = mk_report(vec![mk_result_from_games("enchantress-mirror", games)]);
        let mut failing = mk_result_from_games("enchantress-mirror", games);
        failing.status = SuiteStatus::Fail;
        // Verbatim from `.ab/noC-1.json`'s enchantress-mirror row.
        failing.fail_reason =
            Some("mirror imbalance: p0=0.10, Wilson 95% CI [0.02, 0.40] excludes 0.50".into());
        let current = mk_report(vec![failing]);
        let row = &compare(&baseline, &current, &CompareOptions).unwrap().rows[0];

        // PREMISE: every outcome axis is flat, so nothing above the status arm can fire.
        assert_eq!((row.flipped_w_to_l, row.flipped_l_to_w), (0, 0));
        assert_eq!((row.decisive_to_draw, row.draw_to_decisive), (0, 0));
        assert_eq!(row.avg_turn_delta, Some(0.0));
        assert_eq!(
            row.suite_status_shift,
            Some((SuiteStatus::Pass, SuiteStatus::Fail))
        );

        assert_eq!(
            row.status,
            CompareStatus::Fail,
            "a matchup that started failing its own suite check must fail the comparison; reason={:?}",
            row.reason
        );
        assert!(
            row.reason
                .as_deref()
                .is_some_and(|r| r.contains("Pass → Fail") && r.contains("mirror imbalance")),
            "the reason names the shift AND carries the matchup's own fail_reason; got {:?}",
            row.reason
        );
    }

    /// **The recorded incident, whole — and it had TWO independent holes.**
    ///
    /// This is `.ab/noC-1.json`'s `enchantress-mirror` row as recorded: the eight games that
    /// stopped resolving AND `status: "Fail"` with its Wilson-CI reason. The run's compare section
    /// nonetheless printed `0 FAIL, 0 WARN, 3 PASS`, because neither the draw axis nor the suite
    /// status axis existed in the comparison. Both are asserted present so a future edit that
    /// closes one and reopens the other cannot pass this row.
    ///
    /// The reason string is the draw one: the draw Fail arm sits above the status Fail arm, so the
    /// more specific outcome diagnosis wins. The status hole is still visible on the row via
    /// `suite_status_shift` — which is exactly why every axis carries a field and a column instead
    /// of relying on the first-match-wins reason.
    #[test]
    fn noc1_enchantress_row_carries_both_holes() {
        let before: &[(u64, Option<u8>, u32)] = &[
            (10593729, Some(1), 10),
            (10593730, Some(0), 18),
            (10593731, Some(1), 12),
            (10593732, Some(1), 14),
            (10593733, Some(1), 11),
            (10593734, Some(0), 15),
            (10593735, Some(0), 15),
            (10593736, Some(0), 14),
            (10593737, Some(1), 18),
            (10593738, Some(1), 19),
        ];
        let after: &[(u64, Option<u8>, u32)] = &[
            (10593729, None, 8),
            (10593730, None, 12),
            (10593731, Some(1), 12),
            (10593732, None, 11),
            (10593733, None, 9),
            (10593734, None, 12),
            (10593735, Some(0), 15),
            (10593736, None, 13),
            (10593737, None, 17),
            (10593738, None, 9),
        ];
        let baseline = mk_report(vec![mk_result_from_games("enchantress-mirror", before)]);
        let mut recorded = mk_result_from_games("enchantress-mirror", after);
        recorded.status = SuiteStatus::Fail;
        recorded.fail_reason =
            Some("mirror imbalance: p0=0.10, Wilson 95% CI [0.02, 0.40] excludes 0.50".into());
        let current = mk_report(vec![recorded]);
        let report = compare(&baseline, &current, &CompareOptions).unwrap();
        let row = &report.rows[0];

        // HOLE 1: eight games stopped resolving.
        assert_eq!((row.decisive_to_draw, row.draw_to_decisive), (8, 0));
        // HOLE 2: the matchup failed its own suite check.
        assert_eq!(
            row.suite_status_shift,
            Some((SuiteStatus::Pass, SuiteStatus::Fail))
        );

        assert_eq!(row.status, CompareStatus::Fail);
        assert!(report.any_fail(), "the recorded run must not exit 0");
        assert!(
            row.reason.as_deref().is_some_and(|r| r.contains("draw")),
            "the outcome diagnosis is the more specific one and wins; reason={:?}",
            row.reason
        );
    }

    /// Status-axis asymmetry, same shape as the draw axis: a matchup that STOPPED failing is an
    /// improvement. It is reported, because a comparator silent about it would hide a behavior
    /// change — but it can never Fail, since the Fail arm requires the regression direction.
    #[test]
    fn status_recovery_warns_but_never_fails() {
        let games: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(1), 10),
            (3, Some(0), 10),
            (4, Some(1), 10),
        ];
        let mut was_failing = mk_result_from_games("enchantress-mirror", games);
        was_failing.status = SuiteStatus::Fail;
        was_failing.fail_reason = Some("mirror imbalance".into());
        let baseline = mk_report(vec![was_failing]);
        let current = mk_report(vec![mk_result_from_games("enchantress-mirror", games)]);
        let report = compare(&baseline, &current, &CompareOptions).unwrap();
        let row = &report.rows[0];

        assert_eq!(
            row.suite_status_shift,
            Some((SuiteStatus::Fail, SuiteStatus::Pass)),
            "premise: this fixture really is the recovery direction"
        );
        assert_eq!(
            row.status,
            CompareStatus::Warn,
            "recovery is reported, never failed; reason={:?}",
            row.reason
        );
        // The merged arm's two branches share a status, so only the message distinguishes them.
        // Without this the recovery branch had no output assertion at all and could be emptied.
        assert!(
            row.reason
                .as_deref()
                .is_some_and(|r| r.contains("recovered") && r.contains("Fail → Pass")),
            "reason={:?}",
            row.reason
        );
        assert!(!report.any_fail(), "an improvement must not fail the gate");

        // Second branch case: `Fail → Open` is also recovery. Keying the still-failing branch on
        // `c.status != Pass` instead of `== Fail` would mislabel this row as still failing, and
        // the `Fail → Pass` case above cannot tell the two keyings apart.
        let mut was_failing = mk_result_from_games("enchantress-mirror", games);
        was_failing.status = SuiteStatus::Fail;
        let mut now_open = mk_result_from_games("enchantress-mirror", games);
        now_open.status = SuiteStatus::Open;
        let report = compare(
            &mk_report(vec![was_failing]),
            &mk_report(vec![now_open]),
            &CompareOptions,
        )
        .unwrap();
        assert!(
            report.rows[0]
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("recovered") && r.contains("Fail → Open")),
            "reason={:?}",
            report.rows[0].reason
        );
    }

    /// The status analogue of `draw_regression_escalates_an_insignificant_win_loss_warn`: this is
    /// what pins the status Fail arm ABOVE the win/loss Warn arm. Moved below it, this fixture
    /// reports `any_fail() == false` on a matchup that newly fails its own suite check.
    ///
    /// Same premise structure: the W/L axis is insignificant (p=0.0625) so the W/L Fail arm cannot
    /// fire, and `w2l != l2w` so the W/L axis reaches its Warn arm.
    #[test]
    fn status_regression_escalates_an_insignificant_win_loss_warn() {
        let before: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(0), 10),
            (3, Some(0), 10),
            (4, Some(1), 10),
            (5, Some(1), 10),
            (6, Some(1), 10),
            (7, Some(1), 10),
            (8, Some(1), 10),
            (9, Some(1), 10),
            (10, Some(1), 10),
            (11, Some(1), 10),
        ];
        let after: &[(u64, Option<u8>, u32)] = &[
            (1, Some(1), 10),
            (2, Some(1), 10),
            (3, Some(1), 10),
            (4, Some(1), 10),
            (5, Some(1), 10),
            (6, Some(1), 10),
            (7, Some(1), 10),
            (8, Some(1), 10),
            (9, Some(1), 10),
            (10, Some(1), 10),
            (11, Some(1), 10),
        ];
        let baseline = mk_report(vec![mk_result_from_games("m", before)]);
        let mut failing = mk_result_from_games("m", after);
        failing.status = SuiteStatus::Fail;
        failing.fail_reason = Some("mirror imbalance".into());
        let report = compare(&baseline, &mk_report(vec![failing]), &CompareOptions).unwrap();
        let row = &report.rows[0];

        // PREMISE: the W/L axis is insignificant, so the W/L Fail arm is out...
        assert_eq!((row.flipped_w_to_l, row.flipped_l_to_w), (3, 0));
        assert!(row.sign_test_p.is_some_and(|p| p > 0.05));
        // ...and the draw axis is flat, so neither draw arm can fire either.
        assert_eq!((row.decisive_to_draw, row.draw_to_decisive), (0, 0));

        assert_eq!(
            row.status,
            CompareStatus::Fail,
            "a matchup that newly fails its own check escalates a W/L Warn to Fail; reason={:?}",
            row.reason
        );
        assert!(report.any_fail(), "and the gate must exit non-zero");
        assert!(
            row.reason
                .as_deref()
                .is_some_and(|r| r.contains("status regressed")),
            "the status arm won, not the W/L Warn arm below it; reason={:?}",
            row.reason
        );
    }

    /// A matchup that was ALREADY failing in the baseline and is still failing is reported every
    /// run, not passed over in silence.
    ///
    /// Reachable, not theoretical, and durably so: nothing revalidates a committed baseline on
    /// load, so a baseline that already sanctions a failure keeps sanctioning it regardless of
    /// what guards the write path. (#7029 adds the missing verdict check to `--refresh-baseline`;
    /// this row stays reachable through baselines blessed before it, or hand-edited.)
    ///
    /// Warn rather than Fail is deliberate — see the arm's comment.
    #[test]
    fn a_matchup_still_failing_is_reported_every_run() {
        let games: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10), (2, Some(1), 10)];
        let mut was_failing = mk_result_from_games("m", games);
        was_failing.status = SuiteStatus::Fail;
        was_failing.fail_reason = Some("mirror imbalance".into());
        let mut still_failing = mk_result_from_games("m", games);
        still_failing.status = SuiteStatus::Fail;
        still_failing.fail_reason = Some("mirror imbalance".into());
        let report = compare(
            &mk_report(vec![was_failing]),
            &mk_report(vec![still_failing]),
            &CompareOptions,
        )
        .unwrap();
        let row = &report.rows[0];

        // PREMISE: the status did not shift, and every outcome axis is flat — so a chain that
        // only looked at transitions and outcomes would have nothing at all to say here.
        assert_eq!(row.suite_status_shift, None);
        assert_eq!((row.flipped_w_to_l, row.flipped_l_to_w), (0, 0));
        assert_eq!((row.decisive_to_draw, row.draw_to_decisive), (0, 0));

        assert_eq!(row.status, CompareStatus::Warn);
        assert!(
            row.reason
                .as_deref()
                .is_some_and(|r| r.contains("still failing")),
            "reason={:?}",
            row.reason
        );
        assert!(
            !report.any_fail(),
            "reported, but not red: the baseline already sanctions this state"
        );
    }

    /// One column per axis the verdict chain can decide on: win/loss, draw, avg-turn, suite
    /// status. Enforced here rather than asserted in a comment, so a column dropped from the
    /// renderer cannot leave the suite green.
    #[test]
    fn markdown_has_a_column_for_every_verdict_axis() {
        for axis in [
            "flips W→L",
            "flips L→W",
            "sign p",
            "dec→draw",
            "draw→dec",
            "draw sign p",
            "Δ avg turns",
            "suite status",
        ] {
            assert!(
                COLUMNS.contains(&axis),
                "the chain can decide a verdict on {axis} but the table has no column for it"
            );
        }
        let report = mk_report(vec![mk_result("red-mirror", 5, 10, SuiteStatus::Pass)]);
        let rendered = render_markdown(&compare(&report, &report, &CompareOptions).unwrap());
        let header = rendered.lines().next().unwrap();
        // Exact cell equality, not `contains`. The mutation this catches is on the RENDERED header
        // line: drop its last cell and a containment loop still passes, because the surviving
        // "suite status" cell CONTAINS the string "status". Measured — that variant went from
        // killing 1 test to killing 2.
        //
        // Scope, stated because the two mutations are easy to conflate: against a mutation of
        // `COLUMNS` itself this assertion is tautological, since the header is generated from
        // `COLUMNS`. That variant is caught anyway, by the `debug_assert_eq!` on cell count in
        // `render_markdown` and by the rectangularity test.
        let header_cells: Vec<&str> = header
            .split('|')
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .collect();
        assert_eq!(header_cells, COLUMNS, "header cells drifted from COLUMNS");

        // `status_str`'s `Pass` arm is the last rendered arm of that 5-variant cluster with no
        // assertion binding it — WARN, FAIL, NEW and REMOVED are each pinned by a cell assertion
        // elsewhere, and mutating `Pass => "PASS"` survived the whole suite. This fixture is an
        // identity comparison, so it is the only one that renders a Pass row.
        assert_eq!(cell(&rendered, "red-mirror", "status"), "PASS");
    }

    /// Read the cell under a named column, so an assertion binds a column to the field it renders
    /// rather than to a position. Splitting `| a | b |` yields a leading empty segment, hence +1.
    fn cell<'a>(rendered: &'a str, matchup_id: &str, column: &str) -> &'a str {
        let index = COLUMNS
            .iter()
            .position(|c| *c == column)
            .unwrap_or_else(|| panic!("no column named {column}"));
        let line = rendered
            .lines()
            .find(|l| l.starts_with(&format!("| {matchup_id} |")))
            .unwrap_or_else(|| panic!("no row for {matchup_id} in:\n{rendered}"));
        line.split('|').nth(index + 1).unwrap().trim()
    }

    /// Only `unpaired_baseline` moves: every seed that pairs is UNCHANGED, so every other axis
    /// reads zero. Before this arm existed such a row scored zero on everything and returned
    /// Pass while half its samples went unexamined.
    #[test]
    fn an_unmatched_baseline_sample_warns_instead_of_passing() {
        let before: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(1), 10),
            (3, Some(0), 10),
            (4, Some(1), 10),
        ];
        // Seeds 3 and 4 never ran in current. The two that DID pair are identical.
        let after: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10), (2, Some(1), 10)];

        let report = compare(
            &mk_report(vec![mk_result_from_games("partial", before)]),
            &mk_report(vec![mk_result_from_games("partial", after)]),
            &CompareOptions,
        )
        .unwrap();
        let row = &report.rows[0];

        // PREMISE: every other axis really is flat, so the verdict below can only come from
        // the unpaired arm. Without this the test would pass even if some other arm fired.
        assert_eq!(
            (
                row.flipped_w_to_l,
                row.flipped_l_to_w,
                row.decisive_to_draw,
                row.draw_to_decisive
            ),
            (0, 0, 0, 0)
        );
        assert_eq!((row.unpaired_baseline, row.unpaired_current), (2, 0));
        assert_eq!(row.status, CompareStatus::Warn);
        assert!(!report.any_fail());

        let rendered = render_markdown(&report);
        assert_eq!(cell(&rendered, "partial", "unpaired base"), "2");
        assert_eq!(cell(&rendered, "partial", "unpaired cur"), "0");
        assert_eq!(cell(&rendered, "partial", "status"), "WARN");
    }

    /// The other direction, and the one no counter could ever have seen: pairing walks the
    /// BASELINE, so extra current samples were not skipped — they were never visited. A
    /// current run could add any number of losses here and every counter would stay zero.
    #[test]
    fn an_extra_current_sample_warns_instead_of_passing() {
        let before: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10), (2, Some(1), 10)];
        let after: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(1), 10),
            (3, Some(1), 10),
            (4, Some(1), 10),
        ];

        let report = compare(
            &mk_report(vec![mk_result_from_games("extra", before)]),
            &mk_report(vec![mk_result_from_games("extra", after)]),
            &CompareOptions,
        )
        .unwrap();
        let row = &report.rows[0];

        assert_eq!(
            (
                row.flipped_w_to_l,
                row.flipped_l_to_w,
                row.decisive_to_draw,
                row.draw_to_decisive
            ),
            (0, 0, 0, 0)
        );
        // Mirrored against the test above: this one must be (0, 2), not (2, 0). A single fixture
        // covering "some unpaired sample exists" would pass with the two counters transposed.
        assert_eq!((row.unpaired_baseline, row.unpaired_current), (0, 2));
        assert_eq!(row.status, CompareStatus::Warn);
        assert!(!report.any_fail());

        let rendered = render_markdown(&report);
        assert_eq!(cell(&rendered, "extra", "unpaired base"), "0");
        assert_eq!(cell(&rendered, "extra", "unpaired cur"), "2");
    }

    /// The arm is LAST, so a row with real drift keeps the drift headline and reports the
    /// coverage gap through its columns. This pins the tier position: promoting the unpaired
    /// arm above the W/L arm would replace this reason string and flip this test.
    #[test]
    fn an_unpaired_sample_does_not_shadow_a_real_drift_reason() {
        let before: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(0), 10),
            (3, Some(0), 10),
            (4, Some(0), 10),
        ];
        let after: &[(u64, Option<u8>, u32)] = &[
            (1, Some(1), 10),
            (2, Some(1), 10),
            (3, Some(1), 10),
            (5, Some(1), 10),
        ];

        let report = compare(
            &mk_report(vec![mk_result_from_games("both", before)]),
            &mk_report(vec![mk_result_from_games("both", after)]),
            &CompareOptions,
        )
        .unwrap();
        let row = &report.rows[0];

        assert_eq!((row.unpaired_baseline, row.unpaired_current), (1, 1));
        assert!(
            row.reason.as_deref().unwrap().starts_with("paired"),
            "drift reason must survive; got {:?}",
            row.reason
        );
        // ...and the coverage gap is still visible, because columns outlive suppression.
        let rendered = render_markdown(&report);
        assert_eq!(cell(&rendered, "both", "unpaired base"), "1");
        assert_eq!(cell(&rendered, "both", "unpaired cur"), "1");
    }

    #[test]
    fn a_different_base_seed_is_refused_rather_than_compared() {
        let mut baseline = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);
        let mut current = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);
        baseline.base_seed = 1;
        current.base_seed = 2;

        let err = compare(&baseline, &current, &CompareOptions).unwrap_err();
        assert!(
            matches!(&err, CompareError::WorkloadMismatch { field, .. } if *field == "base_seed"),
            "got {err:?}"
        );
    }

    #[test]
    fn a_different_difficulty_is_refused_rather_than_compared() {
        let baseline = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);
        let mut current = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);
        current.difficulty = "cEDH".into();

        let err = compare(&baseline, &current, &CompareOptions).unwrap_err();
        assert!(
            matches!(&err, CompareError::WorkloadMismatch { field, .. } if *field == "difficulty"),
            "got {err:?}"
        );
    }

    /// The control arm for both guards above. Without it, a mutant that made `compare` reject
    /// EVERY pair would satisfy the two mismatch tests and nothing would notice.
    #[test]
    fn matching_workloads_are_compared_normally() {
        let baseline = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);
        let current = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);

        assert_eq!(baseline.base_seed, current.base_seed);
        assert_eq!(baseline.difficulty, current.difficulty);
        let report = compare(&baseline, &current, &CompareOptions).expect("must compare");
        assert_eq!(report.rows.len(), 1);
    }

    /// A differing `games_per_matchup` is refused. The samples that pair are comparable, but a
    /// `Warn` keeps `any_fail` false, the gate exits 0, and the nightly publishes only on a
    /// non-zero exit — so a verdict drawn from a tenth of the sample must not be able to come
    /// back Pass.
    ///
    /// The two assertions below are the pair that makes this a fix rather than a trade: the row
    /// count pins that no verdict is produced, and the body pins that the diagnostics survive.
    #[test]
    fn a_different_games_per_matchup_is_refused_with_its_diagnostics_intact() {
        let before: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10)];
        let after: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10), (2, Some(0), 10)];
        let mut baseline = mk_report(vec![mk_result_from_games("n", before)]);
        let mut current = mk_report(vec![mk_result_from_games("n", after)]);
        baseline.games_per_matchup = 1;
        current.games_per_matchup = 2;

        // PREMISE: the two fields that already gated are equal, so the refusal below can only
        // come from the new one.
        assert_eq!(baseline.base_seed, current.base_seed);
        assert_eq!(baseline.difficulty, current.difficulty);

        let err = compare(&baseline, &current, &CompareOptions).expect_err("must refuse");
        assert!(
            matches!(&err, CompareError::WorkloadMismatch { field, .. } if *field == "games_per_matchup"),
            "unexpected error: {err:?}"
        );

        // Both values reach the reader, so the drift issue says which knob to turn.
        let body = render_error_markdown(&err);
        assert!(body.contains("games_per_matchup"), "{body}");
        assert!(body.contains("baseline=1"), "{body}");
        assert!(body.contains("current=2"), "{body}");

        // BOTH remedies, named with their concrete target values. One-remedy text reads as
        // "you must re-record", which is the more expensive of the two and not always the
        // one the reader wants — and a reader who believes it is the only option will take
        // it. Each direction is asserted through the value it moves TO, so a text that names
        // both remedies but transposes their targets fails here.
        assert!(
            body.contains("--refresh-baseline"),
            "refresh remedy missing: {body}"
        );
        assert!(body.contains("--games"), "re-run remedy missing: {body}");
        assert!(
            body.contains("Move the baseline to `2`"),
            "remedy 1 must target the CURRENT workload: {body}"
        );
        assert!(
            body.contains("Move this run to `1`"),
            "remedy 2 must target the BASELINE workload: {body}"
        );
    }

    /// A repeated seed has no single partner. Pairing walks the baseline and looks each seed up
    /// in a map keyed by seed, so the duplicate's second game re-pairs against the same current
    /// game while a real current game is never visited — and `unpaired_current`, the counter that
    /// would have said so, reads zero.
    #[test]
    fn a_repeated_seed_is_refused_rather_than_silently_dropping_current_games() {
        let baseline = mk_report(vec![mk_result_from_games(
            "dup",
            &[(1, Some(0), 10), (1, Some(0), 10)],
        )]);
        let current = mk_report(vec![mk_result_from_games(
            "dup",
            &[(1, Some(0), 10), (2, Some(1), 10)],
        )]);

        // PREMISE: every workload field agrees, so the refusal can only come from the seed.
        assert_eq!(baseline.base_seed, current.base_seed);
        assert_eq!(baseline.difficulty, current.difficulty);
        assert_eq!(baseline.games_per_matchup, current.games_per_matchup);

        let err = compare(&baseline, &current, &CompareOptions).expect_err("must refuse");
        assert!(
            matches!(
                &err,
                CompareError::DuplicateSeed {
                    side: ReportSide::Baseline,
                    matchup_id,
                    seed,
                } if matchup_id == "dup" && *seed == 1
            ),
            "unexpected error: {err:?}"
        );

        // The refusal reaches the reader with the coordinates it needs to find the bad report.
        let body = render_error_markdown(&err);
        assert!(body.contains("baseline"), "{body}");
        assert!(body.contains("dup"), "{body}");
        assert!(body.contains("--refresh-baseline"), "{body}");
    }

    /// The other end of the class: the same malformedness on the current side.
    #[test]
    fn a_repeated_seed_in_the_current_report_is_refused_too() {
        let baseline = mk_report(vec![mk_result_from_games(
            "dup",
            &[(1, Some(0), 10), (2, Some(1), 10)],
        )]);
        let current = mk_report(vec![mk_result_from_games(
            "dup",
            &[(1, Some(0), 10), (1, Some(0), 10)],
        )]);

        let err = compare(&baseline, &current, &CompareOptions).expect_err("must refuse");
        assert!(
            matches!(
                &err,
                CompareError::DuplicateSeed {
                    side: ReportSide::Current,
                    ..
                }
            ),
            "unexpected error: {err:?}"
        );
    }

    /// The adversarial admitted member. A longer current run with DISTINCT seeds is the
    /// legitimate unpaired-remainder case, and the guard must not turn it into a refusal — the
    /// counter it pins is the one a duplicate corrupts.
    #[test]
    fn a_longer_current_run_with_distinct_seeds_still_compares() {
        let baseline = mk_report(vec![mk_result_from_games(
            "n",
            &[(1, Some(0), 10), (2, Some(1), 10)],
        )]);
        let current = mk_report(vec![mk_result_from_games(
            "n",
            &[(1, Some(0), 10), (2, Some(1), 10), (3, Some(0), 10)],
        )]);

        let report = compare(&baseline, &current, &CompareOptions).expect("distinct seeds pair");
        assert_eq!(report.rows[0].unpaired_current, 1);
        assert_eq!(report.rows[0].unpaired_baseline, 0);
    }

    /// Every refusal variant renders a body, including the two `compare` itself cannot produce.
    ///
    /// `Io` and `Parse` reach this renderer only from `load_report`'s callers, never from
    /// `compare`, so covering them here holds the arms live regardless of which caller gets
    /// there.
    ///
    /// The `assert_ne!` against the bare `Display` is the discriminating half: an implementation
    /// that forwarded the error string would satisfy every other assertion here while losing the
    /// remedy, and losing the remedy is the whole failure mode.
    #[test]
    fn every_refusal_renders_a_body_that_says_more_than_the_error_line() {
        let io = CompareError::Io(std::io::Error::other("disk"));
        let parse = CompareError::Parse(serde_json::from_str::<SuiteReport>("{").unwrap_err());
        let schema = CompareError::SchemaMismatch {
            baseline: 1,
            current: 2,
        };
        let workload = CompareError::WorkloadMismatch {
            field: "games_per_matchup",
            baseline: "10".to_string(),
            current: "100".to_string(),
        };

        let duplicate = CompareError::DuplicateSeed {
            side: ReportSide::Baseline,
            matchup_id: "red-mirror".to_string(),
            seed: 7,
        };

        for err in [&io, &parse, &schema, &workload, &duplicate] {
            let body = render_error_markdown(err);
            assert!(!body.trim().is_empty(), "empty body for {err:?}");
            assert!(
                body.contains("## Gate: comparison refused"),
                "missing heading for {err:?}: {body}"
            );
            assert!(
                body.contains(&err.to_string()),
                "body must carry the error itself for {err:?}: {body}"
            );
            assert_ne!(
                body.trim(),
                err.to_string().trim(),
                "body must add a remedy, not echo the error, for {err:?}"
            );
        }
    }

    /// The workflow's two conditions for posting a drift issue, asserted together on the refusal
    /// path: a non-zero exit (`if: steps.gate.outcome == 'failure'`) AND a non-empty stdout body
    /// (`if [ ! -s target/ai-gate-report.md ]; then ... exit 1`). Pinning one alone cannot see
    /// the failure this change exists to prevent — the pre-change behaviour satisfied neither on
    /// a mismatched workload, and printing the refusal to stderr alone would satisfy only the
    /// first, producing a red job whose issue body is empty.
    #[test]
    fn a_refused_comparison_exits_nonzero_and_still_writes_a_report_body() {
        let mut baseline = mk_report(vec![mk_result("n", 5, 10, SuiteStatus::Pass)]);
        let current = mk_report(vec![mk_result("n", 5, 10, SuiteStatus::Pass)]);
        baseline.games_per_matchup = 99;

        let (body, code) = gate_verdict(&compare(&baseline, &current, &CompareOptions));
        assert_eq!(code, 2, "a refusal must fail the gate step");
        assert!(!body.trim().is_empty(), "the report body must not be empty");
        assert!(body.contains("games_per_matchup"), "{body}");
    }

    /// Control arm for the test above: `gate_verdict` must not be a constant. An implementation
    /// returning `(something, 2)` for everything would satisfy the refusal assertions completely
    /// while breaking every green run, so the three outcomes are pinned to three distinct codes
    /// — and each carries a body, because the nightly aborts on an empty report file whatever
    /// the exit code was.
    #[test]
    fn gate_verdict_maps_each_outcome_to_its_own_exit_code() {
        let clean = mk_report(vec![mk_result("n", 5, 10, SuiteStatus::Pass)]);
        let (pass_body, pass_code) = gate_verdict(&compare(&clean, &clean, &CompareOptions));
        assert_eq!(pass_code, 0);
        // The WHOLE line, not `contains("compare: 0 FAIL")`. Four of five counters are zero on
        // this fixture, so the substring form is invariant under transposing pass/warn, under
        // zeroing every counter, and under deleting the tally loop outright — measured, all
        // three still contain it. A count that is only ever asserted at zero is not asserted.
        assert!(
            pass_body.contains("compare: 0 FAIL, 0 WARN, 1 PASS, 0 NEW, 0 REMOVED"),
            "{pass_body}"
        );

        // A matchup that regressed into Fail: a comparison that succeeded and found drift.
        let regressed = mk_report(vec![mk_result("n", 5, 10, SuiteStatus::Fail)]);
        let comparison = compare(&clean, &regressed, &CompareOptions);
        // PREMISE: this really is the drift path, not another refusal.
        assert!(comparison.as_ref().expect("must compare").any_fail());
        let (fail_body, fail_code) = gate_verdict(&comparison);
        assert_eq!(fail_code, 1);
        assert!(fail_body.contains("| n |"), "{fail_body}");

        // A tally with two DISTINCT non-zero counters, so no pair of counters can be
        // transposed without changing the rendered line. `1 PASS` and `2 NEW` are different
        // numbers in different slots; the single-row fixture above cannot see that.
        let mut widened = clean.clone();
        widened
            .results
            .push(mk_result("fresh-a", 5, 10, SuiteStatus::Pass));
        widened
            .results
            .push(mk_result("fresh-b", 5, 10, SuiteStatus::Pass));
        let (mixed_body, mixed_code) = gate_verdict(&compare(&clean, &widened, &CompareOptions));
        assert_eq!(mixed_code, 0);
        assert!(
            mixed_body.contains("compare: 0 FAIL, 0 WARN, 1 PASS, 2 NEW, 0 REMOVED"),
            "{mixed_body}"
        );

        let mut incomparable = clean.clone();
        incomparable.schema_version += 1;
        let (err_body, err_code) = gate_verdict(&compare(&incomparable, &clean, &CompareOptions));
        assert_eq!(err_code, 2);
        // `schema_version` comes from the Display impl, so it does NOT pin the remedy text or
        // the heading. Both are asserted separately or they can be emptied in silence.
        assert!(err_body.contains("schema_version"), "{err_body}");
        assert!(
            err_body.contains("## Gate: comparison refused"),
            "{err_body}"
        );
        assert!(err_body.contains("--refresh-baseline"), "{err_body}");
    }

    #[test]
    fn markdown_cells_carry_their_own_column_values() {
        let before: &[(u64, Option<u8>, u32)] = &[
            (1, Some(0), 10),
            (2, Some(0), 10),
            (3, Some(1), 10),
            (4, Some(1), 10),
            (5, Some(1), 10),
            (6, Some(0), 10),
            (7, Some(0), 10),
            // p0 win, and still `Some(_) → None` in `after`, so this separates the two win-rate
            // cells WITHOUT touching any of the four counters. Both sides carry the same seed
            // set, so the separation cannot come from an unpaired-seed skip.
            (8, Some(0), 10),
            (9, Some(1), 10),
            (10, None, 10),
            (11, Some(0), 10),
        ];
        let after: &[(u64, Option<u8>, u32)] = &[
            (1, Some(1), 10),
            (2, Some(1), 10),
            (3, Some(0), 10),
            (4, Some(0), 10),
            (5, Some(0), 10),
            (6, None, 10),
            (7, None, 10),
            (8, None, 10),
            (9, None, 10),
            (10, Some(0), 10),
            (11, Some(0), 10),
        ];
        let baseline = mk_report(vec![mk_result_from_games("distinct", before)]);
        let mut current_result = mk_result_from_games("distinct", after);
        current_result.avg_turns = 16.0; // baseline 10.0 → +6.0
        let report = compare(&baseline, &mk_report(vec![current_result]), &CompareOptions).unwrap();
        let row = &report.rows[0];

        // PREMISE: the four counters really are pairwise distinct, so a transposition must show.
        assert_eq!(
            (
                row.flipped_w_to_l,
                row.flipped_l_to_w,
                row.decisive_to_draw,
                row.draw_to_decisive
            ),
            (2, 3, 4, 1)
        );
        // PREMISE: the two win-rate cells DIFFER. Identical cells would let a transposition
        // survive, so the premise is asserted rather than assumed.
        assert_ne!(
            winrate(row.baseline.as_ref().unwrap()),
            winrate(row.current.as_ref().unwrap())
        );
        // PREMISE: and it achieves that on MATCHED seed sets, so this test measures the
        // transposition property alone. If a future edit reintroduces an unmatched seed here, the
        // row would also start carrying the unpaired axis and this fixture would quietly become
        // two tests wearing one name.
        assert_eq!((row.unpaired_baseline, row.unpaired_current), (0, 0));

        let rendered = render_markdown(&report);
        assert_eq!(cell(&rendered, "distinct", "exercises"), "AggroPressure");
        // 6/11 vs 5/11 — separated by the seed-8 outcome, not by an uncompared sample.
        assert_eq!(cell(&rendered, "distinct", "baseline p0%"), "55%");
        assert_eq!(cell(&rendered, "distinct", "current p0%"), "45%");
        assert_eq!(cell(&rendered, "distinct", "flips W→L"), "2");
        assert_eq!(cell(&rendered, "distinct", "flips L→W"), "3");
        assert_eq!(cell(&rendered, "distinct", "dec→draw"), "4");
        assert_eq!(cell(&rendered, "distinct", "draw→dec"), "1");
        assert_eq!(cell(&rendered, "distinct", "sign p"), "0.3438");
        assert_eq!(cell(&rendered, "distinct", "draw sign p"), "0.1094");
        assert_eq!(cell(&rendered, "distinct", "Δ avg turns"), "+6.0");
        assert_eq!(cell(&rendered, "distinct", "suite status"), "Pass");
        assert_eq!(cell(&rendered, "distinct", "status"), "WARN");
    }

    /// The `suite status` column's SHIFT branch — the one the status axis exists to surface — was
    /// rendered by no test: replacing it with a constant survived the whole suite, because every
    /// other rendered fixture has `suite_status_shift == None`. Round 3's finding, one column over.
    #[test]
    fn markdown_renders_a_suite_status_shift() {
        let games: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10), (2, Some(1), 10)];
        let baseline = mk_report(vec![mk_result_from_games("shifted", games)]);
        let mut failing = mk_result_from_games("shifted", games);
        failing.status = SuiteStatus::Fail;
        failing.fail_reason = Some("mirror imbalance".into());
        let report = compare(&baseline, &mk_report(vec![failing]), &CompareOptions).unwrap();

        // PREMISE: this row really carries a shift, so the fallback branch is not what renders it.
        assert_eq!(
            report.rows[0].suite_status_shift,
            Some((SuiteStatus::Pass, SuiteStatus::Fail))
        );
        let rendered = render_markdown(&report);
        assert_eq!(cell(&rendered, "shifted", "suite status"), "Pass→Fail");
        assert_eq!(cell(&rendered, "shifted", "status"), "FAIL");
    }

    /// Count the `|` characters a markdown parser would treat as cell boundaries.
    ///
    /// A pipe is a separator iff the run of backslashes immediately before it has EVEN length:
    /// each pair is one literal backslash, and an odd leftover escapes the pipe. The obvious
    /// "is the previous character a backslash" test is what this replaces — it is right for `\|`
    /// and wrong for `\\|`, which is a literal backslash followed by a LIVE separator, and being
    /// wrong in exactly that direction it would call a broken encoder green.
    fn md_separator_count(line: &str) -> usize {
        line.char_indices()
            .filter(|(i, c)| {
                *c == '|' && line[..*i].chars().rev().take_while(|p| *p == '\\').count() % 2 == 0
            })
            .count()
    }

    /// The escape order inside `md_cell`, pinned from the outside. Escaping pipes before
    /// backslashes turns the input `\|` into `\\|` — an escaped backslash followed by a live
    /// separator — so the one input that already looks escaped is the one that breaks the row.
    /// `fail_reason` is free-form text from a JSON report, so a backslash is reachable input.
    ///
    /// Both parities are exercised, because an encoder can be wrong in either direction: `\|`
    /// (odd run, must stay content) and `\\|` (even run, a real separator that must be escaped
    /// into the cell). A fixture carrying only one of them cannot distinguish the two.
    #[test]
    fn a_backslash_before_a_pipe_cannot_smuggle_a_separator() {
        let reason = r"odd \| even \\| tail";
        let baseline = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Pass)]);
        let mut current = mk_report(vec![mk_result("m", 5, 10, SuiteStatus::Fail)]);
        current.results[0].fail_reason = Some(reason.to_string());

        let report = compare(&baseline, &current, &CompareOptions).unwrap();
        let rendered = render_markdown(&report);

        // PREMISE: the reason really was rendered, so the row below exists.
        assert!(rendered.contains("tail"), "reason must render:\n{rendered}");

        let widths: Vec<usize> = rendered
            .lines()
            .filter(|l| l.starts_with('|'))
            .map(md_separator_count)
            .collect();
        assert!(widths.len() >= 3, "expected a reason row: {widths:?}");
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "a backslash run changed the column count: {widths:?}\n{rendered}"
        );

        // CONTROL: the previous implementation, inlined, on this same input. Without this the
        // assertion above could be passing for a reason unrelated to the escape order.
        let pipes_first = |t: &str| t.replace('|', r"\|").replace(['\n', '\r'], " ");
        let old_row = format!("|  ↳ _{}_ |", pipes_first(reason));
        let new_row = format!("|  ↳ _{}_ |", md_cell(reason));
        assert_eq!(
            md_separator_count(&new_row),
            2,
            "the fixed encoder must emit exactly the two boundaries this row owns: {new_row}"
        );
        assert!(
            md_separator_count(&old_row) > 2,
            "fixture is not discriminating — the old encoder must leak a separator: {old_row}"
        );

        // Trailing backslashes, asserted on the ENCODER rather than on row width, and the
        // distinction is the point. Cells are joined with `" | "`, so a trailing backslash is
        // separated from the boundary by a space and the row stays rectangular under BOTH
        // encoders — a width assertion here would pass for a reason unrelated to the bug.
        // The property that does separate them is that an emitted cell never ends in an ODD
        // backslash run, since the very next character the renderer writes is a separator's
        // neighbourhood. Runs 0..=4 cover both parities and the boundary case of none.
        for run in 0..=4usize {
            let input = format!("tail{}", "\\".repeat(run));
            let encoded = md_cell(&input);
            let trailing = encoded.chars().rev().take_while(|c| *c == '\\').count();
            assert_eq!(
                trailing % 2,
                0,
                "run of {run} must encode to an even trailing run, got {trailing}: {encoded:?}"
            );
            // CONTROL: the previous encoder leaves odd runs odd, so this loop is discriminating
            // for every odd `run` rather than trivially true.
            let old_trailing = pipes_first(&input)
                .chars()
                .rev()
                .take_while(|c| *c == '\\')
                .count();
            assert_eq!(
                old_trailing % 2,
                run % 2,
                "control drifted — the old encoder must preserve run parity: {input:?}"
            );
        }
    }

    /// Reachability arm for `md_cell`. `markdown_rows_are_rectangular` asserted the property
    /// this test is named for, but every one of its fixtures was pipe-free — so it passed for
    /// a reason unrelated to the hazard and gave false confidence about exactly the invariant
    /// it claimed. A `fail_reason` is free-form text from a report, and `ai_duel compare`
    /// reads whatever file it is handed, so a pipe is reachable input, not a hypothetical.
    #[test]
    fn report_supplied_pipes_cannot_add_columns() {
        let mut baseline = mk_report(vec![mk_result("m|id", 5, 10, SuiteStatus::Pass)]);
        let mut current = mk_report(vec![mk_result("m|id", 5, 10, SuiteStatus::Fail)]);
        baseline.results[0].matchup_id = "m|id".into();
        current.results[0].matchup_id = "m|id".into();
        current.results[0].fail_reason =
            Some("imbalance: p0=0.10 | CI [0.02, 0.40] | excludes 0.50".into());

        let report = compare(&baseline, &current, &CompareOptions).unwrap();
        let rendered = render_markdown(&report);

        // PREMISE: the row really did render a reason carrying pipes, so this is not vacuous.
        assert!(
            rendered.contains("excludes 0.50"),
            "reason must be rendered:\n{rendered}"
        );
        assert_eq!(report.rows[0].status, CompareStatus::Fail);

        // Every row — header, separator, data, and the reason continuation — must have the
        // same cell count. An unescaped pipe shows up here as a longer row.
        let widths: Vec<usize> = rendered
            .lines()
            .filter(|l| l.starts_with('|'))
            .map(md_separator_count)
            .collect();
        assert!(widths.len() >= 4, "expected a reason row too: {widths:?}");
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "pipes changed the column count: {widths:?}\n{rendered}"
        );
    }

    /// Every emitted row — header, separator, data, and the reason continuation — must have the
    /// same cell count, or the table renders broken in the nightly drift issue that
    /// `.github/workflows/ai-gate.yml` posts. Exercises all four row shapes at once: a paired row
    /// that warns (so its reason continuation is emitted), a New row, and a Removed row.
    #[test]
    fn markdown_rows_are_rectangular() {
        let paired_before: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10), (2, Some(1), 10)];
        let paired_after: &[(u64, Option<u8>, u32)] = &[(1, Some(1), 10), (2, Some(1), 10)];
        let baseline = mk_report(vec![
            mk_result_from_games("paired", paired_before),
            mk_result_from_games("removed", paired_before),
        ]);
        let current = mk_report(vec![
            mk_result_from_games("paired", paired_after),
            mk_result_from_games("brand-new", paired_after),
        ]);
        let report = compare(&baseline, &current, &CompareOptions).unwrap();
        let rendered = render_markdown(&report);

        // PREMISE: all four row shapes really are present, otherwise this measures less than it
        // claims to.
        assert!(
            rendered.contains("|  ↳ _"),
            "no reason continuation emitted"
        );
        assert!(rendered.contains("| NEW |"), "no New row emitted");
        assert!(rendered.contains("| REMOVED |"), "no Removed row emitted");

        // A New row's verdict is decided FROM its suite status, so that column must carry a value
        // even though there is no shift to show.
        let new_row = rendered.lines().find(|l| l.contains("| NEW |")).unwrap();
        assert!(
            new_row.contains("| Pass | NEW |"),
            "a status-decided verdict must not print a blank status cell: {new_row}"
        );

        // The `—` fallbacks are what New and Removed rows print in the paired-only columns: a New
        // row has no baseline to compare against, a Removed row no current report at all.
        //
        // Every `—` fallback site is asserted; a partial sweep reads as complete.
        assert_eq!(cell(&rendered, "brand-new", "baseline p0%"), "—");
        assert_eq!(cell(&rendered, "brand-new", "sign p"), "—");
        assert_eq!(cell(&rendered, "brand-new", "draw sign p"), "—");
        assert_eq!(cell(&rendered, "brand-new", "Δ avg turns"), "—");
        assert_eq!(cell(&rendered, "removed", "current p0%"), "—");
        assert_eq!(cell(&rendered, "removed", "suite status"), "—");
        assert_eq!(
            cell(&rendered, "removed", "status"),
            "REMOVED",
            "and the Removed row still reaches its own verdict"
        );

        // The separator is GENERATED from the header widths, so its validity is not free: an empty
        // fill still has the right pipe count but renders the whole table as one paragraph in the
        // issue body. Measured: `"-".repeat(0)` survives a pipe-count-only check.
        //
        // Indexed rather than filtered on purpose. The obvious spelling —
        // `.split('|').filter(|s| !s.is_empty())` — is VACUOUS against exactly the mutation this
        // is here to catch: with empty fills every segment is empty, the filter drops all of them,
        // and the loop body never runs. Measured that too, while fixing this.
        let separator = rendered.lines().nth(1).unwrap();
        let segments: Vec<&str> = separator.split('|').collect();
        assert_eq!(
            segments.len(),
            COLUMNS.len() + 2,
            "separator is not bounded by pipes with one segment per column: {separator}"
        );
        for segment in &segments[1..=COLUMNS.len()] {
            assert!(
                segment.len() >= 3 && segment.chars().all(|c| c == '-'),
                "separator segment {segment:?} is not a valid markdown rule: {separator}"
            );
        }

        let expected = COLUMNS.len() + 1; // n cells ⇒ n+1 pipes
        for line in rendered.lines() {
            assert_eq!(
                line.matches('|').count(),
                expected,
                "row has the wrong cell count: {line}"
            );
        }
    }

    /// The mirror avg-turn arm is guarded on `Expected::Mirror`, and every other fixture in this
    /// module is a mirror — so deleting that guard survived the whole suite. A `Triangle` matchup
    /// has no symmetry expectation, and a turn-count drift is not a finding for it.
    ///
    /// Pre-existing arm, pinned here because this change reorders around it: the new draw and
    /// status Warn arms sit directly above it.
    #[test]
    fn avg_turn_drift_is_only_a_finding_for_mirrors() {
        let games: &[(u64, Option<u8>, u32)] = &[(1, Some(0), 10), (2, Some(1), 10)];
        let mut base = mk_result_from_games("triangle", games);
        base.expected = Expected::Triangle {
            p0_winrate_min: 0.4,
            p0_winrate_max: 0.6,
        };
        let mut drifted = mk_result_from_games("triangle", games);
        drifted.expected = Expected::Triangle {
            p0_winrate_min: 0.4,
            p0_winrate_max: 0.6,
        };
        drifted.avg_turns = 20.0; // +10.0, far past MIRROR_AVG_TURN_WARN_DELTA

        let report = compare(
            &mk_report(vec![base]),
            &mk_report(vec![drifted]),
            &CompareOptions,
        )
        .unwrap();

        // PREMISE: the drift really is past the threshold, so only the Mirror guard suppresses it.
        assert_eq!(report.rows[0].avg_turn_delta, Some(10.0));
        assert!(report.rows[0].avg_turn_delta.unwrap() > MIRROR_AVG_TURN_WARN_DELTA);
        // PREMISE: nothing else could produce a verdict here.
        assert_eq!(
            (
                report.rows[0].flipped_w_to_l,
                report.rows[0].flipped_l_to_w,
                report.rows[0].decisive_to_draw,
                report.rows[0].draw_to_decisive
            ),
            (0, 0, 0, 0)
        );

        assert_eq!(
            report.rows[0].status,
            CompareStatus::Pass,
            "a non-mirror matchup has no symmetry expectation to drift from; reason={:?}",
            report.rows[0].reason
        );
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
        // The counters and their ORDER, not just the phrase: transposing the two format args here
        // printed `W→L=0 L→W=10` for this very fixture: a regression reported as an improvement.
        assert!(
            result.rows[0]
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("paired regression: W→L=10 L→W=0")),
            "reason={:?}",
            result.rows[0].reason
        );
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
        // Same in-order pin on the win/loss Warn reason as on its Fail sibling above.
        assert!(
            result.rows[0]
                .reason
                .as_deref()
                .is_some_and(|r| r.contains("paired shift: W→L=0 L→W=2")),
            "reason={:?}",
            result.rows[0].reason
        );
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
