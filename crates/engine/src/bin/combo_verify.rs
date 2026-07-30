//! `combo-verify` — drive the combo-detection corpus through the engine's detector
//! and report, per row, confirmed / gated / deferred / FAILED.
//!
//! This bin is a THIN harness: it loads the card-data export, calls the shared
//! `engine::analysis::drive_row` (which owns all detection — the engine), and
//! formats the result. Exit `0` = no FAIL; `1` = ≥1 driven-row regression; `2` =
//! the export is missing / failed to load. Gated and deferred rows are expected
//! and never count as failures.

use std::path::PathBuf;
use std::process;

use engine::analysis::{drive_row, DeferralBucket, RowStatus};
use engine::database::CardDatabase;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let json = args.iter().any(|a| a == "--json");

    // Resolve the data root: first positional non-flag arg, else PHASE_CARDS_PATH.
    let root = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("PHASE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(root) = root else {
        eprintln!("Usage: combo-verify <data-root> [--json]");
        eprintln!();
        eprintln!(
            "Drives the {}-row combo corpus through the engine's detector and reports",
            engine::analysis::corpus_len()
        );
        eprintln!("per row: confirmed / gated / deferred / FAILED.");
        eprintln!("Loads cards from <data-root>/card-data.json (or PHASE_CARDS_PATH).");
        process::exit(2);
    };

    let export = root.join("card-data.json");
    let db = match CardDatabase::from_export(&export) {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "Error loading card database from {}: {}",
                export.display(),
                e
            );
            process::exit(2);
        }
    };

    let mut confirmed = 0usize;
    let mut gated = 0usize;
    let mut deferred = 0usize;
    let mut failed = 0usize;
    let mut json_rows: Vec<String> = Vec::new();

    let total = engine::analysis::corpus_len();
    for idx in 0..total {
        let report = drive_row(&db, idx);
        // Build a plain string DTO here (no serde on the engine enums): the axis /
        // win_kind / family are rendered via their Debug impls.
        let (glyph, status_str, detail) = match &report.status {
            RowStatus::Confirmed {
                unbounded,
                win_kind,
            } => {
                confirmed += 1;
                ("✓", "confirmed", format!("{win_kind:?} {unbounded:?}"))
            }
            RowStatus::Gated { card } => {
                gated += 1;
                ("·", "gated", format!("gated on {card}"))
            }
            RowStatus::Deferred { bucket } => {
                deferred += 1;
                ("–", "deferred", deferral_label(*bucket).to_string())
            }
            RowStatus::Failed { detail } => {
                failed += 1;
                ("✗", "FAILED", detail.clone())
            }
        };

        if json {
            json_rows.push(format!(
                "    {{\"idx\": {idx}, \"name\": {}, \"status\": \"{status_str}\", \
                 \"expected_family\": \"{:?}\", \"expected_win_kind\": \"{:?}\", \"detail\": {}}}",
                json_str(report.name),
                report.expected_family,
                report.expected_win_kind,
                json_str(&detail),
            ));
        } else {
            println!(
                "{glyph} [{idx:>2}] {status_str:<9} {:<52} exp {:?}/{:?} | {detail}",
                report.name, report.expected_family, report.expected_win_kind,
            );
        }
    }

    if json {
        println!("{{");
        println!(
            "  \"summary\": {{\"confirmed\": {confirmed}, \"gated\": {gated}, \
             \"deferred\": {deferred}, \"failed\": {failed}, \"total\": {total}}},"
        );
        println!("  \"rows\": [");
        println!("{}", json_rows.join(",\n"));
        println!("  ]");
        println!("}}");
    } else {
        println!();
        println!(
            "{confirmed} confirmed / {gated} gated / {deferred} deferred / {failed} failed (of {total})"
        );
    }

    // Exit non-zero ONLY on a true FAIL (a driven-row regression); gated + deferred
    // are expected outcomes.
    process::exit(if failed > 0 { 1 } else { 0 });
}

/// Human-readable label for a deferral bucket (the measured structural reason a
/// non-gated row is not yet driven on today's in-place loop model).
fn deferral_label(b: DeferralBucket) -> &'static str {
    match b {
        DeferralBucket::ObjectReentry => "object re-entry (fresh ObjectId each cycle)",
        DeferralBucket::ExtraTurnOrCombat => "extra-turn / extra-combat re-entry",
        DeferralBucket::ColorConverting => "color-converting per-color net-progress",
        DeferralBucket::Other => "no bespoke driver on today's in-place loop model",
    }
}

/// Minimal JSON string escaping — the bin emits its own DTO so the engine enums
/// (`ResourceAxis` / `WinKind` / `ResourceFamily`) need no `Serialize` derive.
fn json_str(s: &str) -> String {
    serde_json::to_string(s).expect("serializing a string slice to JSON cannot fail")
}
