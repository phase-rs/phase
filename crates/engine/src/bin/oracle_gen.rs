use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process;

use serde::Serialize;

use engine::database::legality::{legalities_to_export_map, normalize_legalities};
use engine::database::mtgjson::{load_atomic_cards, parse_mtgjson_mana_cost, AtomicCard};
use engine::database::synthesis::{
    build_card_type, layout_faces, map_layout, map_mtgjson_color, parse_pt_value, synthesize_all,
    LayoutKind,
};
use engine::game::coverage::card_face_has_unimplemented_parts;
use engine::game::printed_cards::derive_colors_from_mana_cost;
use engine::parser::oracle::parse_oracle_text;
use engine::types::card::{CardFace, CardLayout};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;

#[derive(Debug, Clone, Serialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: BTreeMap<String, String>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    let mut data_dir: Option<PathBuf> = None;
    let mut mtgjson_override: Option<PathBuf> = None;
    let mut names_out: Option<PathBuf> = None;
    let mut stats = false;

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

    let mut face_index: BTreeMap<String, CardExportEntry> = BTreeMap::new();
    let mut total_cards = 0u32;
    let mut cards_with_unimplemented = 0u32;

    for faces in atomic.data.values() {
        total_cards += 1;

        let oracle_id = faces
            .first()
            .and_then(|f| f.identifiers.scryfall_oracle_id.clone());

        let layout_kind = map_layout(&faces[0].layout);

        if faces.len() >= 2 {
            let face_a = build_oracle_face(&faces[0], oracle_id.clone());
            let face_b = build_oracle_face(&faces[1], oracle_id);
            let mut legalities_by_face = BTreeMap::new();
            legalities_by_face.insert(
                face_a.name.to_lowercase(),
                legalities_to_export_map(&normalize_legalities(&faces[0].legalities)),
            );
            legalities_by_face.insert(
                face_b.name.to_lowercase(),
                legalities_to_export_map(&normalize_legalities(&faces[1].legalities)),
            );
            let layout = match layout_kind {
                LayoutKind::Split => CardLayout::Split(face_a, face_b),
                LayoutKind::Flip => CardLayout::Flip(face_a, face_b),
                LayoutKind::Transform => CardLayout::Transform(face_a, face_b),
                LayoutKind::Meld => CardLayout::Meld(face_a, face_b),
                LayoutKind::Adventure => CardLayout::Adventure(face_a, face_b),
                LayoutKind::Modal => CardLayout::Modal(face_a, face_b),
                LayoutKind::Single => CardLayout::Single(face_a),
            };

            if stats {
                let has_unimplemented = layout_faces(&layout)
                    .iter()
                    .any(|f| card_face_has_unimplemented_parts(f));
                if has_unimplemented {
                    cards_with_unimplemented += 1;
                }
            }

            for face in layout_faces(&layout) {
                let key = face.name.to_lowercase();
                let legalities = legalities_by_face.remove(&key).unwrap_or_default();
                face_index.insert(
                    key,
                    CardExportEntry {
                        face: face.clone(),
                        legalities,
                    },
                );
            }
        } else {
            let face = build_oracle_face(&faces[0], oracle_id);
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

fn build_oracle_face(mtgjson: &AtomicCard, oracle_id: Option<String>) -> CardFace {
    let card_type = build_card_type(mtgjson);
    let mtgjson_keyword_names: Vec<String> = mtgjson
        .keywords
        .as_ref()
        .map(|kws| kws.iter().map(|s| s.to_ascii_lowercase()).collect())
        .unwrap_or_default();

    let mut keywords: Vec<Keyword> = mtgjson
        .keywords
        .as_ref()
        .map(|kws| {
            kws.iter()
                .map(|s| s.parse::<Keyword>().unwrap())
                .filter(|k| !matches!(k, Keyword::Unknown(_)))
                .collect()
        })
        .unwrap_or_default();

    let oracle_text = mtgjson.text.as_deref().unwrap_or("");
    let face_name = mtgjson.face_name.as_deref().unwrap_or(&mtgjson.name);

    let types: Vec<String> = mtgjson.types.clone();
    let subtypes: Vec<String> = mtgjson.subtypes.clone();

    let parsed = parse_oracle_text(
        oracle_text,
        face_name,
        &mtgjson_keyword_names,
        &types,
        &subtypes,
    );

    // Merge keywords extracted from Oracle text with MTGJSON keywords
    keywords.extend(parsed.extracted_keywords);

    let mana_cost = mtgjson
        .mana_cost
        .as_deref()
        .map(parse_mtgjson_mana_cost)
        .unwrap_or_default();

    let mana_derived_colors = derive_colors_from_mana_cost(&mana_cost);
    let mtgjson_colors: Vec<ManaColor> = mtgjson
        .colors
        .iter()
        .filter_map(|c| map_mtgjson_color(c))
        .collect();
    let color_override = if mtgjson_colors != mana_derived_colors {
        Some(mtgjson_colors)
    } else {
        None
    };

    let mut face = CardFace {
        name: face_name.to_string(),
        mana_cost,
        card_type,
        power: mtgjson.power.as_ref().map(|s| parse_pt_value(s)),
        toughness: mtgjson.toughness.as_ref().map(|s| parse_pt_value(s)),
        loyalty: mtgjson.loyalty.clone(),
        defense: mtgjson.defense.clone(),
        oracle_text: mtgjson.text.clone(),
        non_ability_text: None,
        flavor_name: None,
        keywords,
        abilities: parsed.abilities,
        triggers: parsed.triggers,
        static_abilities: parsed.statics,
        replacements: parsed.replacements,
        color_override,
        scryfall_oracle_id: oracle_id,
        modal: parsed.modal,
        additional_cost: parsed.additional_cost,
        casting_restrictions: parsed.casting_restrictions,
        casting_options: parsed.casting_options,
        solve_condition: parsed.solve_condition,
    };

    synthesize_all(&mut face);
    face
}
