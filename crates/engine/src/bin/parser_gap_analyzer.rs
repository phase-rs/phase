use std::path::PathBuf;
use std::process;

use engine::database::legality::{LegalityFormat, LegalityStatus};
use engine::database::CardDatabase;
use engine::game::coverage::analyze_coverage;
use engine::game::gap_analysis::{analyze_gaps, GapClass};

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut category_filter: Option<GapClass> = None;
    let mut format_filter: Option<LegalityFormat> = None;

    let mut args_iter = args.iter().skip(1).peekable();
    while let Some(arg) = args_iter.next() {
        match arg.as_str() {
            "--category" => {
                let raw = args_iter.next().cloned().unwrap_or_default();
                match GapClass::from_label(&raw) {
                    Some(class) => category_filter = Some(class),
                    None => {
                        eprintln!(
                            "Unknown --category value '{}'. Valid categories: {}",
                            raw,
                            category_labels()
                        );
                        process::exit(1);
                    }
                }
            }
            "--format" => {
                let raw = args_iter.next().cloned().unwrap_or_default();
                match LegalityFormat::from_key(&raw) {
                    Some(fmt) => format_filter = Some(fmt),
                    None => {
                        let valid: Vec<&'static str> =
                            LegalityFormat::ALL.iter().map(|f| f.as_key()).collect();
                        eprintln!(
                            "Unknown --format value '{}'. Valid formats: {}",
                            raw,
                            valid.join(", ")
                        );
                        process::exit(1);
                    }
                }
            }
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
        eprintln!(
            "Groups unsupported cards' coverage gaps by typed diagnosis (the category) and by the"
        );
        eprintln!("phrase, feature or handler it names (the family).");
        eprintln!("Loads cards from <data-root>/card-data.json.");
        eprintln!();
        eprintln!("Options:");
        eprintln!(
            "  --category <KEY>      Restrict the report to one category ({})",
            category_labels()
        );
        eprintln!(
            "  --format <FORMAT>     Restrict gaps to cards legal in a format ({})",
            LegalityFormat::ALL
                .iter()
                .map(|f| f.as_key())
                .collect::<Vec<_>>()
                .join(", ")
        );
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
    let mut summary = analyze_coverage(&db);

    if let Some(fmt) = format_filter {
        let before = summary.cards.len();
        summary
            .cards
            .retain(|c| db.legality_status(&c.card_name, fmt) == Some(LegalityStatus::Legal));
        eprintln!(
            "Filtered to {}: {} / {} cards retained",
            fmt.as_key(),
            summary.cards.len(),
            before
        );
    }

    eprintln!("Grouping gaps...");
    let mut analysis = analyze_gaps(&summary.cards);

    if let Some(class) = category_filter {
        let label = class.label();
        analysis.categories.retain(|key, _| *key == label);
    }

    // JSON to stdout
    println!("{}", serde_json::to_string_pretty(&analysis).unwrap());

    // Human-readable to stderr
    eprintln!();
    eprintln!(
        "Parser Gap Analysis: {} unsupported cards, {} gaps",
        analysis.total_unsupported, analysis.total_classified
    );
    eprintln!();
    for (label, category) in &analysis.categories {
        eprintln!(
            "  {} — {} gaps, {} cards affected, {} fixed alone",
            label, category.tally.count, category.tally.cards_affected, category.tally.fixes_alone
        );
        for family in category.families.iter().take(3) {
            eprintln!(
                "    «{}» — {} cards affected, {} fixed alone",
                family.key, family.tally.cards_affected, family.tally.fixes_alone
            );
        }
    }
}

/// Every valid `--category` value, for the usage text and the unknown-value error.
fn category_labels() -> String {
    GapClass::all()
        .map(GapClass::label)
        .collect::<Vec<_>>()
        .join(", ")
}
