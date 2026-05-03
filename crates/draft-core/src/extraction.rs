use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use serde::Deserialize;

use crate::set_pool::{
    LimitedCardPrint, LimitedSetPool, PackSlot, PackVariant, Rarity, SheetCard, SheetDefinition,
    WeightedSheetChoice,
};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ExtractionError {
    #[error("failed to parse MTGJSON set file: {0}")]
    ParseError(#[from] serde_json::Error),
    #[error("extraction error: {0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// MTGJSON deserialization types (private)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MtgjsonSetFile {
    data: MtgjsonSetData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MtgjsonSetData {
    code: String,
    name: String,
    release_date: Option<String>,
    #[serde(default)]
    booster: Option<MtgjsonBooster>,
    #[serde(default)]
    cards: Vec<MtgjsonCard>,
}

#[derive(Deserialize)]
struct MtgjsonBooster {
    #[serde(default)]
    play: Option<MtgjsonBoosterPlay>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MtgjsonBoosterPlay {
    sheets: HashMap<String, MtgjsonSheet>,
    boosters: Vec<MtgjsonBoosterVariant>,
    boosters_total_weight: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MtgjsonSheet {
    cards: HashMap<String, u32>,
    total_weight: u32,
    #[serde(default)]
    foil: bool,
    #[serde(default)]
    balance_colors: bool,
}

#[derive(Deserialize)]
struct MtgjsonBoosterVariant {
    contents: HashMap<String, u8>,
    weight: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MtgjsonCard {
    uuid: String,
    name: String,
    rarity: String,
    number: String,
    set_code: String,
    #[serde(default)]
    booster_types: Vec<String>,
    #[serde(default)]
    supertypes: Vec<String>,
}

// ---------------------------------------------------------------------------
// Rarity mapping
// ---------------------------------------------------------------------------

fn parse_rarity(s: &str) -> Rarity {
    match s {
        "common" => Rarity::Common,
        "uncommon" => Rarity::Uncommon,
        "rare" => Rarity::Rare,
        "mythic" => Rarity::Mythic,
        "special" => Rarity::Special,
        "bonus" => Rarity::Bonus,
        _ => Rarity::Special,
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract a [`LimitedSetPool`] from raw MTGJSON per-set JSON content.
///
/// Returns `Ok(None)` if the set has no `booster.play` section (not draftable).
pub fn extract_set_pool(json_content: &str) -> Result<Option<LimitedSetPool>, ExtractionError> {
    let file: MtgjsonSetFile = serde_json::from_str(json_content)?;
    let data = file.data;

    let play = match data.booster.and_then(|b| b.play) {
        Some(p) => p,
        None => return Ok(None),
    };

    // Build UUID -> card lookup
    let card_by_uuid: HashMap<&str, &MtgjsonCard> =
        data.cards.iter().map(|c| (c.uuid.as_str(), c)).collect();

    // Track which UUIDs appear in any sheet (for prints eligibility)
    let mut uuids_in_sheets: std::collections::HashSet<&str> = std::collections::HashSet::new();

    // Build sheets
    let mut sheets = BTreeMap::new();
    for (sheet_name, mtg_sheet) in &play.sheets {
        let mut cards = Vec::new();
        for (uuid, &weight) in &mtg_sheet.cards {
            uuids_in_sheets.insert(uuid.as_str());
            if let Some(card) = card_by_uuid.get(uuid.as_str()) {
                cards.push(SheetCard {
                    name: card.name.clone(),
                    set_code: card.set_code.clone(),
                    collector_number: card.number.clone(),
                    rarity: parse_rarity(&card.rarity),
                    weight,
                });
            } else {
                eprintln!(
                    "Warning: UUID {} in sheet '{}' not found in cards array, skipping",
                    uuid, sheet_name
                );
            }
        }
        // Sort cards by name for deterministic output
        cards.sort_by(|a, b| a.name.cmp(&b.name));
        sheets.insert(
            sheet_name.clone(),
            SheetDefinition {
                cards,
                total_weight: mtg_sheet.total_weight,
                foil: mtg_sheet.foil,
                balance_colors: mtg_sheet.balance_colors,
            },
        );
    }

    // Build pack variants
    let pack_variants: Vec<PackVariant> = play
        .boosters
        .iter()
        .map(|variant| {
            let mut contents: Vec<PackSlot> = variant
                .contents
                .iter()
                .map(|(sheet_name, &count)| PackSlot {
                    slot: sheet_name.clone(),
                    count,
                    choices: vec![WeightedSheetChoice {
                        sheet: sheet_name.clone(),
                        weight: 1,
                    }],
                })
                .collect();
            // Sort slots by name for deterministic output
            contents.sort_by(|a, b| a.slot.cmp(&b.slot));
            PackVariant {
                contents,
                weight: variant.weight,
            }
        })
        .collect();

    // Build prints: cards that have boosterTypes containing "play" or appear in any sheet
    let prints: Vec<LimitedCardPrint> = data
        .cards
        .iter()
        .filter(|c| {
            c.booster_types.contains(&"play".to_string())
                || uuids_in_sheets.contains(c.uuid.as_str())
        })
        .map(|c| LimitedCardPrint {
            print_id: c.uuid.clone(),
            name: c.name.clone(),
            set_code: c.set_code.clone(),
            collector_number: c.number.clone(),
            rarity: parse_rarity(&c.rarity),
            booster_eligible: c.booster_types.contains(&"play".to_string()),
        })
        .collect();

    // Build basic_lands: cards with "Basic" in supertypes, deduplicated
    let mut basic_lands: Vec<String> = data
        .cards
        .iter()
        .filter(|c| c.supertypes.iter().any(|s| s == "Basic"))
        .map(|c| c.name.clone())
        .collect();
    basic_lands.sort();
    basic_lands.dedup();

    // Fallback: if no basic lands found via supertypes, check sheets with "land" in name
    if basic_lands.is_empty() {
        let mut land_names: Vec<String> = sheets
            .iter()
            .filter(|(name, _)| name.to_lowercase().contains("land"))
            .flat_map(|(_, sheet)| {
                sheet
                    .cards
                    .iter()
                    .filter(|c| c.rarity == Rarity::Common)
                    .map(|c| c.name.clone())
            })
            .collect();
        land_names.sort();
        land_names.dedup();
        basic_lands = land_names;
    }

    Ok(Some(LimitedSetPool {
        code: data.code,
        name: data.name,
        release_date: data.release_date,
        pack_variants,
        pack_variants_total_weight: play.boosters_total_weight,
        sheets,
        prints,
        basic_lands,
    }))
}

/// Extract [`LimitedSetPool`]s from all JSON files in a directory.
///
/// Returns a `BTreeMap` keyed by lowercase set code.
pub fn extract_all_set_pools(
    sets_dir: &Path,
) -> Result<BTreeMap<String, LimitedSetPool>, ExtractionError> {
    let mut pools = BTreeMap::new();

    let entries: Vec<_> = std::fs::read_dir(sets_dir)
        .map_err(|e| ExtractionError::Other(format!("cannot read directory: {e}")))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();

    let total = entries.len();
    for (i, entry) in entries.iter().enumerate() {
        let path = entry.path();
        let filename = path.file_stem().unwrap_or_default().to_string_lossy();
        eprintln!("[{}/{}] Processing {}...", i + 1, total, filename);

        let content = std::fs::read_to_string(&path)
            .map_err(|e| ExtractionError::Other(format!("cannot read {}: {e}", path.display())))?;

        match extract_set_pool(&content)? {
            Some(pool) => {
                let code = pool.code.to_lowercase();
                eprintln!(
                    "  -> {} ({}) — {} sheets, {} prints",
                    pool.name,
                    pool.code,
                    pool.sheets.len(),
                    pool.prints.len()
                );
                pools.insert(code, pool);
            }
            None => {
                eprintln!("  -> skipped (no booster.play section)");
            }
        }
    }

    Ok(pools)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_set_with_booster() -> String {
        r#"{
            "data": {
                "code": "TST",
                "name": "Test Set",
                "releaseDate": "2025-01-01",
                "booster": {
                    "play": {
                        "sheets": {
                            "common": {
                                "cards": {
                                    "uuid-c1": 10,
                                    "uuid-c2": 10,
                                    "uuid-c3": 10
                                },
                                "totalWeight": 30,
                                "foil": false,
                                "balanceColors": true
                            },
                            "rareMythic": {
                                "cards": {
                                    "uuid-r1": 7,
                                    "uuid-m1": 1
                                },
                                "totalWeight": 8,
                                "foil": false
                            }
                        },
                        "boosters": [
                            {
                                "contents": {
                                    "common": 10,
                                    "rareMythic": 1
                                },
                                "weight": 1
                            }
                        ],
                        "boostersTotalWeight": 1
                    }
                },
                "cards": [
                    { "uuid": "uuid-c1", "name": "Test Common A", "rarity": "common", "number": "1", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-c2", "name": "Test Common B", "rarity": "common", "number": "2", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-c3", "name": "Test Common C", "rarity": "common", "number": "3", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-r1", "name": "Test Rare", "rarity": "rare", "number": "4", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-m1", "name": "Test Mythic", "rarity": "mythic", "number": "5", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] }
                ]
            }
        }"#
        .to_string()
    }

    fn minimal_set_without_booster() -> String {
        r#"{
            "data": {
                "code": "PRM",
                "name": "Promo Set",
                "cards": []
            }
        }"#
        .to_string()
    }

    #[test]
    fn test_extract_set_with_booster_play() {
        let json = minimal_set_with_booster();
        let result = extract_set_pool(&json).unwrap();
        let pool = result.expect("should return Some for set with booster.play");

        assert_eq!(pool.code, "TST");
        assert_eq!(pool.name, "Test Set");
        assert_eq!(pool.release_date.as_deref(), Some("2025-01-01"));
        assert_eq!(pool.sheets.len(), 2);
        assert_eq!(pool.sheets["common"].cards.len(), 3);
        assert_eq!(pool.sheets["rareMythic"].total_weight, 8);
        assert_eq!(pool.sheets["rareMythic"].cards.len(), 2);
        assert_eq!(pool.pack_variants.len(), 1);
        assert_eq!(pool.pack_variants[0].contents.len(), 2);
        assert_eq!(pool.pack_variants[0].weight, 1);
        assert_eq!(pool.pack_variants_total_weight, 1);
        assert!(!pool.prints.is_empty());
        assert_eq!(pool.prints.len(), 5);

        // Verify card names are resolved (not UUIDs)
        for sheet in pool.sheets.values() {
            for card in &sheet.cards {
                assert!(
                    !card.name.starts_with("uuid-"),
                    "card name should be resolved, not a UUID: {}",
                    card.name
                );
            }
        }

        // Verify balance_colors is preserved
        assert!(pool.sheets["common"].balance_colors);
        assert!(!pool.sheets["rareMythic"].balance_colors);
    }

    #[test]
    fn test_extract_set_without_booster_play() {
        let json = minimal_set_without_booster();
        let result = extract_set_pool(&json).unwrap();
        assert!(
            result.is_none(),
            "set without booster.play should return None"
        );
    }

    #[test]
    fn test_extract_set_invalid_json() {
        let result = extract_set_pool("not valid json at all");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ExtractionError::ParseError(_)
        ));
    }

    #[test]
    fn test_uuid_not_in_cards_is_skipped() {
        let json = r#"{
            "data": {
                "code": "TST",
                "name": "Test",
                "booster": {
                    "play": {
                        "sheets": {
                            "common": {
                                "cards": {
                                    "uuid-exists": 10,
                                    "uuid-missing": 10
                                },
                                "totalWeight": 20
                            }
                        },
                        "boosters": [{ "contents": { "common": 10 }, "weight": 1 }],
                        "boostersTotalWeight": 1
                    }
                },
                "cards": [
                    { "uuid": "uuid-exists", "name": "Found Card", "rarity": "common", "number": "1", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] }
                ]
            }
        }"#;

        let result = extract_set_pool(json).unwrap();
        let pool = result.expect("should still succeed with missing UUID");
        assert_eq!(
            pool.sheets["common"].cards.len(),
            1,
            "missing UUID should be skipped, leaving only the found card"
        );
        assert_eq!(pool.sheets["common"].cards[0].name, "Found Card");
    }

    #[test]
    fn test_rarity_mapping() {
        let json = r#"{
            "data": {
                "code": "TST",
                "name": "Test",
                "booster": {
                    "play": {
                        "sheets": {
                            "all": {
                                "cards": {
                                    "uuid-c": 1,
                                    "uuid-u": 1,
                                    "uuid-r": 1,
                                    "uuid-m": 1
                                },
                                "totalWeight": 4
                            }
                        },
                        "boosters": [{ "contents": { "all": 1 }, "weight": 1 }],
                        "boostersTotalWeight": 1
                    }
                },
                "cards": [
                    { "uuid": "uuid-c", "name": "C", "rarity": "common", "number": "1", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-u", "name": "U", "rarity": "uncommon", "number": "2", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-r", "name": "R", "rarity": "rare", "number": "3", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-m", "name": "M", "rarity": "mythic", "number": "4", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] }
                ]
            }
        }"#;

        let result = extract_set_pool(json).unwrap().unwrap();

        let sheet_cards = &result.sheets["all"].cards;
        let by_name: HashMap<&str, &SheetCard> =
            sheet_cards.iter().map(|c| (c.name.as_str(), c)).collect();

        assert_eq!(by_name["C"].rarity, Rarity::Common);
        assert_eq!(by_name["U"].rarity, Rarity::Uncommon);
        assert_eq!(by_name["R"].rarity, Rarity::Rare);
        assert_eq!(by_name["M"].rarity, Rarity::Mythic);

        // Also check prints rarity mapping
        let prints_by_name: HashMap<&str, &LimitedCardPrint> =
            result.prints.iter().map(|p| (p.name.as_str(), p)).collect();
        assert_eq!(prints_by_name["C"].rarity, Rarity::Common);
        assert_eq!(prints_by_name["M"].rarity, Rarity::Mythic);
    }

    #[test]
    fn test_basic_lands_from_supertypes() {
        let json = r#"{
            "data": {
                "code": "TST",
                "name": "Test",
                "booster": {
                    "play": {
                        "sheets": {
                            "common": { "cards": { "uuid-c1": 1 }, "totalWeight": 1 }
                        },
                        "boosters": [{ "contents": { "common": 1 }, "weight": 1 }],
                        "boostersTotalWeight": 1
                    }
                },
                "cards": [
                    { "uuid": "uuid-c1", "name": "Goblin", "rarity": "common", "number": "1", "setCode": "TST", "boosterTypes": ["play"], "supertypes": [] },
                    { "uuid": "uuid-p1", "name": "Plains", "rarity": "common", "number": "260", "setCode": "TST", "boosterTypes": [], "supertypes": ["Basic"] },
                    { "uuid": "uuid-p2", "name": "Plains", "rarity": "common", "number": "261", "setCode": "TST", "boosterTypes": [], "supertypes": ["Basic"] },
                    { "uuid": "uuid-i1", "name": "Island", "rarity": "common", "number": "262", "setCode": "TST", "boosterTypes": [], "supertypes": ["Basic"] }
                ]
            }
        }"#;

        let result = extract_set_pool(json).unwrap().unwrap();
        assert_eq!(result.basic_lands, vec!["Island", "Plains"]);
    }
}
