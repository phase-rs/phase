use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process;

use serde::{Deserialize, Serialize};

use engine::database::legality::{legalities_to_export_map, normalize_legalities};
use engine::database::mtgjson::{load_atomic_cards, AtomicCard, Ruling};
use engine::database::synthesis::{
    build_oracle_face, build_oracle_face_multi, layout_faces, map_layout, LayoutKind,
};
use engine::database::CardDatabase;
use engine::game::coverage::{
    audit_semantic, card_face_has_unimplemented_parts, format_semantic_audit_markdown,
};
use engine::types::card::{CardFace, CardLayout};

#[derive(Debug, Clone, Serialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: BTreeMap<String, String>,
    /// MTGJSON layout string for multi-face cards (e.g. "modal_dfc", "transform",
    /// "adventure"). Enables the runtime card database to determine the correct
    /// `LayoutKind` when loading from the export (where `CardRules` is not available).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layout: Option<String>,
    /// Set codes the card has been printed in (from MTGJSON `printings`).
    /// Used by the coverage dashboard to group supported/gap cards by set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    printings: Vec<String>,
    /// Official WotC rulings for the card. MTGJSON duplicates the same rulings
    /// across every face of a multi-face card; we attach them to the front
    /// face only (index 0). Back faces receive an empty vec. Rulings describe
    /// the card as a whole, not a specific face, so no information is lost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    rulings: Vec<Ruling>,
}

