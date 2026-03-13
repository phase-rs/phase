use std::collections::HashMap;
use std::path::PathBuf;
use std::process;
use std::str::FromStr;

use serde::Serialize;

use engine::database::legality::{legalities_to_export_map, normalize_legalities};
use engine::database::mtgjson::{load_atomic_cards, parse_mtgjson_mana_cost, AtomicCard};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, ManaProduction, PtValue,
    TargetFilter, TypeFilter,
};
use engine::types::card::{CardFace, CardLayout};
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};

#[derive(Debug, Clone, Serialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: HashMap<String, String>,
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

    let mut face_index: HashMap<String, CardExportEntry> = HashMap::new();
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
            let mut legalities_by_face = HashMap::new();
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
                    .any(|f| face_has_unimplemented(f));
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

            if stats && face_has_unimplemented(&face) {
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
    };

    synthesize_basic_land_mana(&mut face);
    synthesize_equip(&mut face);
    face
}

fn face_has_unimplemented(face: &CardFace) -> bool {
    face.abilities
        .iter()
        .any(|a| matches!(&a.effect, Effect::Unimplemented { .. }))
        || face.triggers.iter().any(|t| {
            t.execute
                .as_ref()
                .is_some_and(|a| matches!(&a.effect, Effect::Unimplemented { .. }))
        })
}

// ---------------------------------------------------------------------------
// Helpers copied from json_loader.rs (module-private, will be deleted in Task 13)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LayoutKind {
    Single,
    Split,
    Flip,
    Transform,
    Meld,
    Adventure,
    Modal,
}

fn map_layout(layout_str: &str) -> LayoutKind {
    match layout_str {
        "normal" | "saga" | "class" | "case" | "leveler" => LayoutKind::Single,
        "split" => LayoutKind::Split,
        "flip" => LayoutKind::Flip,
        "transform" => LayoutKind::Transform,
        "meld" => LayoutKind::Meld,
        "adventure" => LayoutKind::Adventure,
        "modal_dfc" => LayoutKind::Modal,
        _ => LayoutKind::Single,
    }
}

fn build_card_type(mtgjson: &AtomicCard) -> CardType {
    let supertypes = mtgjson
        .supertypes
        .iter()
        .filter_map(|s| Supertype::from_str(s).ok())
        .collect();
    let core_types = mtgjson
        .types
        .iter()
        .filter_map(|s| CoreType::from_str(s).ok())
        .collect();
    let subtypes = mtgjson.subtypes.clone();
    CardType {
        supertypes,
        core_types,
        subtypes,
    }
}

fn map_mtgjson_color(code: &str) -> Option<ManaColor> {
    match code {
        "W" => Some(ManaColor::White),
        "U" => Some(ManaColor::Blue),
        "B" => Some(ManaColor::Black),
        "R" => Some(ManaColor::Red),
        "G" => Some(ManaColor::Green),
        _ => None,
    }
}

fn parse_pt_value(s: &str) -> PtValue {
    match s.parse::<i32>() {
        Ok(n) => PtValue::Fixed(n),
        Err(_) => PtValue::Variable(s.to_string()),
    }
}

fn synthesize_basic_land_mana(face: &mut CardFace) {
    let land_mana: Vec<(&str, ManaColor)> = vec![
        ("Plains", ManaColor::White),
        ("Island", ManaColor::Blue),
        ("Swamp", ManaColor::Black),
        ("Mountain", ManaColor::Red),
        ("Forest", ManaColor::Green),
    ];

    for (subtype, color) in land_mana {
        if face.card_type.subtypes.iter().any(|s| s == subtype) {
            face.abilities.push(AbilityDefinition {
                kind: AbilityKind::Activated,
                effect: Effect::Mana {
                    produced: ManaProduction::Fixed {
                        colors: vec![color],
                    },
                },
                cost: Some(AbilityCost::Tap),
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
                condition: None,
            });
        }
    }
}

fn synthesize_equip(face: &mut CardFace) {
    let equip_abilities: Vec<AbilityDefinition> = face
        .keywords
        .iter()
        .filter_map(|kw| {
            if let Keyword::Equip(cost) = kw {
                Some(AbilityDefinition {
                    kind: AbilityKind::Activated,
                    effect: Effect::Attach {
                        target: TargetFilter::Typed {
                            card_type: Some(TypeFilter::Creature),
                            subtype: None,
                            controller: Some(ControllerRef::You),
                            properties: vec![],
                        },
                    },
                    cost: Some(AbilityCost::Mana { cost: cost.clone() }),
                    sub_ability: None,
                    duration: None,
                    description: None,
                    target_prompt: None,
                    sorcery_speed: false,
                    condition: None,
                })
            } else {
                None
            }
        })
        .collect();

    face.abilities.extend(equip_abilities);
}

