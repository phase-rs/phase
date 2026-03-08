use std::collections::HashMap;
use std::path::PathBuf;
use std::process;

use engine::database::CardDatabase;
use engine::types::card::{CardFace, CardLayout};

fn primary_face(rules: &engine::types::card::CardRules) -> &CardFace {
    match &rules.layout {
        CardLayout::Single(face)
        | CardLayout::Split(face, _)
        | CardLayout::Flip(face, _)
        | CardLayout::Transform(face, _)
        | CardLayout::Meld(face, _)
        | CardLayout::Adventure(face, _)
        | CardLayout::Modal(face, _)
        | CardLayout::Omen(face, _)
        | CardLayout::Specialize(face, _) => face,
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("FORGE_CARDS_PATH").ok())
        .map(PathBuf::from);

    let Some(path) = path else {
        eprintln!("Usage: card-data-export <card-data-dir>");
        eprintln!("  Or set FORGE_CARDS_PATH environment variable");
        eprintln!();
        eprintln!("Outputting empty card map to stdout.");
        let empty: HashMap<String, &CardFace> = HashMap::new();
        println!("{}", serde_json::to_string_pretty(&empty).unwrap());
        process::exit(0);
    };

    let db = match CardDatabase::load(&path) {
        Ok(db) => db,
        Err(e) => {
            eprintln!("Error loading card database from {}: {}", path.display(), e);
            process::exit(1);
        }
    };

    let mut card_map: HashMap<String, &CardFace> = HashMap::new();
    for (_key, rules) in db.iter() {
        let face = primary_face(rules);
        card_map.insert(face.name.clone(), face);
    }

    println!("{}", serde_json::to_string(&card_map).unwrap());
}
