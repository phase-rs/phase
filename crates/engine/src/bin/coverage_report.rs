use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::{analyze_coverage, CoverageSummary};

fn main() {
    let args: Vec<String> = std::env::args().collect();

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
        eprintln!();
        eprintln!("Outputs JSON coverage summary to stdout and human-readable summary to stderr.");
        let empty = CoverageSummary {
            total_cards: 0,
            supported_cards: 0,
            coverage_pct: 0.0,
            coverage_by_format: Default::default(),
            cards: vec![],
            missing_handler_frequency: vec![],
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
                coverage_by_format: Default::default(),
                cards: vec![],
                missing_handler_frequency: vec![],
            };
            println!("{}", serde_json::to_string_pretty(&empty).unwrap());
            process::exit(1);
        }
    };

    let summary = analyze_coverage(&db);

    // Print JSON to stdout
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());

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
}
