use std::collections::HashMap;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, audit_resolver_features, audit_silent_drops, parse_warning_pattern,
    CardCoverageResult, CoverageSummary, GapDetail, ParsedItem,
};
use engine::parser::oracle_ir::diagnostic::{OracleDiagnostic, OracleItemId, OracleSourceSpan};
use engine::types::card::CardFace;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct WarningDrilldown {
    category: Option<String>,
    pattern: Option<String>,
    detector: Option<String>,
    matched_cards: usize,
    supported_cards: usize,
    single_gap_cards: usize,
    cards: Vec<WarningDrilldownCard>,
}

#[derive(Debug, Serialize)]
struct WarningDrilldownCard {
    name: String,
    key: String,
    supported: Option<bool>,
    gap_count: Option<usize>,
    printings: Vec<String>,
    oracle_text: Option<String>,
    warnings: Vec<WarningDrilldownWarning>,
    parsed_labels: Vec<String>,
    gap_details: Vec<GapDetail>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_details: Option<Vec<ParsedItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    card: Option<CardFace>,
}

#[derive(Debug, Serialize)]
struct WarningDrilldownWarning {
    category: String,
    pattern: String,
    line_index: usize,
    text: String,
    /// Unit provenance, present only on stamped swallow-audit findings. The span is the
    /// card-absolute byte range of the audit unit's OWN lines — line-granular today, so
    /// two clauses on one line share it (see `OracleDiagnostic::SwallowedClause`).
    /// `items` is the unit's pooled evidence set, legitimately empty for a card that
    /// lowered to no items.
    #[serde(skip_serializing_if = "Option::is_none")]
    unit_span: Option<OracleSourceSpan>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    evidence_items: Vec<OracleItemId>,
}

fn build_warning_drilldown(
    db: &CardDatabase,
    summary: &CoverageSummary,
    category: Option<&str>,
    pattern: Option<&str>,
    detector: Option<&str>,
    limit: usize,
    full: bool,
) -> WarningDrilldown {
    let coverage_by_name: HashMap<String, &CardCoverageResult> = summary
        .cards
        .iter()
        .map(|card| (card.card_name.to_ascii_lowercase(), card))
        .collect();
    let mut cards = Vec::new();

    for (key, face) in db.face_iter() {
        let oracle_text = face.oracle_text.as_deref();
        let warnings: Vec<_> = face
            .parse_warnings
            .iter()
            .filter_map(|warning| {
                let (warning_category, warning_pattern) =
                    parse_warning_pattern(warning, oracle_text);
                if category.is_some_and(|expected| expected != warning_category) {
                    return None;
                }
                if pattern.is_some_and(|expected| expected != warning_pattern) {
                    return None;
                }
                if detector.is_some_and(|expected| !warning_detector_is(warning, expected)) {
                    return None;
                }
                Some(WarningDrilldownWarning {
                    category: warning_category,
                    pattern: warning_pattern,
                    line_index: warning.line_index(),
                    text: warning_text(warning),
                    unit_span: warning.unit_span().cloned(),
                    evidence_items: warning.evidence_items().to_vec(),
                })
            })
            .collect();

        if warnings.is_empty() {
            continue;
        }

        let coverage = coverage_by_name
            .get(&face.name.to_ascii_lowercase())
            .copied();
        cards.push(WarningDrilldownCard {
            name: face.name.clone(),
            key: key.to_string(),
            supported: coverage.map(|card| card.supported),
            gap_count: coverage.map(|card| card.gap_count),
            printings: coverage
                .map(|card| card.printings.clone())
                .unwrap_or_default(),
            oracle_text: face.oracle_text.clone(),
            warnings,
            parsed_labels: coverage
                .map(|card| parsed_labels(&card.parse_details))
                .unwrap_or_default(),
            gap_details: coverage
                .map(|card| card.gap_details.clone())
                .unwrap_or_default(),
            parse_details: if full {
                coverage.map(|card| card.parse_details.clone())
            } else {
                None
            },
            card: full.then(|| face.clone()),
        });
    }

    cards.sort_by(|left, right| {
        let left_supported = left.supported.unwrap_or(false);
        let right_supported = right.supported.unwrap_or(false);
        right_supported
            .cmp(&left_supported)
            .then_with(|| {
                left.gap_count
                    .unwrap_or(usize::MAX)
                    .cmp(&right.gap_count.unwrap_or(usize::MAX))
            })
            .then_with(|| left.name.cmp(&right.name))
    });

    let matched_cards = cards.len();
    let supported_cards = cards
        .iter()
        .filter(|card| card.supported == Some(true))
        .count();
    let single_gap_cards = cards
        .iter()
        .filter(|card| card.gap_count == Some(1))
        .count();
    cards.truncate(limit);

    WarningDrilldown {
        category: category.map(str::to_string),
        pattern: pattern.map(str::to_string),
        detector: detector.map(str::to_string),
        matched_cards,
        supported_cards,
        single_gap_cards,
        cards,
    }
}

fn warning_detector_is(warning: &OracleDiagnostic, expected: &str) -> bool {
    matches!(
        warning,
        OracleDiagnostic::SwallowedClause { detector, .. } if detector == expected
    )
}

fn warning_text(warning: &OracleDiagnostic) -> String {
    match warning {
        OracleDiagnostic::TargetFallback { text, .. }
        | OracleDiagnostic::IgnoredRemainder { text, .. } => text.clone(),
        OracleDiagnostic::SwallowedClause { description, .. } => description.clone(),
        OracleDiagnostic::CascadeLoss {
            slot, effect_name, ..
        } => format!("{slot:?}: {effect_name}"),
    }
}

fn parsed_labels(items: &[ParsedItem]) -> Vec<String> {
    let mut labels = Vec::new();
    collect_parsed_labels(items, &mut labels);
    labels
}

fn collect_parsed_labels(items: &[ParsedItem], labels: &mut Vec<String>) {
    for item in items {
        labels.push(format!("{:?}:{}", item.category, item.label));
        collect_parsed_labels(&item.children, labels);
    }
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

    // Load via CardDatabase::from_export() using the pre-processed card-data.json
    let export_path = path.join("card-data.json");
    let db = CardDatabase::from_export(&export_path).map_err(|error| {
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

    let mut summary = analyze_coverage(&db);

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

    fn data_root(card_data: &str) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("card-data.json"), card_data).unwrap();
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
        assert!(!output.is_empty());
    }
}
