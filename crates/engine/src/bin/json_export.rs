use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;

fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("PHASE_DATA_DIR").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        eprintln!("Usage: json-export <data-dir>");
        eprintln!("  Loads MTGJSON + ability JSON and exports card-data.json");
        process::exit(1);
    };

    let mtgjson_path = path.join("mtgjson/AtomicCards.json");
    let abilities_dir = path.join("abilities");

    if !mtgjson_path.exists() {
        eprintln!("Error: {} not found", mtgjson_path.display());
        eprintln!("  Download with: curl -o data/mtgjson/AtomicCards.json https://mtgjson.com/api/v5/AtomicCards.json");
        process::exit(1);
    }

    let db = match CardDatabase::load_json(&mtgjson_path, &abilities_dir) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database: {e}");
            process::exit(1);
        }
    };

    eprintln!("Loaded {} cards", db.card_count());
    if !db.errors().is_empty() {
        eprintln!("{} errors:", db.errors().len());
        for (path, err) in db.errors().iter().take(5) {
            eprintln!("  {:?}: {err}", path);
        }
    }

    // Export face_index as HashMap<String, CardFace> JSON
    // Access face_index via public iterator
    let faces: std::collections::HashMap<String, &engine::types::card::CardFace> = db
        .face_iter()
        .map(|(name, face)| (name.to_string(), face))
        .collect();

    println!(
        "{}",
        serde_json::to_string(&faces).expect("Failed to serialize card data")
    );
}
