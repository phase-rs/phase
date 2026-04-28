//! Phase 2 — strict-failure conversion driver.
//!
//! Reads `data/mtgish-cards.json`, runs `convert_card` per entry,
//! and emits `data/mtgish-import-report.json` with the ranked work
//! queue (top-30 unsupported variants by frequency). No card-data
//! output yet — Phase 2's exit criterion is "0 cards convert" because
//! every Rule currently fails with `ConversionGap::UnknownVariant`.
//!
//! Exit code: non-zero unless `--allow-failures` is set. CI keeps the
//! flag through phase ramp-up; once the work queue empties the flag
//! is removed.
//!
//! Usage:
//!   mtgish-convert [--allow-failures] [<cards.json>] [<report.json>]

use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, Result};

use mtgish_import::convert::convert_card_with_provenance;
use mtgish_import::provenance::ProvenanceTracker;
use mtgish_import::report::{Ctx, ImportReport};
use mtgish_import::schema::types::OracleCard;

struct Args {
    cards_path: PathBuf,
    report_path: PathBuf,
    provenance_path: Option<PathBuf>,
}

fn parse_args() -> Args {
    // Pull off the optional `--provenance <path>` flag and `--allow-failures`
    // (consumed in `main`). Remaining argv entries are positional.
    let mut args = std::env::args().skip(1).peekable();
    let mut positional: Vec<String> = Vec::new();
    let mut provenance_path: Option<PathBuf> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--allow-failures" => {}
            "--provenance" => {
                provenance_path = args.next().map(PathBuf::from);
            }
            other => positional.push(other.to_string()),
        }
    }
    Args {
        cards_path: positional
            .first()
            .cloned()
            .unwrap_or_else(|| "data/mtgish-cards.json".to_string())
            .into(),
        report_path: positional
            .get(1)
            .cloned()
            .unwrap_or_else(|| "data/mtgish-import-report.json".to_string())
            .into(),
        provenance_path,
    }
}

fn run() -> Result<(usize, usize, ImportReport)> {
    let args = parse_args();
    let raw = fs::read_to_string(&args.cards_path)
        .with_context(|| format!("reading {}", args.cards_path.display()))?;
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(&raw).context("parsing top-level array")?;

    let mut report = ImportReport::default();
    let mut tracker = ProvenanceTracker::new();
    let want_provenance = args.provenance_path.is_some();
    let mut ok = 0usize;
    let mut deser_failed = 0usize;
    report.cards_total = arr.len();

    for value in arr {
        let name = value
            .get("Name")
            .and_then(|v| v.as_str())
            .unwrap_or("<no name>")
            .to_string();
        let card: OracleCard = match serde_json::from_value(value) {
            Ok(c) => c,
            Err(_) => {
                deser_failed += 1;
                report.record("DeserializeFailure", &name);
                continue;
            }
        };
        let mut ctx = Ctx::new(name.clone(), &mut report);
        // Provenance is captured into a per-card buffer; only commit it
        // to the corpus tracker when the card converts cleanly. Drops
        // for failed conversions stay invisible to the side-channel.
        let mut per_card = mtgish_import::provenance::CardProvenance::default();
        let result = convert_card_with_provenance(
            &card,
            &mut ctx,
            if want_provenance {
                Some(&mut per_card)
            } else {
                None
            },
        );
        let saw_gap = ctx.finish();
        if result.is_ok() && !saw_gap {
            ok += 1;
            if want_provenance && !per_card.entries.is_empty() {
                *tracker.entry_for(&name) = per_card;
            }
        }
    }

    if let Some(parent) = args.report_path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).ok();
        }
    }
    let summary = report.summary_json();
    fs::write(&args.report_path, serde_json::to_string_pretty(&summary)?)
        .with_context(|| format!("writing {}", args.report_path.display()))?;

    if let Some(path) = &args.provenance_path {
        // Phase 14 will substitute the canonical card-data SHA-256 here.
        tracker
            .write_to(path, "unstamped")
            .with_context(|| format!("writing {}", path.display()))?;
        println!(
            "  provenance:         {} ({} cards, {} entries)",
            path.display(),
            tracker.cards(),
            tracker.total_entries()
        );
    }

    println!("mtgish-import phase 2:");
    println!("  cards total:        {}", report.cards_total);
    println!("  converted clean:    {ok}");
    println!("  deser failed:       {deser_failed}");
    println!("  with conversion gap:{}", report.cards_with_unsupported);
    println!("  report:             {}", args.report_path.display());

    let top: Vec<_> = {
        let mut v: Vec<_> = report.unsupported.iter().collect();
        v.sort_by_key(|b| std::cmp::Reverse(b.1.count));
        v.into_iter().take(30).collect()
    };
    println!("\ntop {} variants:", top.len());
    for (path, stat) in top {
        println!("  {:>5}  {path}", stat.count);
    }

    Ok((ok, deser_failed, report))
}

fn main() -> ExitCode {
    let allow = std::env::args().any(|a| a == "--allow-failures");
    match run() {
        Ok((ok, deser_failed, report)) => {
            let total_failures = deser_failed + report.cards_with_unsupported;
            if total_failures == 0 {
                println!("\n✓ all {ok} cards converted cleanly");
                ExitCode::SUCCESS
            } else if allow {
                println!("\n(--allow-failures) {total_failures} cards have gaps; report written");
                ExitCode::SUCCESS
            } else {
                eprintln!(
                    "\n✗ {total_failures} cards have gaps; rerun with --allow-failures during ramp-up"
                );
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::FAILURE
        }
    }
}
