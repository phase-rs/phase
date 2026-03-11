use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use engine::parser::parse_card_file;
use engine::schema::{AbilityFile, FaceAbilities};
use engine::types::card::{CardLayout, CardRules};
use walkdir::WalkDir;

/// Migration statistics.
#[derive(Default)]
struct MigrationStats {
    total: usize,
    converted: usize,
    errors: Vec<(PathBuf, String, String)>, // (source_path, card_name, message)
    warnings: Vec<(PathBuf, String, String)>,
}

/// Convert a CardRules to an AbilityFile for JSON serialization.
fn card_rules_to_ability_file(rules: &CardRules) -> AbilityFile {
    match &rules.layout {
        CardLayout::Single(face) => AbilityFile {
            schema: Some("schema.json".to_string()),
            abilities: face.abilities.clone(),
            triggers: face.triggers.clone(),
            statics: face.static_abilities.clone(),
            replacements: face.replacements.clone(),
            faces: vec![],
        },
        CardLayout::Split(a, b)
        | CardLayout::Flip(a, b)
        | CardLayout::Transform(a, b)
        | CardLayout::Meld(a, b)
        | CardLayout::Adventure(a, b)
        | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b) => AbilityFile {
            schema: Some("schema.json".to_string()),
            abilities: vec![],
            triggers: vec![],
            statics: vec![],
            replacements: vec![],
            faces: vec![face_to_abilities(a), face_to_abilities(b)],
        },
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![face_to_abilities(base)];
            faces.extend(variants.iter().map(face_to_abilities));
            AbilityFile {
                schema: Some("schema.json".to_string()),
                abilities: vec![],
                triggers: vec![],
                statics: vec![],
                replacements: vec![],
                faces,
            }
        }
    }
}

/// Convert a CardFace into FaceAbilities for multi-face cards.
fn face_to_abilities(face: &engine::types::card::CardFace) -> FaceAbilities {
    FaceAbilities {
        abilities: face.abilities.clone(),
        triggers: face.triggers.clone(),
        statics: face.static_abilities.clone(),
        replacements: face.replacements.clone(),
    }
}

/// Convert a card name to a snake_case filename.
/// "Lightning Bolt" -> "lightning_bolt"
/// "Jace, the Mind Sculptor" -> "jace_the_mind_sculptor"
fn card_name_to_filename(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_alphanumeric() {
                c.to_lowercase().next().unwrap()
            } else {
                '_'
            }
        })
        .collect::<String>()
        .split('_')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("_")
}

/// Serializable migration report.
#[derive(serde::Serialize)]
struct MigrationReport {
    summary: ReportSummary,
    errors: Vec<ReportEntry>,
    warnings: Vec<ReportEntry>,
}

#[derive(serde::Serialize)]
struct ReportSummary {
    total: usize,
    converted: usize,
    errors: usize,
    warnings: usize,
}

#[derive(serde::Serialize)]
struct ReportEntry {
    source_path: String,
    card_name: String,
    message: String,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let forge_dir = PathBuf::from(
        args.get(1)
            .map(|s| s.as_str())
            .unwrap_or("data/cardsfolder"),
    );
    let output_dir = PathBuf::from(args.get(2).map(|s| s.as_str()).unwrap_or("data/abilities"));

    if !forge_dir.is_dir() {
        eprintln!("Error: Forge directory not found: {}", forge_dir.display());
        std::process::exit(1);
    }
    fs::create_dir_all(&output_dir).expect("Failed to create output directory");

    let mut stats = MigrationStats::default();
    // Track which output filenames we've already written to detect collisions
    let mut written_files: HashMap<String, PathBuf> = HashMap::new();

    for entry in WalkDir::new(&forge_dir).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("txt") {
            continue;
        }

