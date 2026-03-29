use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process;

use serde::Serialize;

use engine::database::legality::{legalities_to_export_map, normalize_legalities};
use engine::database::mtgjson::{load_atomic_cards, AtomicCard};
use engine::database::synthesis::{
    build_oracle_face, build_oracle_face_multi, layout_faces, map_layout, LayoutKind,
};
use engine::game::coverage::card_face_has_unimplemented_parts;
use engine::types::card::{CardFace, CardLayout};

#[derive(Debug, Clone, Serialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: BTreeMap<String, String>,
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

    // Build Forge index if --forge flag or PHASE_FORGE_PATH env var is set.
    #[cfg(feature = "forge")]
    let forge_index = {
        let path = forge_path.or_else(|| std::env::var("PHASE_FORGE_PATH").ok().map(PathBuf::from));
        match path {
            Some(p) if p.exists() => {
                eprintln!("Building Forge index from: {}", p.display());
                let idx = engine::database::forge::ForgeIndex::scan(&p);
                eprintln!("Forge index: {} face names", idx.len());
                Some(idx)
            }
            Some(p) => {
                eprintln!("Warning: Forge path {} not found, skipping", p.display());
                None
            }
            None => None,
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

            for face_ref in layout_faces(&layout) {
                let key = face_ref.name.to_lowercase();
                let legalities = legalities_by_face.remove(&key).unwrap_or_default();
                let mut face = face_ref.clone();
                #[cfg(feature = "forge")]
                if let Some(ref fi) = forge_index {
                    engine::database::forge::apply_forge_fallback(&mut face, fi);
                }
                face_index.insert(key, CardExportEntry { face, legalities });
            }
        } else {
            let mut face = build_oracle_face(&faces[0], oracle_id);
            #[cfg(feature = "forge")]
            if let Some(ref fi) = forge_index {
                engine::database::forge::apply_forge_fallback(&mut face, fi);
            }
            let key = face.name.to_lowercase();
            let legalities = legalities_to_export_map(&normalize_legalities(&faces[0].legalities));

            if stats && card_face_has_unimplemented_parts(&face) {
                cards_with_unimplemented += 1;
            }

            face_index.insert(key, CardExportEntry { face, legalities });
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