fn build_export_layout(
    faces: &[AtomicCard],
    oracle_id: Option<String>,
    layout_kind: LayoutKind,
) -> CardLayout {
    if faces.len() >= 2 {
        let face_a = build_oracle_face_multi(&faces[0], oracle_id.clone());
        let face_b = build_oracle_face_multi(&faces[1], oracle_id);
        match layout_kind {
            LayoutKind::Split => CardLayout::Split(face_a, face_b),
            LayoutKind::Flip => CardLayout::Flip(face_a, face_b),
            LayoutKind::Transform => CardLayout::Transform(face_a, face_b),
            LayoutKind::Meld => CardLayout::Meld(face_a, face_b),
            LayoutKind::Adventure => CardLayout::Adventure(face_a, face_b),
            LayoutKind::Modal => CardLayout::Modal(face_a, face_b),
            LayoutKind::Single => CardLayout::Single(face_a),
        }
    } else {
        CardLayout::Single(build_oracle_face(&faces[0], oracle_id))
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    // Check for semantic-audit subcommand before normal parsing
    if args.get(1).map(|s| s.as_str()) == Some("semantic-audit") {
        run_semantic_audit(&args[2..]);
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("rulings") {
        run_rulings(&args[2..]);
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("set-list") {
        run_set_list(&args[2..]);
        return;
    }

    if args.get(1).map(|s| s.as_str()) == Some("decks") {
        run_decks(&args[2..]);
        return;
    }

    let mut data_dir: Option<PathBuf> = None;
    let mut mtgjson_override: Option<PathBuf> = None;
    let mut names_out: Option<PathBuf> = None;
    let mut stats = false;
    let mut filter_names: Vec<String> = Vec::new();
    #[cfg(feature = "forge")]
    let mut forge_path: Option<PathBuf> = None;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--mtgjson" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --mtgjson requires a path argument");
                    process::exit(1);
                }
                mtgjson_override = Some(PathBuf::from(&args[i]));
            }
            "--names-out" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --names-out requires a path argument");
                    process::exit(1);
                }
                names_out = Some(PathBuf::from(&args[i]));
            }
            "--stats" => {
                stats = true;
            }
            "--filter" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --filter requires card name(s) separated by |");
                    process::exit(1);
                }
                filter_names = args[i]
                    .split('|')
                    .map(|s| s.trim().to_lowercase())
                    .collect();
            }
            #[cfg(feature = "forge")]
            "--forge" => {
                i += 1;
                if i >= args.len() {
                    eprintln!("Error: --forge requires a path to Forge cardsfolder/");
                    process::exit(1);
                }
                forge_path = Some(PathBuf::from(&args[i]));
            }
            _ if data_dir.is_none() && !args[i].starts_with('-') => {
                data_dir = Some(PathBuf::from(&args[i]));
            }
            other => {
                eprintln!("Unknown argument: {other}");
                process::exit(1);
            }
        }
        i += 1;
    }

    let data_dir = data_dir.or_else(|| std::env::var("PHASE_DATA_DIR").ok().map(PathBuf::from));

    let mtgjson_path = match mtgjson_override {
        Some(p) => p,
        None => match &data_dir {
            Some(d) => d.join("mtgjson/AtomicCards.json"),
            None => {
                eprintln!("Usage: oracle-gen <data-dir> [--mtgjson <path>] [--stats]");
                eprintln!("  Parses Oracle text from MTGJSON and outputs card-data export JSON");
                process::exit(1);
            }
        },
    };

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oracle_gen=info,engine=info".parse().unwrap()),
        )
        .init();

    if !mtgjson_path.exists() {
        eprintln!("Error: {} not found", mtgjson_path.display());
        process::exit(1);
    }

    let atomic = match load_atomic_cards(&mtgjson_path) {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Error loading MTGJSON: {e}");
            process::exit(1);
        }
    };

    // Build Forge index: --forge flag > PHASE_FORGE_PATH env var > data/forge-cardsfolder/ default.
    #[cfg(feature = "forge")]
    let forge_index = {
        let explicit = forge_path.is_some() || std::env::var("PHASE_FORGE_PATH").is_ok();
        let default_path = data_dir
            .as_ref()
            .map(|d| d.join("forge-cardsfolder"))
            .unwrap_or_else(|| PathBuf::from("data/forge-cardsfolder"));
        let path = forge_path
            .or_else(|| std::env::var("PHASE_FORGE_PATH").ok().map(PathBuf::from))
            .unwrap_or(default_path);
        if path.exists() {
            eprintln!("Building Forge index from: {}", path.display());
            let idx = engine::database::forge::ForgeIndex::scan(&path);
            eprintln!("Forge index: {} face names", idx.len());
            Some(idx)
        } else if explicit {
            // Only warn if the user explicitly requested a path that doesn't exist.
            eprintln!("Warning: Forge path {} not found, skipping", path.display());
            None
        } else {
            None
        }
    };

    let mut face_index: BTreeMap<String, CardExportEntry> = BTreeMap::new();
    let mut total_cards = 0u32;
    let mut cards_with_unimplemented = 0u32;

    for faces in atomic.data.values() {
        // --filter: skip cards not matching any filter name
        if !filter_names.is_empty() {
            let card_name = faces
                .first()
                .map(|f| f.name.to_lowercase())
                .unwrap_or_default();
            if !filter_names.iter().any(|n| card_name.contains(n)) {
                continue;
            }
        }

        total_cards += 1;

        let oracle_id = faces
            .first()
            .and_then(|f| f.identifiers.scryfall_oracle_id.clone());

        let layout_kind = map_layout(&faces[0].layout);

        if faces.len() >= 2 {
            let mut legalities_by_face = BTreeMap::new();
            let layout = build_export_layout(faces, oracle_id, layout_kind);
            for (face, source) in layout_faces(&layout).iter().zip(faces.iter()) {
                legalities_by_face.insert(
                    face.name.to_lowercase(),
                    legalities_to_export_map(&normalize_legalities(&source.legalities)),
                );
            }

            if stats {
                let has_unimplemented = layout_faces(&layout)
                    .iter()
                    .any(|f| card_face_has_unimplemented_parts(f));
                if has_unimplemented {
                    cards_with_unimplemented += 1;
                }
            }

            for (face_idx, face_ref) in layout_faces(&layout).into_iter().enumerate() {
                let key = face_ref.name.to_lowercase();
                let legalities = legalities_by_face.remove(&key).unwrap_or_default();
                let face = face_ref.clone();
                #[cfg(feature = "forge")]
                if let Some(ref fi) = forge_index {
                    engine::database::forge::apply_forge_fallback(&mut face, fi);
                }
                let layout_str = match layout_kind {
                    LayoutKind::Single => None,
                    _ => Some(faces[0].layout.clone()),
                };
                // Front face (index 0) owns the rulings; back faces get an empty vec.
                // MTGJSON duplicates rulings across faces; this dedups at export time.
                let rulings = if face_idx == 0 {
                    faces[0].rulings.clone()
                } else {
                    Vec::new()
                };
                face_index.insert(
                    key,
                    CardExportEntry {
                        face,
                        legalities,
                        layout: layout_str,
                        printings: faces[0].printings.clone(),
                        rulings,
                    },
                );
            }
        } else {
            let face = build_oracle_face(&faces[0], oracle_id);
            #[cfg(feature = "forge")]
            if let Some(ref fi) = forge_index {
                engine::database::forge::apply_forge_fallback(&mut face, fi);
            }
            let key = face.name.to_lowercase();
            let legalities = legalities_to_export_map(&normalize_legalities(&faces[0].legalities));

            if stats && card_face_has_unimplemented_parts(&face) {
                cards_with_unimplemented += 1;
            }

            face_index.insert(
                key,
                CardExportEntry {
                    face,
                    legalities,
                    layout: None,
                    printings: faces[0].printings.clone(),
                    rulings: faces[0].rulings.clone(),
                },
            );
        }
    }

    println!(
        "{}",
        serde_json::to_string(&face_index).expect("Failed to serialize card data")
    );

    if let Some(names_path) = names_out {
        let mut names: Vec<&str> = face_index.values().map(|e| e.face.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        let names_json = serde_json::to_string(&names).expect("Failed to serialize card names");
        std::fs::write(&names_path, names_json)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", names_path.display()));
        eprintln!("Card names written: {} names", names.len());
    }

    if stats {
        eprintln!("Total cards: {total_cards}");
        eprintln!("Faces indexed: {}", face_index.len());
        eprintln!("Cards with unimplemented effects: {cards_with_unimplemented}");
        let implemented = total_cards.saturating_sub(cards_with_unimplemented);
        let pct = if total_cards > 0 {
            (implemented as f64 / total_cards as f64) * 100.0
        } else {
            0.0
        };
        eprintln!("Fully implemented: {implemented}/{total_cards} ({pct:.1}%)");
    }
}

