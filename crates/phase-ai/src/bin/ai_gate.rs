use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;

use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;
use phase_ai::duel_suite::compare::{compare, load_report, print_markdown, CompareOptions};
use phase_ai::duel_suite::run::{run_suite, SuiteOptions, SuiteReport};

const DEFAULT_BASELINE: &str = "crates/phase-ai/baselines/suite-baseline.json";
const DEFAULT_CURRENT: &str = "target/ai-gate-current.json";
// Quick PR-gate matchup set (comma-separated id substrings). `red-mirror` is the
// fast aggro-mirror smoke; `affinity-mirror` and `enchantress-mirror` are the
// floor-crossing artifacts/enchantments decks that exercise ArtifactSynergyPolicy
// and EnchantmentsPayoffPolicy (commitment >= COMMITMENT_FLOOR), so the required
// gate actually runs the policies these baselines are meant to guard.
const DEFAULT_QUICK_FILTER: &str = "red-mirror,affinity-mirror,enchantress-mirror";
const DEFAULT_SEED: u64 = 0xA1_57A1;

struct Args {
    data_root: PathBuf,
    baseline: PathBuf,
    current_output: PathBuf,
    games: usize,
    seed: u64,
    difficulty: AiDifficulty,
    suite_filter: Option<String>,
    refresh_baseline: bool,
}

fn main() {
    let args = match parse_args() {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}");
            print_usage();
            std::process::exit(2);
        }
    };

    let db_path = args.data_root.join("card-data.json");
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

    let mut options = SuiteOptions::new(args.difficulty, args.games, args.seed);
    options.output_path = args.current_output.clone();
    options.filter = args.suite_filter.clone();
    options.git_sha = command_output("git", &["rev-parse", "--short=12", "HEAD"]);
    options.card_data_hash = command_output("git", &["hash-object", path_str(&db_path)]);

    let current = match run_suite(&db, &options) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("suite run failed: {err}");
            std::process::exit(1);
        }
    };

    if args.refresh_baseline {
        if args.baseline.exists() {
            match load_report(&args.baseline)
                .and_then(|baseline| compare(&baseline, &current, &CompareOptions))
            {
                Ok(report) => print_markdown(&report),
                Err(err) => eprintln!("could not compare old baseline: {err}"),
            }
        }
        if let Err(err) = write_report(&current, &args.baseline) {
            eprintln!(
                "failed to write baseline {}: {err}",
                args.baseline.display()
            );
            std::process::exit(1);
        }
        eprintln!("baseline refreshed at {}", args.baseline.display());
        return;
    }

    let baseline = match load_report(&args.baseline) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("failed to load baseline {}: {err}", args.baseline.display());
            std::process::exit(2);
        }
    };

    let report = match compare(&baseline, &current, &CompareOptions) {
        Ok(report) => report,
        Err(err) => {
            eprintln!("compare failed: {err}");
            std::process::exit(2);
        }
    };
    print_markdown(&report);
    if report.any_fail() {
        std::process::exit(1);
    }
}

fn parse_args() -> Result<Args, String> {
    let mut data_root = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("data"));
    let mut baseline = PathBuf::from(DEFAULT_BASELINE);
    let mut current_output = PathBuf::from(DEFAULT_CURRENT);
    let mut games = 10usize;
    let mut seed = DEFAULT_SEED;
    let mut difficulty = AiDifficulty::Medium;
    let mut suite_filter = Some(DEFAULT_QUICK_FILTER.to_string());
    let mut refresh_baseline = false;

    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--data-root" => {
                data_root = next_path(&mut iter, "--data-root")?;
            }
            "--baseline" => {
                baseline = next_path(&mut iter, "--baseline")?;
            }
            "--current-output" => {
                current_output = next_path(&mut iter, "--current-output")?;
            }
            "--games" => {
                games = next_value(&mut iter, "--games")?
                    .parse()
                    .map_err(|_| "--games must be a positive integer".to_string())?;
            }
            "--seed" => {
                seed = next_value(&mut iter, "--seed")?
                    .parse()
                    .map_err(|_| "--seed must be an integer".to_string())?;
            }
            "--difficulty" => {
                // Case-insensitive; unknown labels fall back to Medium via
                // `AiDifficulty::from_label`. The determinization gate runs
                // `--difficulty hard` on both branch and baseline so the pair
                // isolates the K=2-vs-K=0 delta (§11).
                difficulty = AiDifficulty::from_label(&next_value(&mut iter, "--difficulty")?);
            }
            "--suite-filter" => {
                suite_filter = Some(next_value(&mut iter, "--suite-filter")?);
            }
            "--full-suite" => suite_filter = None,
            "--refresh-baseline" => refresh_baseline = true,
            "--help" | "-h" => return Err(String::new()),
            _ => return Err(format!("unknown option: {arg}")),
        }
    }

    Ok(Args {
        data_root,
        baseline,
        current_output,
        games,
        seed,
        difficulty,
        suite_filter,
        refresh_baseline,
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

fn path_str(path: &Path) -> &str {
    path.to_str().unwrap_or("")
}

fn write_report(report: &SuiteReport, path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = File::create(path)?;
    serde_json::to_writer_pretty(BufWriter::new(file), report).map_err(std::io::Error::other)
}

fn print_usage() {
    eprintln!("Usage: cargo ai-gate [--refresh-baseline] [--games N] [--seed S]");
    eprintln!("                     [--difficulty {{medium|hard|veryhard|cedh}}]");
    eprintln!("                     [--suite-filter STR[,STR...] | --full-suite]");
    eprintln!("                     [--data-root DIR] [--baseline PATH] [--current-output PATH]");
}
