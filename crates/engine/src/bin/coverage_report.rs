use std::collections::HashSet;
use std::io::BufRead;
use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::game::coverage::{
    analyze_standard_coverage, is_fully_covered, CardCoverageResult, CoverageSummary,
};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let ci_mode = args.iter().any(|a| a == "--ci");

    let path = args
        .iter()
        .skip(1)
        .find(|a| !a.starts_with("--"))
        .cloned()
        .or_else(|| std::env::var("PHASE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        eprintln!("Usage: coverage-report <data-root> [--ci]");
        eprintln!("  Or set PHASE_CARDS_PATH environment variable");
        eprintln!();
        eprintln!("Loads cards from <data-root>/card-data.json (pre-processed export).");
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
                cards: vec![],
                missing_handler_frequency: vec![],
            };
            println!("{}", serde_json::to_string_pretty(&empty).unwrap());
            process::exit(1);
        }
    };

    let summary = analyze_standard_coverage(&db);

    // Filter to only Standard manifest cards
    let manifest_path = path.join("standard-cards.txt");
    let summary = match load_manifest(&manifest_path) {
        Ok(manifest_names) => filter_to_manifest(summary, &manifest_names),
        Err(e) => {
            eprintln!(
                "Error loading manifest from {}: {}",
                manifest_path.display(),
                e
            );
            process::exit(1);
        }
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
/// Also strips benign MTGJSON keyword mismatches (bare parameterized keywords and
/// action keywords like Scry/Mill that MTGJSON tracks but Forge doesn't).
fn filter_to_manifest(summary: CoverageSummary, manifest: &HashSet<String>) -> CoverageSummary {
    let cards: Vec<_> = summary
        .cards
        .into_iter()
        .filter(|c| manifest.contains(&c.card_name.to_lowercase()))
        .map(strip_benign_keyword_mismatches)
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

/// MTGJSON provides bare keyword names (e.g. "Flashback", "Protection") without
/// the colon-delimited parameters that Forge uses (e.g. "Flashback:{2}{U}",
/// "Protection:Demon"). The engine's Keyword::from_str treats these bare names as
/// Unknown because they're parameterized keywords missing their parameter.
///
/// Additionally, MTGJSON tracks action keywords (Scry, Mill) that Forge doesn't
/// include in its K: keyword lines -- these are handled as effects, not keywords.
///
/// This function strips these known-benign mismatches from a card's missing handlers,
/// matching the same allowlisting pattern established in the parity tests (Plan 02).
fn strip_benign_keyword_mismatches(mut card: CardCoverageResult) -> CardCoverageResult {
    // Keywords that the engine supports as parameterized variants but MTGJSON sends bare
    const KNOWN_PARAMETERIZED: &[&str] = &[
        "Protection",
        "Flashback",
        "Cycling",
        "Ward",
        "Kicker",
        "Equip",
        "Landwalk",
        "Ninjutsu",
        "Morph",
        "Madness",
        "Dash",
        "Emerge",
        "Escape",
        "Evoke",
        "Foretell",
        "Mutate",
        "Disturb",
        "Disguise",
        "Blitz",
        "Overload",
        "Spectacle",
        "Surge",
        "Buyback",
        "Echo",
        "Outlast",
        "Bestow",
        "Embalm",
        "Eternalize",
        "Unearth",
        "Reconfigure",
    ];

    // MTGJSON action keywords not in the engine's Keyword enum (handled as effects)
    const MTGJSON_ACTION_KEYWORDS: &[&str] = &["Scry", "Mill", "Fateseal", "Surveil"];

    card.missing_handlers.retain(|handler| {
        if let Some(kw_name) = handler.strip_prefix("Keyword:") {
            // Strip if it's a known parameterized keyword sent bare by MTGJSON
            if KNOWN_PARAMETERIZED
                .iter()
                .any(|k| k.eq_ignore_ascii_case(kw_name))
            {
                return false;
            }
            // Strip if it's an MTGJSON-only action keyword
            if MTGJSON_ACTION_KEYWORDS
                .iter()
                .any(|k| k.eq_ignore_ascii_case(kw_name))
            {
                return false;
            }
        }
        true
    });

    card.supported = card.missing_handlers.is_empty();
    card
}