fn run_semantic_audit(remaining_args: &[String]) {
    // Parse optional data dir from remaining args
    let card_data_path = if let Some(dir) = remaining_args.first() {
        PathBuf::from(dir).join("card-data.json")
    } else {
        // Default: try PHASE_DATA_DIR, then client/public/card-data.json
        std::env::var("PHASE_CARDS_PATH")
            .map(|p| PathBuf::from(p).join("card-data.json"))
            .or_else(|_| {
                std::env::var("PHASE_DATA_DIR").map(|d| PathBuf::from(d).join("card-data.json"))
            })
            .unwrap_or_else(|_| PathBuf::from("client/public/card-data.json"))
    };

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "oracle_gen=info,engine=info".parse().unwrap()),
        )
        .init();

    if !card_data_path.exists() {
        eprintln!(
            "Error: card-data.json not found at {}",
            card_data_path.display()
        );
        eprintln!("Run ./scripts/gen-card-data.sh first, or pass a data directory.");
        process::exit(1);
    }

    eprintln!("Loading card database from {}...", card_data_path.display());
    let card_db = match CardDatabase::from_export(&card_data_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database: {e}");
            process::exit(1);
        }
    };

    eprintln!("Running semantic audit...");
    let summary = audit_semantic(&card_db);

    eprintln!(
        "Audit complete: {} cards audited, {} with findings",
        summary.total_supported_audited, summary.cards_with_findings
    );

    // Write JSON output
    let json_path = PathBuf::from("data/semantic-audit.json");
    let json_str =
        serde_json::to_string_pretty(&summary).expect("Failed to serialize audit summary");
    std::fs::write(&json_path, &json_str)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", json_path.display()));
    eprintln!("JSON written to {}", json_path.display());

    // Write markdown output
    let md_path = PathBuf::from("data/semantic-audit.md");
    let md_str = format_semantic_audit_markdown(&summary);
    std::fs::write(&md_path, &md_str)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", md_path.display()));
    eprintln!("Markdown written to {}", md_path.display());

    // Print summary to stdout
    for (category, count) in &summary.finding_counts {
        eprintln!("  {category}: {count}");
    }
}