fn layout_faces(layout: &CardLayout) -> Vec<&CardFace> {
    match layout {
        CardLayout::Single(face) => vec![face],
        CardLayout::Split(a, b)
        | CardLayout::Flip(a, b)
        | CardLayout::Transform(a, b)
        | CardLayout::Meld(a, b)
        | CardLayout::Adventure(a, b)
        | CardLayout::Modal(a, b)
        | CardLayout::Omen(a, b) => vec![a, b],
        CardLayout::Specialize(base, variants) => {
            let mut faces = vec![base];
            faces.extend(variants);
            faces
        }
    }
}

// ---------------------------------------------------------------------------
// Copied from game/deck_loading.rs (pub(crate), not accessible from binaries)
// ---------------------------------------------------------------------------

fn shard_colors(shard: &ManaCostShard) -> Vec<ManaColor> {
    match shard {
        ManaCostShard::White | ManaCostShard::TwoWhite | ManaCostShard::PhyrexianWhite => {
            vec![ManaColor::White]
        }
        ManaCostShard::Blue | ManaCostShard::TwoBlue | ManaCostShard::PhyrexianBlue => {
            vec![ManaColor::Blue]
        }
        ManaCostShard::Black | ManaCostShard::TwoBlack | ManaCostShard::PhyrexianBlack => {
            vec![ManaColor::Black]
        }
        ManaCostShard::Red | ManaCostShard::TwoRed | ManaCostShard::PhyrexianRed => {
            vec![ManaColor::Red]
        }
        ManaCostShard::Green | ManaCostShard::TwoGreen | ManaCostShard::PhyrexianGreen => {
            vec![ManaColor::Green]
        }
        ManaCostShard::WhiteBlue | ManaCostShard::PhyrexianWhiteBlue => {
            vec![ManaColor::White, ManaColor::Blue]
        }
        ManaCostShard::WhiteBlack | ManaCostShard::PhyrexianWhiteBlack => {
            vec![ManaColor::White, ManaColor::Black]
        }
        ManaCostShard::BlueBlack | ManaCostShard::PhyrexianBlueBlack => {
            vec![ManaColor::Blue, ManaColor::Black]
        }
        ManaCostShard::BlueRed | ManaCostShard::PhyrexianBlueRed => {
            vec![ManaColor::Blue, ManaColor::Red]
        }
        ManaCostShard::BlackRed | ManaCostShard::PhyrexianBlackRed => {
            vec![ManaColor::Black, ManaColor::Red]
        }
        ManaCostShard::BlackGreen | ManaCostShard::PhyrexianBlackGreen => {
            vec![ManaColor::Black, ManaColor::Green]
        }
        ManaCostShard::RedWhite | ManaCostShard::PhyrexianRedWhite => {
            vec![ManaColor::Red, ManaColor::White]
        }
        ManaCostShard::RedGreen | ManaCostShard::PhyrexianRedGreen => {
            vec![ManaColor::Red, ManaColor::Green]
        }
        ManaCostShard::GreenWhite | ManaCostShard::PhyrexianGreenWhite => {
            vec![ManaColor::Green, ManaColor::White]
        }
        ManaCostShard::GreenBlue | ManaCostShard::PhyrexianGreenBlue => {
            vec![ManaColor::Green, ManaColor::Blue]
        }
        ManaCostShard::ColorlessWhite => vec![ManaColor::White],
        ManaCostShard::ColorlessBlue => vec![ManaColor::Blue],
        ManaCostShard::ColorlessBlack => vec![ManaColor::Black],
        ManaCostShard::ColorlessRed => vec![ManaColor::Red],
        ManaCostShard::ColorlessGreen => vec![ManaColor::Green],
        ManaCostShard::Colorless | ManaCostShard::Snow | ManaCostShard::X => vec![],
    }
}

fn derive_colors_from_mana_cost(mana_cost: &ManaCost) -> Vec<ManaColor> {
    match mana_cost {
        ManaCost::NoCost => vec![],
        ManaCost::Cost { shards, .. } => {
            let mut colors = Vec::new();
            for shard in shards {
                for color in shard_colors(shard) {
                    if !colors.contains(&color) {
                        colors.push(color);
                    }
                }
            }
            colors
        }
    }
}