        stats.total += 1;

        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                stats.errors.push((
                    path.to_path_buf(),
                    String::new(),
                    format!("Failed to read file: {e}"),
                ));
                continue;
            }
        };

        let card_rules = match parse_card_file(&content) {
            Ok(r) => r,
            Err(e) => {
                stats.errors.push((
                    path.to_path_buf(),
                    String::new(),
                    format!("Parse error: {e}"),
                ));
                continue;
            }
        };

        let card_name = card_rules.name().to_string();
        let ability_file = card_rules_to_ability_file(&card_rules);
        let filename = card_name_to_filename(&card_name);

        // Check for filename collision (e.g. multiple Forge files producing same output name)
        if let Some(prev_path) = written_files.get(&filename) {
            stats.warnings.push((
                path.to_path_buf(),
                card_name.clone(),
                format!(
                    "Filename collision: {filename}.json already written from {}",
                    prev_path.display()
                ),
            ));
            // Still overwrite -- last wins (consistent behavior)
        }
        written_files.insert(filename.clone(), path.to_path_buf());

        // Oracle text heuristic check
        check_oracle_heuristic(&card_rules, path, &mut stats);

        let json = match serde_json::to_string_pretty(&ability_file) {
            Ok(j) => j,
            Err(e) => {
                stats.errors.push((
                    path.to_path_buf(),
                    card_name,
                    format!("Serialization error: {e}"),
                ));
                continue;
            }
        };

        let output_path = output_dir.join(format!("{filename}.json"));
        if let Err(e) = fs::write(&output_path, format!("{json}\n")) {
            stats
                .errors
                .push((path.to_path_buf(), card_name, format!("Write error: {e}")));
            continue;
        }

        stats.converted += 1;
    }

    // Print summary to stdout
    println!("Migration complete:");
    println!("  Total files:  {}", stats.total);
    println!("  Converted:    {}", stats.converted);
    println!("  Errors:       {}", stats.errors.len());
    println!("  Warnings:     {}", stats.warnings.len());

    // Write detailed report
    let report = MigrationReport {
        summary: ReportSummary {
            total: stats.total,
            converted: stats.converted,
            errors: stats.errors.len(),
            warnings: stats.warnings.len(),
        },
        errors: stats
            .errors
            .iter()
            .map(|(p, n, m)| ReportEntry {
                source_path: p.display().to_string(),
                card_name: n.clone(),
                message: m.clone(),
            })
            .collect(),
        warnings: stats
            .warnings
            .iter()
            .map(|(p, n, m)| ReportEntry {
                source_path: p.display().to_string(),
                card_name: n.clone(),
                message: m.clone(),
            })
            .collect(),
    };

    let report_json = serde_json::to_string_pretty(&report).expect("Failed to serialize report");
    fs::write("migration-report.json", format!("{report_json}\n"))
        .expect("Failed to write migration report");
    println!("  Report:       migration-report.json");
}

/// Heuristic oracle text check: compare keywords from oracle text against parsed keywords.
fn check_oracle_heuristic(
    card_rules: &CardRules,
    source_path: &std::path::Path,
    stats: &mut MigrationStats,
) {
    let face = match &card_rules.layout {
        CardLayout::Single(f) => f,
        CardLayout::Split(f, _)
        | CardLayout::Flip(f, _)
        | CardLayout::Transform(f, _)
        | CardLayout::Meld(f, _)
        | CardLayout::Adventure(f, _)
        | CardLayout::Modal(f, _)
        | CardLayout::Omen(f, _)
        | CardLayout::Specialize(f, _) => f,
    };

    let oracle = match &face.oracle_text {
        Some(text) => text.to_lowercase(),
        None => return,
    };

    let common_keywords = [
        "flying",
        "trample",
        "lifelink",
        "deathtouch",
        "first strike",
        "double strike",
        "vigilance",
        "haste",
        "reach",
        "menace",
        "hexproof",
        "indestructible",
        "flash",
    ];

    let parsed_keywords_lower: Vec<String> = face
        .keywords
        .iter()
        .map(|k| format!("{k:?}").to_lowercase())
        .collect();

    for kw in &common_keywords {
        // Check if the oracle text starts with or contains the keyword as a distinct word
        // (avoid false positives from substrings like "flash" in "flashback")
        let has_in_oracle = oracle.starts_with(kw)
            || oracle.contains(&format!("\n{kw}"))
            || oracle.contains(&format!(", {kw}"));
        let has_in_parsed = parsed_keywords_lower.iter().any(|pk| pk == kw);

        if has_in_oracle && !has_in_parsed {
            stats.warnings.push((
                source_path.to_path_buf(),
                card_rules.name().to_string(),
                format!("Oracle text mentions '{kw}' but keyword not in parsed keywords list"),
            ));
        }
    }
}
