use std::path::PathBuf;
use std::process;

use draft_core::extraction::extract_all_set_pools;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let sets_dir = args
        .get(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/mtgjson/sets"));

    let output_path = args
        .get(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("client/public/draft-pools.json"));

    if !sets_dir.exists() {
        eprintln!(
            "Error: Sets directory not found at {}.\n\
             Run ./scripts/fetch-draft-sets.sh first to download MTGJSON set data.",
            sets_dir.display()
        );
        process::exit(1);
    }

    let pools = match extract_all_set_pools(&sets_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Error extracting set pools: {e}");
            process::exit(1);
        }
    };

    if pools.is_empty() {
        eprintln!(
            "Warning: no draftable sets found in {}. \
             Ensure the directory contains MTGJSON per-set JSON files.",
            sets_dir.display()
        );
        process::exit(1);
    }

    let json = match serde_json::to_string_pretty(&pools) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("Error serializing draft pools: {e}");
            process::exit(1);
        }
    };

    if let Some(parent) = output_path.parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                eprintln!("Error creating output directory: {e}");
                process::exit(1);
            }
        }
    }

    if let Err(e) = std::fs::write(&output_path, &json) {
        eprintln!("Error writing {}: {e}", output_path.display());
        process::exit(1);
    }

    eprintln!(
        "Extracted {} sets, wrote {} ({} bytes)",
        pools.len(),
        output_path.display(),
        json.len()
    );
}
