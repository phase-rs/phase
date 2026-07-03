//! Deterministic decision-cost regression gate.
//!
//! Runs the three quick-gate mirror matchups through a fixed seeded action-cap
//! prefix, field-wise sums the engine perf counters, and compares the integer
//! payload against a committed baseline. Catches cost-per-decision regressions
//! (clone storms, quadratic combat scans, display sweeps in search) that the
//! win-rate `cargo ai-gate` is structurally blind to.
//!
//! Workload (seed, action_cap) is fixed by compile-time consts in
//! `duel_suite::perf`, never flags, so the gate can never run against a workload
//! that mismatches the baseline.
//!
//! Individual trajectories are NOT cross-process deterministic — engine
//! HashSet/HashMap iteration order leaks per-process RandomState into AI
//! tie-breaking (issue #4878). The gate therefore aggregates the per-counter
//! MEDIAN over `PERF_SAMPLE_COUNT` INDEPENDENT cold child processes (fresh
//! RandomState each), spawned via `current_exe()` with the internal
//! `--emit-sample` flag. `main()` dispatches three mutually exclusive modes:
//! child (emit one sample), repro-report (margin gate over saved runs), and
//! parent gate (spawn K children, median, compare).

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use engine::database::CardDatabase;
use phase_ai::duel_suite::perf::{
    compare, default_scenarios, load_report, median_report, print_markdown, print_repro_margin,
    repro_margin_report, run_perf_suite, PerfReport, PERF_ACTION_CAP, PERF_BASE_SEED,
    PERF_SAMPLE_COUNT,
};

const DEFAULT_BASELINE: &str = "crates/phase-ai/baselines/perf-baseline.json";
const DEFAULT_CURRENT: &str = "target/ai-perf-gate-current.json";

struct Args {
    data_root: PathBuf,
    baseline: PathBuf,
    current_output: PathBuf,
    refresh_baseline: bool,
    /// Internal: emit a single-trajectory sample to this path and exit. Set only
    /// on the K child processes the parent spawns.
    emit_sample: Option<PathBuf>,
    /// Internal: run the reproducibility MARGIN gate over `--repro-input` reports.
    repro_report: bool,
    /// Internal: the validation-run reports the margin gate aggregates (repeatable).
    repro_inputs: Vec<PathBuf>,
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            if !message.is_empty() {
                eprintln!("{message}");
            }
            print_usage();
            std::process::exit(2);
        }
    };

    // Branch 1 — child: load the DB, emit ONE single-trajectory sample to the
    // file, exit. Emits NOTHING on stdout (GAP 4) so the parent's stdout stays a
    // clean table; diagnostics go to stderr only.
    if let Some(sample_path) = &args.emit_sample {
        run_child_sample(&args.data_root, sample_path);
        return;
    }

    // Branch 2 — repro-report: pure aggregation over saved reports, no DB load.
    if args.repro_report {
        run_repro_report(&args.baseline, &args.repro_inputs);
        return;
    }

    // Branch 3 — parent gate: spawn K children, take the per-counter median, stamp
    // provenance, compare (or refresh). Never loads the DB itself.
    run_parent_gate(&args);
}

/// Branch 1: emit a single-trajectory sample report to `sample_path`. Loads the
/// card DB (the only branch that does). Never writes stdout.
fn run_child_sample(data_root: &Path, sample_path: &Path) {
    let db_path = data_root.join("card-data.json");
    let db = match CardDatabase::from_export(&db_path) {
        Ok(db) => db,
        Err(err) => {
            eprintln!(
                "failed to load card database from {}: {err}",
                db_path.display()
            );
            std::process::exit(2);
        }
    };
    let report = run_perf_suite(&db, PERF_BASE_SEED, PERF_ACTION_CAP, &default_scenarios());
    if let Err(err) = write_report(&report, sample_path) {
        eprintln!(
            "failed to write sample report {}: {err}",
            sample_path.display()
        );
        std::process::exit(2);
    }
}

/// Branch 2: the reproducibility MARGIN gate. Exit 0 iff every counter's worst
/// observed value across the validation runs stays within the named fraction of
/// its FAIL headroom. This exit code IS the M15 margin gate.
fn run_repro_report(baseline_path: &Path, repro_inputs: &[PathBuf]) {
    let baseline = match load_report(baseline_path) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("failed to load baseline {}: {err}", baseline_path.display());
            std::process::exit(2);
        }
    };
    let mut runs = Vec::with_capacity(repro_inputs.len());
    for path in repro_inputs {
        match load_report(path) {
            Ok(report) => runs.push(report),
            Err(err) => {
                eprintln!("failed to load repro input {}: {err}", path.display());
                std::process::exit(2);
            }
        }
    }
    let margin = repro_margin_report(&baseline, &runs);
    print_repro_margin(&margin);
    if margin.all_within_margin() {
        std::process::exit(0);
    }
    std::process::exit(1);
}

