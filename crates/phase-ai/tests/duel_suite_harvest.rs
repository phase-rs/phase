//! End-to-end guards for the U4 self-play eval-feature harvest (`--harvest`).
//!
//! These drive real duel-suite games, so they load `card-data.json` and are
//! `#[ignore]`d (CI has no card data). Opt in with:
//!
//! ```text
//! cargo test -p phase-ai --test duel_suite_harvest -- --ignored
//! ```
//!
//! They cover the verification-matrix rows that need a real game loop:
//! - **Harvest determinism + sink lifecycle** — two identical runs produce
//!   byte-identical JSONL with exactly ONE meta line (line 1) and records from
//!   multiple games sharing the one file.
//! - **Labeling correct + both classes present** — the near-50% `red-mirror`
//!   matchup at base seed `0xA157A1` yields both win and loss labels by K=12.
//!
//! The `harvest.rs` inline unit tests cover the `GameHarvester` gating / flush /
//! finish semantics at the building-block level (no card data required).

use std::path::{Path, PathBuf};

use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;
use phase_ai::duel_suite::run::{run_suite, SuiteOptions};

const RED_MIRROR: &str = "red-mirror";
const MIRROR_BASE_SEED: u64 = 0x00A1_57A1;

fn load_db() -> CardDatabase {
    let cards_dir = std::env::var("PHASE_CARDS_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("..")
                .join("..")
                .join("client")
                .join("public")
        });
    let export_path = cards_dir.join("card-data.json");
    CardDatabase::from_export(&export_path)
        .unwrap_or_else(|e| panic!("load card-data.json from {}: {e}", export_path.display()))
}

/// A line is a meta line iff its JSON carries a top-level `meta` key — key
/// presence, not substring, so a record field containing the text `"meta"`
/// can never miscount (mirrors `train_eval_weights.py`'s loader).
fn is_meta_line(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).is_ok_and(|v| v.get("meta").is_some())
}

fn harvest_opts(games: usize, base_seed: u64, harvest_path: &Path) -> SuiteOptions {
    let mut opts = SuiteOptions::new(AiDifficulty::Medium, games, base_seed);
    opts.output_path = PathBuf::from("target/duel-suite-harvest-report.json");
    opts.filter = Some(RED_MIRROR.to_string());
    opts.harvest_output = Some(harvest_path.to_path_buf());
    opts
}

/// One meta line (line 1), records from every game appended to the SAME file,
/// and byte-identical output across two runs of the same `(matchup, seed, K)`.
#[test]
#[ignore = "loads card-data.json + runs real games; opt in via --ignored"]
fn harvest_is_deterministic_with_single_meta_line() {
    let db = load_db();
    let games = 3;

    let path_a = PathBuf::from("target/test-artifacts/u4-harvest-det/run-a.jsonl");
    let path_b = PathBuf::from("target/test-artifacts/u4-harvest-det/run-b.jsonl");
    run_suite(&db, &harvest_opts(games, MIRROR_BASE_SEED, &path_a)).expect("run a");
    run_suite(&db, &harvest_opts(games, MIRROR_BASE_SEED, &path_b)).expect("run b");

    let a = std::fs::read_to_string(&path_a).expect("read a");
    let b = std::fs::read_to_string(&path_b).expect("read b");
    assert_eq!(
        a, b,
        "same (matchup, seed, K) must produce byte-identical JSONL"
    );

    let lines: Vec<&str> = a.lines().filter(|l| !l.trim().is_empty()).collect();
    let meta_lines = lines.iter().filter(|l| is_meta_line(l)).count();
    assert_eq!(meta_lines, 1, "exactly one file-scoped meta line");
    assert!(
        is_meta_line(lines[0]),
        "the meta line must be line 1, got: {}",
        lines[0]
    );

    // Records from more than one game share the single file.
    let seeds: std::collections::HashSet<u64> = lines
        .iter()
        .filter(|l| !is_meta_line(l))
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v.get("seed").and_then(|s| s.as_u64()))
        .collect();
    assert!(
        seeds.len() >= 2,
        "records from multiple games must share one file (distinct seeds: {})",
        seeds.len()
    );
}

/// The near-50% red mirror yields BOTH win and loss labels by K=12 (grown
/// through {4, 8, 12}). Deterministic — once green, green forever; a red here
/// means an AI change shifted the mirror, so bump `MIRROR_BASE_SEED`.
#[test]
#[ignore = "loads card-data.json + runs real games; opt in via --ignored"]
fn harvest_labels_both_classes_on_red_mirror() {
    let db = load_db();
    let path = PathBuf::from("target/test-artifacts/u4-harvest-labels/run.jsonl");

    let mut satisfied_at = None;
    for &games in &[4usize, 8, 12] {
        run_suite(&db, &harvest_opts(games, MIRROR_BASE_SEED, &path)).expect("harvest run");
        let text = std::fs::read_to_string(&path).expect("read jsonl");
        let mut saw_win = false;
        let mut saw_loss = false;
        for line in text
            .lines()
            .filter(|l| !l.trim().is_empty() && !is_meta_line(l))
        {
            let v: serde_json::Value = serde_json::from_str(line).expect("record json");
            match v.get("won").and_then(|w| w.as_bool()) {
                Some(true) => saw_win = true,
                Some(false) => saw_loss = true,
                None => panic!("record missing bool `won`: {line}"),
            }
        }
        if saw_win && saw_loss {
            satisfied_at = Some(games);
            break;
        }
    }

    assert!(
        satisfied_at.is_some(),
        "red-mirror produced a single label class through K=12 — an AI change \
         likely shifted the mirror; bump MIRROR_BASE_SEED"
    );
}
