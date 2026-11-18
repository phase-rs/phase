use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine::database::card_data_provenance::CardDataProvenance;
use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, audit_resolver_features, audit_silent_drops, build_warning_drilldown,
};
use engine::parser::oracle_ir::trace::sha256_bytes;

fn read_card_data_provenance(
    data_root: &Path,
    card_data_bytes: &[u8],
) -> Result<CardDataProvenance, String> {
    let path = data_root.join("card-data.provenance.json");
    let provenance = CardDataProvenance::read(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "CARD_DATA_PROVENANCE_MISSING: {} was not found",
                path.display()
            )
        } else {
            format!(
                "CARD_DATA_PROVENANCE_MISMATCH: failed to read {}: {error}",
                path.display()
            )
        }
    })?;
    let actual_card_data_sha256 = sha256_bytes(card_data_bytes);
    if !provenance.hashes_are_well_formed()
        || provenance.card_data_sha256 != actual_card_data_sha256
    {
        return Err(format!(
            "CARD_DATA_PROVENANCE_MISMATCH: {} does not describe the exact card-data.json bytes",
            path.display()
        ));
    }
    Ok(provenance)
}

fn main() -> ExitCode {
    match run(std::env::args().skip(1), io::stdout().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("coverage-report: {error}");
            ExitCode::from(2)
        }
    }
}

