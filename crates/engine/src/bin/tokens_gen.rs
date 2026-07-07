//! Generate the engine token catalog from MTGJSON per-set token data.
//!
//! Usage:
//!     cargo run --bin tokens-gen -- \
//!         --input data/mtgjson/sets \
//!         --output crates/engine/data/known-tokens.toml

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::str::FromStr;

use engine::database::mtgjson::{SetFile, SetToken};
use engine::game::token_presets::{
    PredefinedTokenKind, PresetFidelity, TokenCategory, TokenPreset, TokenPtProvenance,
    TokenSourceRef,
};
use engine::types::card::TokenImageRef;
use engine::types::card_type::{CoreType, Supertype};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::proposed_event::TokenCharacteristics;
use serde::Serialize;

#[derive(Serialize)]
struct CatalogFile {
    token: Vec<TokenPreset>,
}

#[derive(Default, Clone)]
struct SourceCardIndex {
    names_by_token_id: HashMap<String, BTreeSet<String>>,
    refs_by_token_id: HashMap<String, BTreeMap<String, TokenSourceRef>>,
}

fn main() -> ExitCode {
    let mut input = PathBuf::from("data/mtgjson/sets");
    let mut output = PathBuf::from("crates/engine/data/known-tokens.toml");

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => {
                let Some(value) = args.next() else {
                    eprintln!("--input requires a path");
                    return ExitCode::FAILURE;
                };
                input = PathBuf::from(value);
            }
            "--output" => {
                let Some(value) = args.next() else {
                    eprintln!("--output requires a path");
                    return ExitCode::FAILURE;
                };
                output = PathBuf::from(value);
            }
            other => {
                eprintln!("unknown argument: {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    match generate(&input, &output) {
        Ok(count) => {
            eprintln!("Generated {} token presets at {}", count, output.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("tokens-gen: {err}");
            ExitCode::FAILURE
        }
    }
}

fn generate(input: &PathBuf, output: &PathBuf) -> Result<usize, String> {
    let mut set_files = Vec::new();
    for entry in fs::read_dir(input).map_err(|e| format!("read {}: {e}", input.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        let parsed: SetFile =
            serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", path.display()))?;
        set_files.push(parsed);
    }

    let source_index = build_source_index(&set_files);
    let mut presets = Vec::new();
    let mut seen = BTreeSet::new();

    for set_file in &set_files {
        for token in &set_file.data.tokens {
            if !seen.insert(token.uuid.clone()) {
                continue;
            }
            if let Some(preset) = build_preset(token, set_file, &source_index)? {
                presets.push(preset);
            }
        }
    }

    presets.sort_by(|a, b| a.id.cmp(&b.id));
    let toml = toml::to_string_pretty(&CatalogFile {
        token: presets.clone(),
    })
    .map_err(|e| format!("serialize toml: {e}"))?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    fs::write(output, toml).map_err(|e| format!("write {}: {e}", output.display()))?;
    Ok(presets.len())
}

fn build_source_index(set_files: &[SetFile]) -> SourceCardIndex {
    let mut index = SourceCardIndex::default();
    for set_file in set_files {
        for card in &set_file.data.cards {
            for token_id in &card.related_cards.tokens {
                index
                    .names_by_token_id
                    .entry(token_id.clone())
                    .or_default()
                    .insert(card.face_name.clone().unwrap_or_else(|| card.name.clone()));
                let key = format!(
                    "{}|{}|{}",
                    card.uuid,
                    card.face_name.as_deref().unwrap_or(""),
                    card.identifiers.scryfall_oracle_id.as_deref().unwrap_or("")
                );
                index
                    .refs_by_token_id
                    .entry(token_id.clone())
                    .or_default()
                    .insert(
                        key,
                        TokenSourceRef {
                            card_name: card.name.clone(),
                            face_name: card.face_name.clone(),
                            scryfall_oracle_id: card.identifiers.scryfall_oracle_id.clone(),
                            scryfall_id: card.identifiers.scryfall_id.clone(),
                        },
                    );
            }
        }
    }
    index
}

fn build_preset(
    token: &SetToken,
    set_file: &SetFile,
    source_index: &SourceCardIndex,
) -> Result<Option<TokenPreset>, String> {
    let applicable_keyword_names = applicable_keyword_names(token);
    let body = TokenCharacteristics {
        display_name: token
            .face_name
            .clone()
            .unwrap_or_else(|| token.name.clone()),
        power: parse_pt(token.power.as_deref()),
        toughness: parse_pt(token.toughness.as_deref()),
        core_types: token
            .types
            .iter()
            .filter_map(|s| CoreType::from_str(s).ok())
            .collect(),
        subtypes: token.subtypes.clone(),
        supertypes: token
            .supertypes
            .iter()
            .filter_map(|s| Supertype::from_str(s).ok())
            .collect(),
        colors: token.colors.iter().filter_map(|s| parse_color(s)).collect(),
        keywords: applicable_keyword_names
            .iter()
            .filter_map(|s| supported_token_keyword(s))
            .collect(),
    };

    if !is_catalog_token_body(&body) {
        return Ok(None);
    }
    let category = classify_token(&body)?;
    let pt_provenance = classify_pt_provenance(token, &body);
    let source_card_names: Vec<String> = source_index
        .names_by_token_id
        .get(&token.uuid)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .chain(token.related_cards.reverse_related.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let source_card_refs = source_index
        .refs_by_token_id
        .get(&token.uuid)
        .map(|refs| refs.values().cloned().collect())
        .unwrap_or_default();

    let token_image_ref = token.identifiers.scryfall_id.as_ref().map(|scryfall_id| {
        if token.layout == "double_faced_token" && token.face_name.is_none() {
            return Err(format!(
                "double-faced token {} ({}) lacks faceName",
                token.uuid, token.name
            ));
        }
        Ok(TokenImageRef {
            scryfall_id: scryfall_id.clone(),
            scryfall_oracle_id: token.identifiers.scryfall_oracle_id.clone(),
            face_name: token.face_name.clone(),
            preset_id: token.uuid.clone(),
        })
    });

    let token_image_ref = match token_image_ref {
        Some(result) => Some(result?),
        None => None,
    };
    let fidelity = classify_fidelity(token, &category, &body);

    Ok(Some(TokenPreset {
        id: token.uuid.clone(),
        category,
        fidelity,
        pt_provenance,
        body,
        source_card_names,
        source_card_refs,
        token_image_ref,
        set_code: set_file.data.code.clone(),
        set_name: set_file.data.name.clone(),
        collector_number: token.number.clone(),
        released_at: set_file.data.release_date.clone(),
        type_line: token.type_line.clone(),
        rules_text: token.text.clone(),
    }))
}

fn is_catalog_token_body(body: &TokenCharacteristics) -> bool {
    body.core_types.iter().any(|card_type| {
        matches!(
            card_type,
            CoreType::Artifact | CoreType::Creature | CoreType::Enchantment | CoreType::Land
        )
    })
}

fn parse_pt(value: Option<&str>) -> Option<i32> {
    value.and_then(|s| s.parse::<i32>().ok())
}

fn classify_pt_provenance(token: &SetToken, body: &TokenCharacteristics) -> TokenPtProvenance {
    let has_dynamic_power = token
        .power
        .as_deref()
        .is_some_and(|value| parse_pt(Some(value)).is_none());
    let has_dynamic_toughness = token
        .toughness
        .as_deref()
        .is_some_and(|value| parse_pt(Some(value)).is_none());

    if body.core_types.contains(&CoreType::Creature) && (has_dynamic_power || has_dynamic_toughness)
    {
        TokenPtProvenance::SourceDefinedOrDynamic {
            power: token.power.clone(),
            toughness: token.toughness.clone(),
        }
    } else {
        TokenPtProvenance::FixedOrAbsent
    }
}

fn parse_color(value: &str) -> Option<ManaColor> {
    match value {
        "W" => Some(ManaColor::White),
        "U" => Some(ManaColor::Blue),
        "B" => Some(ManaColor::Black),
        "R" => Some(ManaColor::Red),
        "G" => Some(ManaColor::Green),
        _ => None,
    }
}

fn supported_token_keyword(value: &str) -> Option<Keyword> {
    match value {
        "Flying" => Some(Keyword::Flying),
        "First strike" | "First Strike" => Some(Keyword::FirstStrike),
        "Double strike" | "Double Strike" => Some(Keyword::DoubleStrike),
        "Trample" => Some(Keyword::Trample),
        "Deathtouch" => Some(Keyword::Deathtouch),
        "Lifelink" => Some(Keyword::Lifelink),
        "Vigilance" => Some(Keyword::Vigilance),
        "Haste" => Some(Keyword::Haste),
        "Reach" => Some(Keyword::Reach),
        "Defender" => Some(Keyword::Defender),
        "Menace" => Some(Keyword::Menace),
        "Indestructible" => Some(Keyword::Indestructible),
        "Hexproof" => Some(Keyword::Hexproof),
        "Shroud" => Some(Keyword::Shroud),
        "Flash" => Some(Keyword::Flash),
        "Fear" => Some(Keyword::Fear),
        "Intimidate" => Some(Keyword::Intimidate),
        "Skulk" => Some(Keyword::Skulk),
        "Shadow" => Some(Keyword::Shadow),
        "Horsemanship" => Some(Keyword::Horsemanship),
        "Wither" => Some(Keyword::Wither),
        "Infect" => Some(Keyword::Infect),
        "Prowess" => Some(Keyword::Prowess),
        "Undying" => Some(Keyword::Undying),
        "Persist" => Some(Keyword::Persist),
        "Changeling" => Some(Keyword::Changeling),
        "Phasing" => Some(Keyword::Phasing),
        "Battle Cry" | "Battle cry" => Some(Keyword::Battlecry),
        "Decayed" => Some(Keyword::Decayed),
        _ => None,
    }
}

fn applicable_keyword_names(token: &SetToken) -> Vec<String> {
    if token.layout != "double_faced_token" || token.face_name.is_none() {
        return token.keywords.clone();
    }

    let Some(text) = token.text.as_deref() else {
        return Vec::new();
    };
    let normalized = text.to_ascii_lowercase().replace('\n', " ");
    token
        .keywords
        .iter()
        .filter(|keyword| {
            let needle = keyword.to_ascii_lowercase();
            normalized.contains(&needle)
        })
        .cloned()
        .collect()
}

fn classify_token(body: &TokenCharacteristics) -> Result<TokenCategory, String> {
    for subtype in &body.subtypes {
        if let Some(kind) = predefined_kind(subtype) {
            return Ok(TokenCategory::PredefinedArtifact { kind });
        }
    }

    if body.core_types.contains(&CoreType::Creature) {
        return Ok(TokenCategory::Creature);
    }
    if body.core_types.contains(&CoreType::Enchantment) && body.subtypes.iter().any(|s| s == "Aura")
    {
        return Ok(TokenCategory::Aura);
    }
    if body.core_types.contains(&CoreType::Artifact)
        && body.subtypes.iter().any(|s| s == "Equipment")
    {
        return Ok(TokenCategory::Equipment);
    }
    if body.core_types.contains(&CoreType::Artifact) && body.subtypes.iter().any(|s| s == "Vehicle")
    {
        return Ok(TokenCategory::Vehicle);
    }
    if body.core_types.contains(&CoreType::Enchantment) {
        return Ok(TokenCategory::Enchantment);
    }
    if body.core_types.contains(&CoreType::Land) {
        return Ok(TokenCategory::Land);
    }
    if body.core_types.contains(&CoreType::Artifact) {
        return Ok(TokenCategory::Artifact);
    }

    Err(format!("unclassified token body: {:?}", body))
}

fn predefined_kind(subtype: &str) -> Option<PredefinedTokenKind> {
    match subtype {
        "Treasure" => Some(PredefinedTokenKind::Treasure),
        "Food" => Some(PredefinedTokenKind::Food),
        "Gold" => Some(PredefinedTokenKind::Gold),
        "Clue" => Some(PredefinedTokenKind::Clue),
        "Blood" => Some(PredefinedTokenKind::Blood),
        "Powerstone" => Some(PredefinedTokenKind::Powerstone),
        "Map" => Some(PredefinedTokenKind::Map),
        "Lander" => Some(PredefinedTokenKind::Lander),
        _ => None,
    }
}

fn classify_fidelity(
    token: &SetToken,
    category: &TokenCategory,
    body: &TokenCharacteristics,
) -> PresetFidelity {
    let applicable_keyword_names = applicable_keyword_names(token);
    let unsupported_keyword = applicable_keyword_names
        .iter()
        .any(|s| supported_token_keyword(s).is_none());
    if unsupported_keyword {
        return PresetFidelity::PartialMissingAbilities;
    }

    let Some(text) = token
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return PresetFidelity::Full;
    };

    if matches!(category, TokenCategory::PredefinedArtifact { .. }) {
        return PresetFidelity::Full;
    }
    if !body.keywords.is_empty() && text_matches_keywords(text, &body.keywords) {
        return PresetFidelity::Full;
    }
    PresetFidelity::PartialMissingAbilities
}

fn text_matches_keywords(text: &str, keywords: &[Keyword]) -> bool {
    let normalized = text.to_ascii_lowercase().replace('\n', " ");
    keywords.iter().all(|keyword| {
        let needle = format!("{keyword:?}")
            .replace("FirstStrike", "First strike")
            .replace("DoubleStrike", "Double strike")
            .replace("Battlecry", "Battle cry")
            .to_ascii_lowercase();
        normalized.contains(&needle)
    })
}
