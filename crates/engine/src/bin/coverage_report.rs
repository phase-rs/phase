use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::{analyze_standard_coverage, CoverageSummary};

fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("FORGE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        eprintln!("Usage: coverage-report <card-data-dir>");
        eprintln!("  Or set FORGE_CARDS_PATH environment variable");
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
    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}