/// Pretty-print the WotC rulings for a card. Useful during parser authoring
/// to verify parsed AbilityDefinitions don't contradict official rulings.
///
/// Usage: `cargo run --bin oracle-gen -- rulings "<card name>"`
fn run_rulings(remaining_args: &[String]) {
    let Some(card_name) = remaining_args.first() else {
        eprintln!("Usage: oracle-gen rulings <card name> [data-dir]");
        process::exit(1);
    };

    let card_data_path = if let Some(dir) = remaining_args.get(1) {
        PathBuf::from(dir).join("card-data.json")
    } else {
        std::env::var("PHASE_CARDS_PATH")
            .map(|p| PathBuf::from(p).join("card-data.json"))
            .or_else(|_| {
                std::env::var("PHASE_DATA_DIR").map(|d| PathBuf::from(d).join("card-data.json"))
            })
            .unwrap_or_else(|_| PathBuf::from("client/public/card-data.json"))
    };

    if !card_data_path.exists() {
        eprintln!(
            "Error: card-data.json not found at {}",
            card_data_path.display()
        );
        eprintln!("Run ./scripts/gen-card-data.sh first, or pass a data directory.");
        process::exit(1);
    }

    let card_db = match CardDatabase::from_export(&card_data_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database: {e}");
            process::exit(1);
        }
    };

    let rulings = card_db.rulings_for(card_name);
    if rulings.is_empty() {
        eprintln!("No rulings found for '{card_name}'.");
        eprintln!("(Note: rulings are attached to the front face of multi-face cards.)");
        return;
    }

    println!("Rulings for {card_name}:");
    for ruling in rulings {
        println!("  [{}] {}", ruling.date, ruling.text);
    }
}

/// Top-level wrapper for MTGJSON's `SetList.json` file.
#[derive(Deserialize)]
struct SetListFile {
    data: Vec<SetListRawEntry>,
}

/// Raw SetList entry — only the fields we forward to the frontend.
/// Fields we ignore (decks, sealedProduct, translations, keyruneCode, languages,
/// mcm/mtgo/tcgplayer metadata, isFoilOnly, totalSetSize, block) would bloat the
/// sidecar by ~10x with no current consumer.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetListRawEntry {
    code: String,
    name: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default, rename = "type")]
    set_type: Option<String>,
    #[serde(default)]
    is_online_only: bool,
    #[serde(default)]
    base_set_size: Option<u32>,
    #[serde(default)]
    parent_code: Option<String>,
}

/// Projected SetList entry written to `client/public/set-list.json`. Keys are
/// camelCase so the frontend can use them verbatim without renaming.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SetListEntry {
    code: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_date: Option<String>,
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")]
    set_type: Option<String>,
    is_online_only: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_set_size: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_code: Option<String>,
}

