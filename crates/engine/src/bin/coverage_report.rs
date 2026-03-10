use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::{analyze_standard_coverage, is_fully_covered, CoverageSummary};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ci_mode = args.iter().any(|a| a == "--ci");

    let path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("FORGE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        eprintln!("Usage: coverage-report <card-data-dir> [--ci]");
        eprintln!("  Or set FORGE_CARDS_PATH environment variable");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --ci    Exit with code 1 if any cards are unsupported");
        eprintln!();
        eprintln!("Outputting empty coverage summary to stdout.");
        let empty = CoverageSummary {
            total_cards: 0,
            supported_cards: 0,
            coverage_pct: 0.0,
            cards: vec![],
            missing_handler_frequency: vec![],
        };
        println!("{}", serde_json::to_string_pretty(&empty).unwrap());
        process::exit(0);
    };

    let db = match CardDatabase::load(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database from {}: {}", path.display(), e);
            let empty = CoverageSummary {
                total_cards: 0,
                supported_cards: 0,
                coverage_pct: 0.0,
                cards: vec![],
                missing_handler_frequency: vec![],
            };
            println!("{}", serde_json::to_string_pretty(&empty).unwrap());
            process::exit(1);
        }
    };

    let summary = analyze_standard_coverage(&db);

    // Print JSON to stdout
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());

    // Print human-readable summary to stderr
    eprintln!(
        "Coverage: {}/{} cards supported ({:.1}%)",
        summary.supported_cards, summary.total_cards, summary.coverage_pct
    );

    if ci_mode {
        if !is_fully_covered(&summary) {
            let unsupported: Vec<_> = summary.cards.iter().filter(|c| !c.supported).collect();
            eprintln!();
            eprintln!("UNSUPPORTED CARDS ({}):", unsupported.len());
            for card in &unsupported {
                eprintln!(
                    "  {} - missing: {}",
                    card.card_name,
                    card.missing_handlers.join(", ")
                );
            }
            eprintln!();
            eprintln!(
                "CI FAILED: {} unsupported Standard-legal cards",
                unsupported.len()
            );
            process::exit(1);
        }
        eprintln!("CI PASSED: 100% Standard-legal coverage");
    }
}
