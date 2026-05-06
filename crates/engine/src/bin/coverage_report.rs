use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_coverage, audit_resolver_features, audit_silent_drops, CoverageSummary,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Parse CLI flags
    let mut min_global: Option<f64> = None;
    let mut min_standard: Option<f64> = None;
    let mut run_audit = false;
    let mut brief = false;
    let mut write_stats: Option<String> = None;
    let mut write_warning_patterns: Option<String> = None;

    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--min-global" => min_global = args_iter.next().and_then(|v| v.parse().ok()),
            "--min-standard" => min_standard = args_iter.next().and_then(|v| v.parse().ok()),
            "--audit" => run_audit = true,
            "--brief" => brief = true,
            "--write-stats" => write_stats = args_iter.next().cloned(),
            "--write-warning-patterns" => write_warning_patterns = args_iter.next().cloned(),
            _ => {}
        }
    }

    let path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("PHASE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        eprintln!("Usage: coverage-report <data-root>");
        eprintln!("  Or set PHASE_CARDS_PATH environment variable");
        eprintln!();
        eprintln!("Loads cards from <data-root>/card-data.json (pre-processed export).");
        eprintln!("Options:");
        eprintln!("  --brief                          Suppress detailed human report sections.");
        eprintln!("  --write-warning-patterns <path>  Write full parser warning pattern report.");
        eprintln!();
        eprintln!("Outputs JSON coverage summary to stdout and human-readable summary to stderr.");
        let empty = CoverageSummary {
            total_cards: 0,
            supported_cards: 0,
            coverage_pct: 0.0,
            keyword_count: 0,
            coverage_by_format: Default::default(),
            coverage_by_set: Default::default(),
            cards: vec![],
            top_gaps: vec![],
            gap_bundles: vec![],
            parse_warning_patterns: vec![],
            diagnostics: Default::default(),
        };
        println!("{}", serde_json::to_string_pretty(&empty).unwrap());
        process::exit(0);
    };

    // Load via CardDatabase::from_export() using the pre-processed card-data.json
    let export_path = path.join("card-data.json");
    let db = match CardDatabase::from_export(&export_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "Error loading card database from {}: {}",
                export_path.display(),
                e
            );
            let empty = CoverageSummary {
                total_cards: 0,
                supported_cards: 0,
                coverage_pct: 0.0,
                keyword_count: 0,
                coverage_by_format: Default::default(),
                coverage_by_set: Default::default(),
                cards: vec![],
                top_gaps: vec![],
                gap_bundles: vec![],
                parse_warning_patterns: vec![],
                diagnostics: Default::default(),
            };
            println!("{}", serde_json::to_string_pretty(&empty).unwrap());
            process::exit(1);
        }
    };

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

    // Print JSON to stdout
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());

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
        process::exit(1);
    }
}