/// Project MTGJSON's `SetList.json` down to the fields the frontend actually
/// needs (~10% of the source file's size). Input is `<data-dir>/mtgjson/SetList.json`
/// by default; output goes to `<data-dir>/set-list.json` (or stdout if no data dir).
///
/// Usage: `cargo run --bin oracle-gen -- set-list <data-dir> [output-path]`
fn run_set_list(remaining_args: &[String]) {
    let Some(data_dir) = remaining_args.first() else {
        eprintln!("Usage: oracle-gen set-list <data-dir> [output-path]");
        process::exit(1);
    };
    let input = PathBuf::from(data_dir).join("mtgjson").join("SetList.json");
    let output = remaining_args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("client/public/set-list.json"));

    if !input.exists() {
        eprintln!(
            "Error: SetList.json not found at {}. Run ./scripts/gen-card-data.sh first.",
            input.display()
        );
        process::exit(1);
    }

    let contents = std::fs::read_to_string(&input)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", input.display()));
    let raw: SetListFile = serde_json::from_str(&contents)
        .unwrap_or_else(|e| panic!("Failed to parse {}: {e}", input.display()));

    let projected: BTreeMap<String, SetListEntry> = raw
        .data
        .into_iter()
        .map(|s| {
            (
                s.code.clone(),
                SetListEntry {
                    code: s.code,
                    name: s.name,
                    release_date: s.release_date,
                    set_type: s.set_type,
                    is_online_only: s.is_online_only,
                    base_set_size: s.base_set_size,
                    parent_code: s.parent_code,
                },
            )
        })
        .collect();

    let json = serde_json::to_string(&projected).expect("SetListEntry serialization cannot fail");
    std::fs::write(&output, &json)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", output.display()));
    eprintln!(
        "Projected {} sets to {} ({} bytes)",
        projected.len(),
        output.display(),
        json.len()
    );
}

/// MTGJSON per-deck file wrapper: `{ "meta": {...}, "data": { ... } }`.
#[derive(Deserialize)]
struct DeckFile {
    data: DeckRaw,
}

/// Raw deck payload. MTGJSON deck entries carry ~200 fields per card
/// (localized names, purchase URLs, printings, identifiers); we only need
/// name + count, so everything else is dropped via `#[serde(default)]` +
/// ignoring unknown fields.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeckRaw {
    code: String,
    name: String,
    #[serde(rename = "type")]
    deck_type: String,
    #[serde(default)]
    release_date: Option<String>,
    #[serde(default)]
    main_board: Vec<DeckCardRaw>,
    #[serde(default)]
    side_board: Vec<DeckCardRaw>,
    #[serde(default)]
    commander: Vec<DeckCardRaw>,
}

#[derive(Deserialize)]
struct DeckCardRaw {
    name: String,
    #[serde(default = "default_count")]
    count: u32,
}

fn default_count() -> u32 {
    1
}

/// Projected deck entry written to `client/public/decks.json`.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeckEntry {
    code: String,
    name: String,
    #[serde(rename = "type")]
    deck_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_date: Option<String>,
    main_board: Vec<DeckCardEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    side_board: Vec<DeckCardEntry>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    commander: Vec<DeckCardEntry>,
}

#[derive(Serialize)]
struct DeckCardEntry {
    name: String,
    count: u32,
}

/// Debug sidecar entry describing a deck that was filtered out.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SkippedDeck {
    code: String,
    name: String,
    #[serde(rename = "type")]
    deck_type: String,
    unsupported: Vec<String>,
}

fn project_deck_cards(raw: &[DeckCardRaw]) -> Vec<DeckCardEntry> {
    raw.iter()
        .map(|c| DeckCardEntry {
            name: c.name.clone(),
            count: c.count,
        })
        .collect()
}

/// Total physical cards in main + commander (the "is this actually a deck?"
/// yardstick). Sideboard is excluded because many non-decks (Secret Lair,
/// Jumpstart, sample product) list their whole content under mainBoard.
fn deck_card_total(raw: &DeckRaw) -> u32 {
    raw.main_board.iter().map(|c| c.count).sum::<u32>()
        + raw.commander.iter().map(|c| c.count).sum::<u32>()
}

