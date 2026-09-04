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

// pod-lab loop-3 Q5: native-binary throughput lever, gated in Cargo.toml so
// wasm32 builds of this crate's lib (pulled in by engine-wasm/draft-wasm)
// never see it.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use engine::database::CardDatabase;
use phase_ai::duel_suite::perf::{
    compare, default_scenarios, load_report, median_report, print_markdown, print_repro_margin,
    render_error_markdown, repro_margin_report, run_perf_suite, PerfReport, PERF_ACTION_CAP,
    PERF_BASE_SEED, PERF_SAMPLE_COUNT,
};
use phase_ai::duel_suite::{find_matchup, resolve_deck_ref};

const DEFAULT_BASELINE: &str = "crates/phase-ai/baselines/perf-baseline.json";
const DEFAULT_CURRENT: &str = "target/ai-perf-gate-current.json";

/// `run_perf_suite`'s AI search recurses deeper than the platform default
/// thread stack on Windows (confirmed: every child overflows immediately
/// after game start under this crate's release profile). Same root cause and
/// fix as `ai_commander.rs`'s `GAME_THREAD_STACK_SIZE` / `duel_suite::run`'s
/// identical spawn — this binary just never got the fix applied.
const PERF_THREAD_STACK_SIZE: usize = 32 << 20;

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
    // clean table; diagnostics go to stderr only. Runs on a large-stack thread
    // (see `PERF_THREAD_STACK_SIZE`) since the AI search recurses past the
    // platform default; an unhandled panic there would otherwise unwind only
    // the spawned thread and exit 0 silently, so a join failure is mapped to
    // exit 101 (mirrors `ai_commander.rs`'s identical convention).
    if let Some(sample_path) = &args.emit_sample {
        let data_root = args.data_root.clone();
        let sample_path = sample_path.clone();
        let handle = std::thread::Builder::new()
            .name("ai-perf-gate-sample".to_string())
            .stack_size(PERF_THREAD_STACK_SIZE)
            .spawn(move || run_child_sample(&data_root, &sample_path))
            .expect("failed to spawn perf-sample thread");
        if handle.join().is_err() {
            std::process::exit(101);
        }
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
    current.card_data_hash = gate_card_data_hash(&args.data_root);

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

    // Both bail-outs print the refusal to STDOUT as well as stderr. The workflow redirects
    // stdout into `target/ai-perf-gate-report.md` and posts it as a drift issue, and nothing
    // on the path above this point writes to stdout — so a stderr-only refusal left that file
    // at zero bytes and the workflow answered it with "failed without a drift report" and no
    // issue. The exit code and a non-empty body are needed TOGETHER; either alone posts nothing.
    let baseline = match load_report(&args.baseline) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("failed to load baseline {}: {err}", args.baseline.display());
            print!("{}", render_error_markdown(&err));
            cleanup_temps(&temp_paths);
            std::process::exit(2);
        }
    };

    let report = match compare(&baseline, &current) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("compare failed: {err}");
            print!("{}", render_error_markdown(&err));
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

/// Provenance hash over ONLY the `card-data.json` entries this gate's scenarios
/// actually consume.
///
/// This was previously `git hash-object data/card-data.json` — the whole file,
/// ~35.6k cards. The three scenarios in [`default_scenarios`] draw from
/// committed, frozen decks (inline builders plus pinned snapshots) naming ~46.
/// `card-data.json` is a DERIVED artifact of both MTGJSON and the Oracle parser,
/// so every unrelated set release *and every parser change* moved the stamp.
/// `PerfCompareReport::card_data_changed` was therefore true on nearly every
/// run, which made it useless for the one judgement it exists to support:
/// telling a genuine cost-per-node regression (hashes equal) apart from a
/// card-data-driven trajectory shift (hashes differ).
///
/// Measured over five local card-data vintages spanning 2026-07-25..2026-08-04,
/// one pair differing only by an Oracle-parser change and not by MTGJSON: five
/// distinct whole-file hashes, exactly ONE distinct gate-subset hash.
///
/// Still a `git hash-object` blob SHA, so the field's format and meaning are
/// unchanged and no hashing dependency enters this crate. Re-serializing is
/// canonical: this workspace leaves serde_json's `preserve_order` off, so object
/// keys serialize sorted (documented at `engine/src/bin/set_check.rs`), and the
/// `BTreeMap`s below fix the order of everything above them. A card a deck names
/// but `card-data.json` lacks is simply absent from the subset — which still
/// moves the hash, since the key set shrinks.
///
/// Returns `None` on any failure, matching [`command_output`]'s convention:
/// `card_data_changed()` then reports false rather than inventing a delta. Every
/// failure path announces itself on stderr — an unstamped run must not read as a
/// clean one.
fn gate_card_data_hash(data_root: &Path) -> Option<String> {
    // DB-free by construction: `resolve_deck_ref` expands inline builders and
    // pinned snapshots without a `CardDatabase`, preserving this branch's
    // documented never-loads-the-DB property.
    let mut names = BTreeSet::new();
    for id in default_scenarios() {
        let Some(matchup) = find_matchup(id) else {
            eprintln!("provenance: scenario {id:?} does not resolve — card-data hash unstamped");
            return None;
        };
        for deck in [&matchup.p0, &matchup.p1] {
            match resolve_deck_ref(deck) {
                // The engine keys `card-data.json` by Rust `to_lowercase()`.
                Ok(cards) => names.extend(cards.into_iter().map(|c| c.to_lowercase())),
                Err(err) => {
                    eprintln!(
                        "provenance: deck {deck:?} failed to resolve ({err}) — card-data hash unstamped"
                    );
                    return None;
                }
            }
        }
    }

    let db_path = data_root.join("card-data.json");
    let db: BTreeMap<String, serde_json::Value> = match std::fs::read_to_string(&db_path)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string()))
    {
        Ok(db) => db,
        Err(err) => {
            eprintln!(
                "provenance: could not read {} ({err}) — card-data hash unstamped",
                db_path.display()
            );
            return None;
        }
    };
    let subset: BTreeMap<&str, &serde_json::Value> = names
        .iter()
        .filter_map(|name| db.get(name).map(|entry| (name.as_str(), entry)))
        .collect();

    // `git hash-object` needs a path, so the canonical subset goes through a
    // temp file. Removed on every exit path, including the failures below.
    let tmp = std::env::temp_dir().join(format!("ai-perf-gate-cards-{}.json", std::process::id()));
    let hash = match File::create(&tmp)
        .map_err(|e| e.to_string())
        .and_then(|file| {
            let mut writer = BufWriter::new(file);
            serde_json::to_writer(&mut writer, &subset).map_err(|e| e.to_string())?;
            // `BufWriter` discards errors from its drop-time flush, so a failed
            // final write would leave a TRUNCATED subset that `git hash-object`
            // still hashes — yielding a well-formed provenance stamp for content
            // that was never written. Flush explicitly and fail closed instead.
            writer.flush().map_err(|e| e.to_string())
        }) {
        Ok(()) => match tmp.to_str() {
            Some(path) => command_output("git", &["hash-object", path]),
            None => {
                eprintln!("provenance: temp path is not valid UTF-8 — card-data hash unstamped");
                None
            }
        },
        Err(err) => {
            eprintln!(
                "provenance: could not write the card subset ({err}) — card-data hash unstamped"
            );
            None
        }
    };
    let _ = std::fs::remove_file(&tmp);

    if hash.is_some() {
        eprintln!(
            "provenance: card-data hash covers {} of {} deck-named cards from scenarios {:?}",
            subset.len(),
            names.len(),
            default_scenarios()
        );
    }
    hash
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