fn run(args: impl Iterator<Item = String>, mut output: impl Write) -> Result<(), String> {
    let args: Vec<String> = args.collect();

    // Parse CLI flags
    let mut min_global: Option<f64> = None;
    let mut min_standard: Option<f64> = None;
    let mut run_audit = false;
    let mut brief = false;
    let mut write_stats: Option<String> = None;
    let mut write_warning_patterns: Option<String> = None;
    let mut warning_category: Option<String> = None;
    let mut warning_pattern: Option<String> = None;
    let mut warning_detector: Option<String> = None;
    let mut warning_limit: usize = 50;
    let mut warning_full = false;

    let mut args_iter = args.iter().peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--min-global" => min_global = args_iter.next().and_then(|v| v.parse().ok()),
            "--min-standard" => min_standard = args_iter.next().and_then(|v| v.parse().ok()),
            "--audit" => run_audit = true,
            "--brief" => brief = true,
            "--write-stats" => write_stats = args_iter.next().cloned(),
            "--write-warning-patterns" => write_warning_patterns = args_iter.next().cloned(),
            "--warning-category" => warning_category = args_iter.next().cloned(),
            "--warning-pattern" => warning_pattern = args_iter.next().cloned(),
            "--warning-detector" => warning_detector = args_iter.next().cloned(),
            "--warning-limit" => {
                warning_limit = args_iter
                    .next()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(warning_limit)
            }
            "--warning-full" => warning_full = true,
            _ => {}
        }
    }

    let path = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("PHASE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        return Err("Usage: coverage-report <data-root> (or set PHASE_CARDS_PATH)".to_string());
    };

    // Hash and deserialize the same pre-processed card-data.json bytes.
    let export_path = path.join("card-data.json");
    let card_data_bytes = std::fs::read(&export_path).map_err(|error| {
        format!(
            "could not load card database from {}: {error}",
            export_path.display()
        )
    })?;
    let provenance = read_card_data_provenance(&path, &card_data_bytes)?;
    let db = CardDatabase::from_export_reader(card_data_bytes.as_slice()).map_err(|error| {
        format!(
            "could not load card database from {}: {error}",
            export_path.display()
        )
    })?;
    if db.face_iter().next().is_none() {
        return Err(format!(
            "card database at {} contains no usable card data",
            export_path.display()
        ));
    }

    let misparses = engine::types::confirmed_misparse::ConfirmedMisparses::committed();
    let mut present = 0;
    let mut marked = 0;
    let mut absent: std::collections::BTreeSet<&str> =
        misparses.iter().map(|face| face.key.as_str()).collect();
    for (key, face) in db.face_iter() {
        if absent.remove(key) {
            present += 1;
        }
        if face.metadata.confirmed_misparse.is_some() {
            marked += 1;
        }
    }
    eprintln!(
        "Confirmed misparses: listed={}, exported/present={}, marked={}; absent={} sample={:?}",
        misparses.len(),
        present,
        marked,
        absent.len(),
        absent.iter().take(10).collect::<Vec<_>>()
    );

    let mut summary = analyze_coverage(&db);
    // Keep the artifact self-describing. The regression comparator uses face
    // support/gap/parser fields; set/rarity/rulings sidecars only enrich
    // metadata and set rollups, so AtomicCards is the corpus identity here.
    // The hash comes from oracle-gen's provenance sidecar, which records the
    // exact selected source bytes rather than guessing from a nearby default
    // path file.
    summary.source_corpus_hash = Some(provenance.source_corpus_sha256);

    // Populate per-category diagnostic counts for JSON output (D-08).
    {
        let mut counts: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for (_key, face) in db.face_iter() {
            for warning in &face.parse_warnings {
                *counts
                    .entry(warning.category_name().to_string())
                    .or_default() += 1;
            }
        }
        summary.diagnostics = counts;
    }

    if warning_pattern.is_some() || warning_detector.is_some() {
        let drilldown = build_warning_drilldown(
            &db,
            &summary,
            warning_category.as_deref(),
            warning_pattern.as_deref(),
            warning_detector.as_deref(),
            warning_limit,
            warning_full,
        );
        writeln!(
            output,
            "{}",
            serde_json::to_string_pretty(&drilldown).unwrap()
        )
        .map_err(|error| format!("could not write report: {error}"))?;
        eprintln!(
            "Warning drilldown: {} cards ({} supported, {} single-gap)",
            drilldown.matched_cards, drilldown.supported_cards, drilldown.single_gap_cards
        );
        return Ok(());
    }

    // Print JSON to stdout
    writeln!(
        output,
        "{}",
        serde_json::to_string_pretty(&summary).unwrap()
    )
    .map_err(|error| format!("could not write report: {error}"))?;

    // Write compact stats file if requested
    if let Some(stats_path) = &write_stats {
        let stats = serde_json::json!({
            "total_cards": summary.total_cards,
            "supported_cards": summary.supported_cards,
            "coverage_pct": (summary.coverage_pct * 10.0).round() / 10.0,
            "keyword_count": summary.keyword_count,
            "formats": summary.coverage_by_format.iter()
                .map(|(k, v)| (k.clone(), serde_json::json!({
                    "total": v.total_cards,
                    "supported": v.supported_cards,
                    "pct": (v.coverage_pct).round() as u32,
                })))
                .collect::<serde_json::Map<String, serde_json::Value>>(),
        });
        std::fs::write(stats_path, serde_json::to_string_pretty(&stats).unwrap()).unwrap_or_else(
            |e| eprintln!("Warning: failed to write stats to {}: {}", stats_path, e),
        );
        eprintln!("Wrote coverage stats to {}", stats_path);
    }

    if let Some(warning_patterns_path) = &write_warning_patterns {
        std::fs::write(
            warning_patterns_path,
            serde_json::to_string_pretty(&summary.parse_warning_patterns).unwrap(),
        )
        .unwrap_or_else(|e| {
            eprintln!(
                "Warning: failed to write parser warning patterns to {}: {}",
                warning_patterns_path, e
            )
        });
        eprintln!("Wrote parser warning patterns to {}", warning_patterns_path);
    }

    // Print human-readable summary to stderr
    eprintln!(
        "Coverage: {}/{} cards supported ({:.1}%)",
        summary.supported_cards, summary.total_cards, summary.coverage_pct
    );
    for (format, format_summary) in &summary.coverage_by_format {
        eprintln!(
            "  {} legal: {}/{} fully supported ({:.1}%)",
            format,
            format_summary.supported_cards,
            format_summary.total_cards,
            format_summary.coverage_pct
        );
    }

    // Print parse warnings grouped by category
    {
        let mut warnings_by_category: std::collections::BTreeMap<String, Vec<String>> =
            std::collections::BTreeMap::new();
        for (_key, face) in db.face_iter() {
            for warning in &face.parse_warnings {
                let category_name = warning.category_name();
                warnings_by_category
                    .entry(category_name.to_string())
                    .or_default()
                    .push(face.name.clone());
            }
        }
        if !warnings_by_category.is_empty() {
            let total: usize = warnings_by_category.values().map(|v| v.len()).sum();
            eprintln!();
            eprintln!(
                "Parse warnings: {} warnings across {} categories",
                total,
                warnings_by_category.len()
            );
            for (category, cards) in &warnings_by_category {
                let unique: std::collections::BTreeSet<&str> =
                    cards.iter().map(|s| s.as_str()).collect();
                eprintln!(
                    "  {} — {} warnings ({} cards)",
                    category,
                    cards.len(),
                    unique.len()
                );
            }

            if !brief && !summary.parse_warning_patterns.is_empty() {
                eprintln!();
                eprintln!("Top parser warning patterns by otherwise-supported cards:");
                for pattern in summary.parse_warning_patterns.iter().take(5) {
                    eprintln!(
                        "  {} — {} warnings, {} cards, {} otherwise-supported, {} single-gap",
                        pattern.pattern,
                        pattern.warning_count,
                        pattern.card_count,
                        pattern.otherwise_supported_cards,
                        pattern.single_gap_cards
                    );
                }
                if write_warning_patterns.is_none() {
                    eprintln!(
                        "  Run with --write-warning-patterns <path> for the full parser warning pattern report."
                    );
                }
            }
        }
    }

    // Print top gaps with format breakdown, independence ratio, and oracle patterns
    if !brief && !summary.top_gaps.is_empty() {
        eprintln!();
        eprintln!("Top gaps by single-gap card unlock potential:");
        for (i, gap) in summary.top_gaps.iter().take(15).enumerate() {
            if gap.single_gap_cards == 0 {
                continue;
            }
            let format_str: String = ["standard", "modern", "pioneer", "pauper", "commander"]
                .iter()
                .filter_map(|&f| {
                    gap.single_gap_by_format
                        .get(f)
                        .map(|count| format!("{}:{}", &f[..3], count))
                })
                .collect::<Vec<_>>()
                .join(" ");
            let ratio_str = gap
                .independence_ratio
                .map(|r| format!(" (ind: {:.0}%)", r * 100.0))
                .unwrap_or_default();
            eprintln!(
                "  {} — {} total, {} single-gap{} [{}]",
                gap.handler, gap.total_count, gap.single_gap_cards, ratio_str, format_str
            );

            // Show top 3 oracle patterns for the first 5 gaps
            if i < 5 {
                for pattern in gap.oracle_patterns.iter().take(3) {
                    eprintln!(
                        "    «{}» ×{} (e.g. {})",
                        pattern.pattern,
                        pattern.count,
                        pattern.example_cards.first().unwrap_or(&String::new())
                    );
                }
            }
        }
    }

    // Print top gap bundles
    let two_gap_bundles: Vec<_> = summary
        .gap_bundles
        .iter()
        .filter(|b| b.handlers.len() == 2)
        .take(5)
        .collect();
    if !brief && !two_gap_bundles.is_empty() {
        eprintln!();
        eprintln!("Top 2-gap bundles (implementing both unlocks cards):");
        for bundle in two_gap_bundles {
            eprintln!(
                "  [{}] — {} cards",
                bundle.handlers.join(" + "),
                bundle.unlocked_cards,
            );
        }
    }

    // Run silent-drop audit if requested
    if run_audit {
        let drops = audit_silent_drops(&summary);
        eprintln!();
        eprintln!(
            "Silent-drop audit: {} cards flagged out of {} supported",
            drops.len(),
            summary.supported_cards
        );
        for drop in &drops {
            eprintln!(
                "  {} — oracle:{} parsed:{} delta:{} missing:[{}]",
                drop.card_name,
                drop.oracle_lines,
                drop.parsed_items,
                drop.delta,
                drop.missing_lines.join("; ")
            );
        }
        // Also output audit results as JSON to stdout (after the main summary)
        let audit_json = serde_json::json!({
            "total_supported_audited": summary.supported_cards,
            "silent_drops_found": drops.len(),
            "cards": drops,
        });
        // Print audit JSON to stderr as a separate block for easy extraction
        eprintln!();
        eprintln!("AUDIT_JSON_START");
        eprintln!("{}", serde_json::to_string_pretty(&audit_json).unwrap());
        eprintln!("AUDIT_JSON_END");
    }

    // Run resolver feature audit if requested
    if run_audit {
        let resolver_audit = audit_resolver_features(&db);
        eprintln!();
        eprintln!(
            "Resolver feature audit: {} supported cards audited, {} with unhandled features",
            resolver_audit.total_supported_audited, resolver_audit.cards_with_unhandled_features
        );
        if !resolver_audit.unhandled_features.is_empty() {
            eprintln!();
            eprintln!("Unhandled features (resolver may silently ignore):");
            for feat in &resolver_audit.unhandled_features {
                let examples = feat.example_cards.join(", ");
                eprintln!(
                    "  {} — {} cards (e.g. {})",
                    feat.feature, feat.card_count, examples
                );
            }
        }
        // Show top handled features by usage for informational purposes
        let top_handled: Vec<_> = resolver_audit
            .all_features
            .iter()
            .filter(|f| f.handled)
            .take(10)
            .collect();
        if !top_handled.is_empty() {
            eprintln!();
            eprintln!("Top handled features by card count:");
            for feat in top_handled {
                eprintln!("  {} — {} cards", feat.feature, feat.card_count);
            }
        }
        if !resolver_audit.flagged_cards.is_empty() {
            eprintln!();
            eprintln!("Flagged cards ({}):", resolver_audit.flagged_cards.len());
            for card in resolver_audit.flagged_cards.iter().take(20) {
                eprintln!(
                    "  {} — [{}]",
                    card.card_name,
                    card.unhandled_features.join(", ")
                );
            }
            if resolver_audit.flagged_cards.len() > 20 {
                eprintln!("  ... and {} more", resolver_audit.flagged_cards.len() - 20);
            }
        }
    }

    // Check threshold enforcement
    let mut failed = false;
    if let Some(min) = min_global {
        if summary.coverage_pct < min {
            eprintln!(
                "FAIL: Global coverage {:.1}% < minimum {:.1}%",
                summary.coverage_pct, min
            );
            failed = true;
        }
    }
    if let Some(min) = min_standard {
        if let Some(std_cov) = summary.coverage_by_format.get("standard") {
            if std_cov.coverage_pct < min {
                eprintln!(
                    "FAIL: Standard coverage {:.1}% < minimum {:.1}%",
                    std_cov.coverage_pct, min
                );
                failed = true;
            }
        }
    }
    if failed {
        return Err("coverage threshold not met".to_string());
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    const CARD_DATA: &str = include_str!("../../tests/fixtures/runtime_card_export_fixture.json");

    fn source_corpus_sha256() -> String {
        "a".repeat(64)
    }

    /// A data root whose provenance sidecar describes the exact export bytes,
    /// as `oracle-gen --provenance-out` writes it.
    fn data_root(card_data: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("card-data.json"), card_data).unwrap();
        CardDataProvenance {
            source_corpus_sha256: source_corpus_sha256(),
            card_data_sha256: sha256_bytes(card_data.as_bytes()),
        }
        .write_atomic(&dir.path().join("card-data.provenance.json"))
        .unwrap();
        dir
    }

    #[test]
    fn unavailable_card_export_returns_error_without_rendering_a_report() {
        let dir = tempfile::tempdir().unwrap();
        let mut output = Vec::new();

        let error = run(
            [dir.path().to_string_lossy().into_owned()].into_iter(),
            &mut output,
        )
        .unwrap_err();

        assert!(error.contains("card-data.json"));
        assert!(
            output.is_empty(),
            "unavailable data must not emit a zero-valued report"
        );
    }

    #[test]
    fn card_export_renders_a_non_empty_report() {
        let dir = data_root(CARD_DATA);
        let mut output = Vec::new();

        run(
            [
                dir.path().to_string_lossy().into_owned(),
                "--brief".to_string(),
            ]
            .into_iter(),
            &mut output,
        )
        .unwrap();

        let report: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert!(report["total_cards"].as_u64().unwrap() > 0);
        assert_eq!(report["source_corpus_hash"], source_corpus_sha256());
        assert!(!output.is_empty());
    }

    #[test]
    fn missing_or_mismatched_provenance_returns_error_without_rendering_a_report() {
        let missing = data_root(CARD_DATA);
        std::fs::remove_file(missing.path().join("card-data.provenance.json")).unwrap();
        let mismatched = data_root(CARD_DATA);
        CardDataProvenance {
            source_corpus_sha256: source_corpus_sha256(),
            card_data_sha256: "b".repeat(64),
        }
        .write_atomic(&mismatched.path().join("card-data.provenance.json"))
        .unwrap();

        for (dir, code) in [
            (&missing, "CARD_DATA_PROVENANCE_MISSING"),
            (&mismatched, "CARD_DATA_PROVENANCE_MISMATCH"),
        ] {
            let mut output = Vec::new();
            let error = run(
                [dir.path().to_string_lossy().into_owned()].into_iter(),
                &mut output,
            )
            .unwrap_err();

            assert!(error.starts_with(code), "expected {code}, got {error}");
            assert!(output.is_empty(), "{code} must not emit a report");
        }
    }
}