/// Minimum card count to qualify as a "deck" (MTG Limited-format minimum,
/// CR 100.2a). Anything smaller is a product — Secret Lair Drops, half-
/// jumpstart packs, welcome boosters, sample decks, toolkits, etc. MTGJSON
/// ships all of these in AllDeckFiles and distinguishing them by `type`
/// alone is brittle (every new product line invents a new type string).
const MIN_DECK_CARDS: u32 = 40;

/// Collect the names of cards in a deck that aren't playable in the engine.
/// Dedups while preserving first-seen order so the skipped sidecar is readable.
fn unsupported_cards(raw: &DeckRaw, db: &CardDatabase) -> Vec<String> {
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out: Vec<String> = Vec::new();
    let sections = [&raw.main_board, &raw.side_board, &raw.commander];
    for section in sections {
        for card in section.iter() {
            if seen.insert(card.name.to_lowercase())
                && !engine::database::is_card_playable(db, &card.name)
            {
                out.push(card.name.clone());
            }
        }
    }
    out
}

/// Ingest MTGJSON's `AllDeckFiles` (one JSON per deck extracted under
/// `<data-dir>/mtgjson/decks/`), filter to decks whose every card is playable
/// by the engine, and project to a flat map keyed by deck code.
///
/// Usage: `cargo run --bin oracle-gen -- decks <data-dir> [output-path] [--emit-skipped]`
fn run_decks(remaining_args: &[String]) {
    let mut positional: Vec<&String> = Vec::new();
    let mut emit_skipped = false;
    for arg in remaining_args {
        if arg == "--emit-skipped" {
            emit_skipped = true;
        } else {
            positional.push(arg);
        }
    }

    let Some(data_dir) = positional.first() else {
        eprintln!("Usage: oracle-gen decks <data-dir> [output-path] [--emit-skipped]");
        process::exit(1);
    };
    let decks_dir = PathBuf::from(data_dir).join("mtgjson").join("decks");
    let output = positional
        .get(1)
        .map(|s| PathBuf::from(s.as_str()))
        .unwrap_or_else(|| PathBuf::from("client/public/decks.json"));

    if !decks_dir.is_dir() {
        eprintln!(
            "Error: decks directory not found at {}. Extract AllDeckFiles.tar.gz first.",
            decks_dir.display()
        );
        process::exit(1);
    }

    let card_data_path = PathBuf::from(data_dir).join("../client/public/card-data.json");
    let card_data_path = if card_data_path.exists() {
        card_data_path
    } else {
        PathBuf::from("client/public/card-data.json")
    };
    if !card_data_path.exists() {
        eprintln!(
            "Error: card-data.json not found at {}. Run oracle-gen card export first.",
            card_data_path.display()
        );
        process::exit(1);
    }

    let card_db = match CardDatabase::from_export(&card_data_path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database: {e}");
            process::exit(1);
        }
    };

    let mut included: BTreeMap<String, DeckEntry> = BTreeMap::new();
    let mut skipped: Vec<SkippedDeck> = Vec::new();
    let mut too_small: u32 = 0;
    let mut read_errors: u32 = 0;

    let entries = std::fs::read_dir(&decks_dir)
        .unwrap_or_else(|e| panic!("Failed to read {}: {e}", decks_dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let contents = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => {
                read_errors += 1;
                continue;
            }
        };
        let parsed: DeckFile = match serde_json::from_str(&contents) {
            Ok(d) => d,
            Err(_) => {
                read_errors += 1;
                continue;
            }
        };
        let deck_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();
        let raw = parsed.data;
        if deck_card_total(&raw) < MIN_DECK_CARDS {
            too_small += 1;
            continue;
        }
        let unsupported = unsupported_cards(&raw, &card_db);
        if unsupported.is_empty() {
            included.insert(
                deck_id,
                DeckEntry {
                    code: raw.code,
                    name: raw.name,
                    deck_type: raw.deck_type,
                    release_date: raw.release_date,
                    main_board: project_deck_cards(&raw.main_board),
                    side_board: project_deck_cards(&raw.side_board),
                    commander: project_deck_cards(&raw.commander),
                },
            );
        } else {
            skipped.push(SkippedDeck {
                code: raw.code,
                name: raw.name,
                deck_type: raw.deck_type,
                unsupported,
            });
        }
    }

    let json = serde_json::to_string(&included).expect("DeckEntry serialization cannot fail");
    std::fs::write(&output, &json)
        .unwrap_or_else(|e| panic!("Failed to write {}: {e}", output.display()));
    eprintln!(
        "Wrote {} decks to {} ({} bytes; {} skipped for unsupported cards, {} dropped as non-decks (<{} cards), {} read errors)",
        included.len(),
        output.display(),
        json.len(),
        skipped.len(),
        too_small,
        MIN_DECK_CARDS,
        read_errors
    );

    if emit_skipped {
        let skipped_path = output
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("decks-skipped.json");
        skipped.sort_by(|a, b| {
            a.unsupported
                .len()
                .cmp(&b.unsupported.len())
                .then_with(|| a.code.cmp(&b.code))
        });
        let skipped_json =
            serde_json::to_string_pretty(&skipped).expect("SkippedDeck serialization cannot fail");
        std::fs::write(&skipped_path, &skipped_json)
            .unwrap_or_else(|e| panic!("Failed to write {}: {e}", skipped_path.display()));
        eprintln!("Wrote skipped-deck sidecar to {}", skipped_path.display());
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::OnceLock;

    use engine::database::mtgjson::{load_atomic_cards, AtomicCardsFile};
    use engine::types::ability::TargetFilter;
    use engine::types::keywords::Keyword;

    use super::*;

    fn load_atomic_fixture() -> &'static AtomicCardsFile {
        static ATOMIC: OnceLock<AtomicCardsFile> = OnceLock::new();
        ATOMIC.get_or_init(|| {
            let path =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/mtgjson/AtomicCards.json");
            load_atomic_cards(&path).expect("AtomicCards.json should load")
        })
    }

    #[test]
    fn export_layout_keeps_aang_front_face_keywords_face_local() {
        let atomic = load_atomic_fixture();
        let faces = atomic
            .data
            .get("Aang, Swift Savior // Aang and La, Ocean's Fury")
            .expect("Aang faces should exist");
        let oracle_id = faces[0].identifiers.scryfall_oracle_id.clone();
        let layout = build_export_layout(faces, oracle_id, map_layout(&faces[0].layout));
        let layout_face_refs = layout_faces(&layout);
        let front = layout_face_refs
            .iter()
            .find(|face| face.name == "Aang, Swift Savior")
            .expect("front face should exist");

        assert!(front.keywords.contains(&Keyword::Flash));
        assert!(front.keywords.contains(&Keyword::Flying));
        assert!(!front.keywords.contains(&Keyword::Reach));
        assert!(!front.keywords.contains(&Keyword::Trample));
    }

    #[test]
    fn export_layout_keeps_floodpits_etb_counter_on_parent_target() {
        let atomic = load_atomic_fixture();
        let faces = atomic
            .data
            .get("Floodpits Drowner")
            .expect("Floodpits should exist");
        let oracle_id = faces[0].identifiers.scryfall_oracle_id.clone();
        let layout = build_export_layout(faces, oracle_id, map_layout(&faces[0].layout));
        let face = match layout {
            CardLayout::Single(face) => face,
            other => panic!("expected single-face layout, got {other:?}"),
        };
        let trigger = face.triggers.first().expect("ETB trigger should exist");
        let sub = trigger
            .execute
            .as_ref()
            .and_then(|ability| ability.sub_ability.as_ref())
            .expect("ETB should chain into PutCounter");

        match &*sub.effect {
            engine::types::ability::Effect::PutCounter { target, .. } => {
                assert!(matches!(target, TargetFilter::ParentTarget));
            }
            other => panic!("expected PutCounter sub-ability, got {other:?}"),
        }
    }
}
