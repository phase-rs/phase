use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::analyze_coverage;
use engine::game::gap_analysis::{analyze_gaps, GapCategory};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut near_misses_only = false;
    let mut category_filter: Option<String> = None;
    let mut verb_filter: Option<String> = None;

    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--near-misses-only" => near_misses_only = true,
            "--category" => category_filter = args_iter.next().cloned(),
            "--verb" => verb_filter = args_iter.next().cloned(),
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
        eprintln!("Usage: parser-gap-analyzer <data-root> [OPTIONS]");
        eprintln!();
        eprintln!("Classifies parser gaps by failure reason to surface quick wins.");
        eprintln!("Loads cards from <data-root>/card-data.json.");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --near-misses-only    Show only categories A-D (parser-fix gaps)");
        eprintln!("  --category <CAT>      Filter to a single category (A, B, C, D, F, G)");
        eprintln!("  --verb <VERB>         Filter Category A/B to a specific verb");
        process::exit(0);
    };

    let export_path = path.join("card-data.json");
    let db = match CardDatabase::from_export(&export_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!(
                "Error loading card database from {}: {}",
                export_path.display(),
                e
            );
            process::exit(1);
        }
    };

    eprintln!("Analyzing coverage...");
    let summary = analyze_coverage(&db);
    eprintln!("Classifying gaps...");
    let mut analysis = analyze_gaps(&summary);

    // Apply filters
    if near_misses_only {
        let near_miss_keys: Vec<String> = [
            GapCategory::VerbVariation,
            GapCategory::SubjectStripping,
            GapCategory::TriggerEffect,
            GapCategory::StaticCondition,
        ]
        .iter()
        .map(|c| c.label().to_string())
        .collect();

        analysis
            .categories
            .retain(|k, _| near_miss_keys.contains(k));
        analysis
            .quick_wins
            .retain(|w| near_miss_keys.contains(&w.category));
    }

    if let Some(ref cat) = category_filter {
        let cat_label = match cat.to_uppercase().as_str() {
            "A" => GapCategory::VerbVariation.label(),
            "B" => GapCategory::SubjectStripping.label(),
            "C" => GapCategory::TriggerEffect.label(),
            "D" => GapCategory::StaticCondition.label(),
            "F" => GapCategory::NewMechanic.label(),
            "G" => GapCategory::Unclassified.label(),
            _ => {
                eprintln!("Unknown category: {}. Use A, B, C, D, F, or G.", cat);
                process::exit(1);
            }
        };
        analysis.categories.retain(|k, _| k == cat_label);
        analysis.quick_wins.retain(|w| w.category == cat_label);
    }

    if let Some(ref verb) = verb_filter {
        let verb_lower = verb.to_lowercase();
        for cat_summary in analysis.categories.values_mut() {
            cat_summary.by_verb.retain(|b| b.verb == verb_lower);
        }
        analysis
            .quick_wins
            .retain(|w| w.verb.as_deref() == Some(verb_lower.as_str()));
    }

    // JSON to stdout
    println!("{}", serde_json::to_string_pretty(&analysis).unwrap());

    // Human-readable to stderr
    eprintln!();
    eprintln!(
        "Parser Gap Analysis: {} unsupported cards, {} classified gaps",
        analysis.total_unsupported, analysis.total_classified
    );
    eprintln!();

    for (cat_label, cat_summary) in &analysis.categories {
        eprintln!(
            "  {} — {} gaps, {} single-gap unlocks",
            cat_label, cat_summary.count, cat_summary.single_gap_unlocks
        );

        // Show top verb breakdowns
        for verb_bd in cat_summary.by_verb.iter().take(5) {
            if verb_bd.single_gap_unlocks > 0 {
                eprintln!(
                    "    verb '{}': {} gaps, {} single-gap unlocks",
                    verb_bd.verb, verb_bd.count, verb_bd.single_gap_unlocks
                );
                for pattern in verb_bd.top_patterns.iter().take(2) {
                    eprintln!("      «{}» ×{}", pattern.pattern, pattern.count);
                }
            }
        }

        // Show top patterns for non-verb categories
        if cat_summary.by_verb.is_empty() {
            for pattern in cat_summary.top_patterns.iter().take(3) {
                eprintln!("    «{}» ×{}", pattern.pattern, pattern.count);
            }
        }
    }

    if !analysis.quick_wins.is_empty() {
        eprintln!();
        eprintln!("Quick wins (sorted by cards unlocked):");
        for (i, win) in analysis.quick_wins.iter().take(10).enumerate() {
            eprintln!(
                "  {}. {} — unlocks {} cards",
                i + 1,
                win.description,
                win.cards_unlocked
            );
        }
    }
}
