use std::collections::HashSet;
use std::io::BufRead;
use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::{analyze_standard_coverage, is_fully_covered, CoverageSummary};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ci_mode = args.iter().any(|a| a == "--ci");
    let json_mode = args.iter().any(|a| a == "--json");

    let path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("FORGE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        eprintln!("Usage: coverage-report <path> [--ci] [--json]");
        eprintln!("  Or set FORGE_CARDS_PATH environment variable");
        eprintln!();
        eprintln!("Modes:");
        eprintln!("  Forge (default):  coverage-report <forge-cards-dir> [--ci]");
        eprintln!("  JSON:             coverage-report --json <data-root> [--ci]");
        eprintln!();
        eprintln!("Flags:");
        eprintln!("  --ci    Exit with code 1 if any cards are unsupported");
        eprintln!("  --json  Load cards via JSON (mtgjson + abilities) instead of Forge .txt files");
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

    let db = if json_mode {
        // JSON mode: load via CardDatabase::load_json()
        // path is the data root directory (e.g., "data/")
        let mtgjson_path = path.join("mtgjson/test_fixture.json");
        let abilities_dir = path.join("abilities");
        match CardDatabase::load_json(&mtgjson_path, &abilities_dir) {
            Ok(db) => db,
            Err(e) => {
                eprintln!("Error loading JSON card database from {}: {}", path.display(), e);
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
        }
    } else {
        // Forge mode (unchanged)
        match CardDatabase::load(&path) {
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
        }
    };

    let summary = analyze_standard_coverage(&db);

    // In JSON mode, filter to only Standard manifest cards
    let summary = if json_mode {
        let manifest_path = path.join("standard-cards.txt");
        let manifest_names = match load_manifest(&manifest_path) {
            Ok(names) => names,
            Err(e) => {
                eprintln!(
                    "Error loading manifest from {}: {}",
                    manifest_path.display(),
                    e
                );
                process::exit(1);
            }
        };
        filter_to_manifest(summary, &manifest_names)
    } else {
        summary
    };

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

/// Load the Standard card manifest file.
/// Returns a set of lowercase card names, filtering out blank lines and comments.
fn load_manifest(path: &PathBuf) -> Result<HashSet<String>, std::io::Error> {
    let file = std::fs::File::open(path)?;
    let reader = std::io::BufReader::new(file);
    let mut names = HashSet::new();
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        names.insert(trimmed.to_lowercase());
    }
    Ok(names)
}

/// Filter a CoverageSummary to only include cards whose names are in the manifest.
fn filter_to_manifest(summary: CoverageSummary, manifest: &HashSet<String>) -> CoverageSummary {
    let cards: Vec<_> = summary
        .cards
        .into_iter()
        .filter(|c| manifest.contains(&c.card_name.to_lowercase()))
        .collect();

    let total_cards = cards.len();
    let supported_cards = cards.iter().filter(|c| c.supported).count();
    let coverage_pct = if total_cards > 0 {
        (supported_cards as f64 / total_cards as f64) * 100.0
    } else {
        0.0
    };

    // Rebuild frequency from filtered cards
    let mut freq = std::collections::HashMap::new();
    for card in &cards {
        for handler in &card.missing_handlers {
            *freq.entry(handler.clone()).or_insert(0usize) += 1;
        }
    }
    let mut missing_handler_frequency: Vec<(String, usize)> = freq.into_iter().collect();
    missing_handler_frequency.sort_by(|a, b| b.1.cmp(&a.1));

    CoverageSummary {
        total_cards,
        supported_cards,
        coverage_pct,
        cards,
        missing_handler_frequency,
    }
}
