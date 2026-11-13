use std::process;

use engine::database::legality::LegalityFormat;
use engine::game::gap_analysis::{gap_report_from_args, GapArgsError, GapClass, GapReport};

fn main() {
    let GapReport { analysis, retained } = match gap_report_from_args(std::env::args()) {
        Ok(report) => report,
        Err(GapArgsError::Usage) => {
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
        }
        Err(GapArgsError::UnknownCategory(raw)) => {
            eprintln!(
                "Unknown --category value '{}'. Valid categories: {}",
                raw,
                category_labels()
            );
            process::exit(1);
        }
        Err(GapArgsError::UnknownFormat(raw)) => {
            let valid: Vec<&'static str> = LegalityFormat::ALL.iter().map(|f| f.as_key()).collect();
            eprintln!(
                "Unknown --format value '{}'. Valid formats: {}",
                raw,
                valid.join(", ")
            );
            process::exit(1);
        }
        Err(GapArgsError::Load(export_path, e)) => {
            eprintln!(
                "Error loading card database from {}: {}",
                export_path.display(),
                e
            );
            process::exit(1);
        }
    };
    if let Some(rows) = retained {
        eprintln!(
            "Filtered to {}: {} / {} cards retained",
            rows.format.as_key(),
            rows.kept,
            rows.before
        );
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
