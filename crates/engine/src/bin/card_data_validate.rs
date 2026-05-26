//! Deploy-time gate: parse `card-data.json` through the same `CardDatabase::from_export`
//! path the WASM bundle uses at runtime, and exit non-zero if the schema rejects it.
//!
//! Catches the "this commit's engine cannot read this commit's card-data" class of bug
//! before the file ships to R2. Pairs with the content-addressed `card-data-<hash>.json`
//! pipeline in `scripts/gen-card-data.sh` — together they make WASM/card-data drift
//! across deployments structurally impossible.

use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;

fn main() {
    let path: PathBuf = match std::env::args().nth(1) {
        Some(p) => PathBuf::from(p),
        None => {
            eprintln!("Usage: card-data-validate <path-to-card-data.json>");
            process::exit(2);
        }
    };

    match CardDatabase::from_export(&path) {
        Ok(db) => {
            let count = db.card_count();
            if count == 0 {
                eprintln!("FAIL: card database parsed but contained 0 cards");
                process::exit(1);
            }
            let integrity_errors = db.export_integrity_errors();
            if !integrity_errors.is_empty() {
                eprintln!("FAIL: card database parsed but failed export integrity checks");
                for error in integrity_errors.iter().take(20) {
                    eprintln!("      {error}");
                }
                if integrity_errors.len() > 20 {
                    eprintln!("      ... and {} more", integrity_errors.len() - 20);
                }
                process::exit(1);
            }
            println!("OK: {} cards parsed from {}", count, path.display());
        }
        Err(e) => {
            eprintln!(
                "FAIL: {} could not be parsed by current engine schema",
                path.display()
            );
            eprintln!("      {e}");
            process::exit(1);
        }
    }
}