/// Branch 3: spawn `PERF_SAMPLE_COUNT` independent cold child processes, aggregate
/// the per-counter median, stamp provenance, then refresh-or-compare.
fn run_parent_gate(args: &Args) {
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            eprintln!("failed to resolve current executable for sampling: {err}");
            std::process::exit(2);
        }
    };
    // Spawn K children SEQUENTIALLY (blocking .status()); each is an independent
    // process with a fresh std RandomState, hence an independent trajectory.
    let mut samples = Vec::with_capacity(PERF_SAMPLE_COUNT);
    let mut temp_paths = Vec::with_capacity(PERF_SAMPLE_COUNT);
    for i in 0..PERF_SAMPLE_COUNT {
        let tmp_i =
            std::env::temp_dir().join(format!("ai-perf-sample-{}-{i}.json", std::process::id()));
        // Registered BEFORE the spawn so every failure path below cleans it up.
        temp_paths.push(tmp_i.clone());
        let status = Command::new(&exe)
            .arg("--emit-sample")
            .arg(&tmp_i)
            .arg("--data-root")
            .arg(&args.data_root)
            .stdout(Stdio::null()) // GAP 4: parent's stdout stays a clean table
            .stderr(Stdio::inherit()) // child diagnostics still visible in CI logs
            .status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => {
                eprintln!("perf sample child {i} exited with status {s} — aborting (no silent K reduction)");
                cleanup_temps(&temp_paths);
                std::process::exit(2);
            }
            Err(err) => {
                eprintln!("failed to spawn perf sample child {i}: {err}");
                cleanup_temps(&temp_paths);
                std::process::exit(2);
            }
        }
        match load_report(&tmp_i) {
            Ok(report) => samples.push(report),
            Err(err) => {
                eprintln!(
                    "perf sample child {i} produced an unreadable report {}: {err}",
                    tmp_i.display()
                );
                cleanup_temps(&temp_paths);
                std::process::exit(2);
            }
        }
    }

    let mut current = median_report(&samples);
    // Stamp provenance the parent can compute without loading the DB.
    current.git_sha = command_output("git", &["rev-parse", "--short=12", "HEAD"]);
    let db_path = args.data_root.join("card-data.json");
    current.card_data_hash = command_output(
        "git",
        &[
            "hash-object",
            db_path
                .to_str()
                .expect("card-data path must be valid UTF-8"),
        ],
    );

    eprintln!(
        "perf suite: seed={} action_cap={} sample_count={} scenarios={:?} wall_clock={}ms",
        current.base_seed,
        current.action_cap,
        current.sample_count,
        current.scenarios,
        current.wall_clock_ms
    );

    if let Err(err) = write_report(&current, &args.current_output) {
        eprintln!(
            "failed to write current report {}: {err}",
            args.current_output.display()
        );
        cleanup_temps(&temp_paths);
        std::process::exit(2);
    }

    if args.refresh_baseline {
        if args.baseline.exists() {
            match load_report(&args.baseline).and_then(|baseline| compare(&baseline, &current)) {
                Ok(report) => print_markdown(&report),
                Err(err) => eprintln!("could not compare old baseline: {err}"),
            }
        }
        if let Err(err) = write_report(&current, &args.baseline) {
            eprintln!(
                "failed to write baseline {}: {err}",
                args.baseline.display()
            );
            cleanup_temps(&temp_paths);
            std::process::exit(2);
        }
        eprintln!("baseline refreshed at {}", args.baseline.display());
        cleanup_temps(&temp_paths);
        return;
    }

    let baseline = match load_report(&args.baseline) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("failed to load baseline {}: {err}", args.baseline.display());
            cleanup_temps(&temp_paths);
            std::process::exit(2);
        }
    };

    let report = match compare(&baseline, &current) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("compare failed: {err}");
            cleanup_temps(&temp_paths);
            std::process::exit(2);
        }
    };
    print_markdown(&report);
    cleanup_temps(&temp_paths);
    if report.any_fail() {
        std::process::exit(1);
    }
}

/// Best-effort removal of the per-run temp sample files (ignore errors).
fn cleanup_temps(paths: &[PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

fn parse_args() -> Result<Args, String> {
    let mut data_root = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data"));
    let mut baseline = PathBuf::from(DEFAULT_BASELINE);
    let mut current_output = PathBuf::from(DEFAULT_CURRENT);
    let mut refresh_baseline = false;
    let mut emit_sample = None;
    let mut repro_report = false;
    let mut repro_inputs = Vec::new();

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--data-root" => data_root = next_path(&mut iter, "--data-root")?,
            "--baseline" => baseline = next_path(&mut iter, "--baseline")?,
            "--current-output" => current_output = next_path(&mut iter, "--current-output")?,
            "--refresh-baseline" => refresh_baseline = true,
            "--emit-sample" => emit_sample = Some(next_path(&mut iter, "--emit-sample")?),
            "--repro-report" => repro_report = true,
            "--repro-input" => repro_inputs.push(next_path(&mut iter, "--repro-input")?),
            "--help" | "-h" => return Err(String::new()),
            _ => return Err(format!("unknown option: {arg}")),
        }
    }

    Ok(Args {
        data_root,
        baseline,
        current_output,
        refresh_baseline,
        emit_sample,
        repro_report,
        repro_inputs,
    })
}

fn next_path(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<PathBuf, String> {
    next_value(iter, flag).map(PathBuf::from)
}

fn next_value(iter: &mut impl Iterator<Item = String>, flag: &str) -> Result<String, String> {
    iter.next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_report(report: &PerfReport, path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    serde_json::to_writer_pretty(BufWriter::new(file), report).map_err(std::io::Error::other)
}

fn print_usage() {
    eprintln!("Usage: cargo ai-perf-gate [--refresh-baseline]");
    eprintln!(
        "                          [--data-root DIR] [--baseline PATH] [--current-output PATH]"
    );
    eprintln!();
    eprintln!("The gate runs PERF_SAMPLE_COUNT independent sample processes and compares the");
    eprintln!("per-counter median against the committed baseline (issue #4878).");
    eprintln!();
    eprintln!("Internal flags (spawned/orchestrated automatically, not for manual use):");
    eprintln!("  --emit-sample PATH   emit one single-trajectory sample to PATH and exit");
    eprintln!(
        "  --repro-report       run the reproducibility MARGIN gate over --repro-input reports"
    );
    eprintln!("  --repro-input PATH   a validation-run report for --repro-report (repeatable)");
}
