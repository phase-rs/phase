use std::collections::{BTreeMap, HashMap, HashSet};
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
    /// Play Booster product (MKM 2024 onward). Preferred when present.
    #[serde(default)]
    play: Option<MtgjsonBoosterConfig>,
    /// Draft Booster product (the legacy limited product, ~2018–2024). Sets
    /// printed before Play Boosters carry `draft` but no `play`.
    #[serde(default)]
    draft: Option<MtgjsonBoosterConfig>,
    /// The unnamed "standard" booster MTGJSON emits for the oldest expansions
    /// (Ice Age, Antiquities, Legends, …) that predate the draft/set/collector
    /// product split. It is the de-facto draft booster for those sets.
    #[serde(default)]
    default: Option<MtgjsonBoosterConfig>,
}

impl MtgjsonBooster {
    /// The draftable booster configuration, in product-recency order: modern
    /// Play Booster, else legacy Draft Booster, else the `default` booster the
    /// oldest expansions carry. All three share an identical MTGJSON shape
    /// (sheets + weighted boosters), so any one drives extraction. The three
    /// are mutually exclusive across the corpus, so the order only documents
    /// intent. Platform-only products (`arena`, `mtgo`) and non-draft products
    /// (`set`, `collector`, `jumpstart`) are deliberately excluded.
    fn draftable(&self) -> Option<&MtgjsonBoosterConfig> {
        // Eager `.or()` (not `.or_else`): `as_ref()` is trivial and side-effect
        // free, so clippy::unnecessary_lazy_evaluations rejects a lazy closure.
        self.play
            .as_ref()
            .or(self.draft.as_ref())
            .or(self.default.as_ref())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MtgjsonBoosterConfig {
    sheets: HashMap<String, MtgjsonSheet>,
    boosters: Vec<MtgjsonBoosterVariant>,
    boosters_total_weight: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MtgjsonSheet {
    cards: HashMap<String, u64>,
    total_weight: u64,
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
    /// Color identity letters (e.g. ["W", "U"]). Used for bot AI color preference.
    #[serde(default)]
    colors: Vec<String>,
    /// Converted mana cost. Used for bot AI curve awareness.
    #[serde(default, alias = "manaValue")]
    mana_value: f64,
    /// Full type line (e.g. "Creature — Human Wizard"). Used for frontend sorting.
    #[serde(default, rename = "type")]
    type_line: String,
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

/// Build a UUID → card index spanning one or more parsed set datas.
///
/// Supplemental booster sheets (`specialGuest`, `mysticalArchive`, `theList`,
/// `breakingNews`, `sourceMaterial`, …) reference printings that live in *other*
/// sets' MTGJSON files (`SPG`, `STA`, `OTP`, …), so resolving them needs an index
/// over the whole downloaded corpus, not just the set being extracted.
fn build_card_index(sets: &[MtgjsonSetData]) -> HashMap<&str, &MtgjsonCard> {
    sets.iter()
        .flat_map(|d| d.cards.iter())
        .map(|c| (c.uuid.as_str(), c))
        .collect()
}

/// Extract a [`LimitedSetPool`] from raw MTGJSON per-set JSON content.
///
/// Returns `Ok(None)` if the set has no draftable booster (`play`, `draft`, or
/// `default`). Sheet UUIDs are resolved against this set's own cards only — use
/// [`extract_all_set_pools`] when supplemental sheets need cross-set resolution.
pub fn extract_set_pool(json_content: &str) -> Result<Option<LimitedSetPool>, ExtractionError> {
    let file: MtgjsonSetFile = serde_json::from_str(json_content)?;
    let card_index = build_card_index(std::slice::from_ref(&file.data));
    Ok(extract_set_pool_indexed(&file.data, &card_index))
}

/// Extract a [`LimitedSetPool`] from one set's parsed data, resolving sheet UUIDs
/// against `card_index` (which may span multiple sets). Returns `None` if the set
/// has no draftable booster config (`play`/`draft`/`default`). `prints` and
/// `basic_lands` stay set-local — they describe *this* set's print run, not the corpus.
fn extract_set_pool_indexed(
    data: &MtgjsonSetData,
    card_index: &HashMap<&str, &MtgjsonCard>,
) -> Option<LimitedSetPool> {
    let booster = data.booster.as_ref().and_then(MtgjsonBooster::draftable)?;

    // Track which UUIDs appear in any sheet (for prints eligibility).
    let mut uuids_in_sheets: HashSet<&str> = HashSet::new();

    // Build sheets, resolving UUIDs against the (possibly cross-set) index.
    let mut sheets = BTreeMap::new();
    for (sheet_name, mtg_sheet) in &booster.sheets {
        let mut cards = Vec::new();
        for (uuid, &weight) in &mtg_sheet.cards {
            uuids_in_sheets.insert(uuid.as_str());
            if let Some(card) = card_index.get(uuid.as_str()) {
                cards.push(SheetCard {
                    name: card.name.clone(),
                    set_code: card.set_code.clone(),
                    collector_number: card.number.clone(),
                    rarity: parse_rarity(&card.rarity),
                    weight,
                    colors: card.colors.clone(),
                    cmc: card.mana_value as u8,
                    type_line: card.type_line.clone(),
                });
            } else {
                eprintln!(
                    "Warning: UUID {uuid} in sheet '{sheet_name}' of set {} not found \
                     in any downloaded set, skipping (pack generation will backfill)",
                    data.code
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
    let pack_variants: Vec<PackVariant> = booster
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

    // Build prints: cards tagged for the booster pool or appearing in any sheet.
    // Set-local: this is *this* set's print run, not the cross-set index.
    //
    // `booster_eligible` means "can be opened in a pack of this set", which is
    // exactly sheet membership — the same ground truth across every era. The
    // per-card MTGJSON `boosterTypes` field cannot answer this: it carries pool
    // tags (`default`/`deck`), never the set-level product key, so the old
    // `contains("play")` check was `false` for every card in every set (Play
    // Boosters included) and is unrelated to the `play`/`draft`/`default`
    // product fallback in `MtgjsonBooster::draftable`.
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
            booster_eligible: uuids_in_sheets.contains(c.uuid.as_str()),
        })
        .collect();

    // Build basic_lands: cards with "Basic" in supertypes, deduplicated (set-local).
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

    Some(LimitedSetPool {
        code: data.code.clone(),
        name: data.name.clone(),
        release_date: data.release_date.clone(),
        pack_variants,
        pack_variants_total_weight: booster.boosters_total_weight,
        sheets,
        prints,
        basic_lands,
    })
}

/// Extract [`LimitedSetPool`]s from all JSON files in a directory.
///
/// Returns a `BTreeMap` keyed by lowercase set code. Parses every file once, then
/// resolves all booster sheets against a single corpus-wide UUID index, so
/// supplemental sheets (`specialGuest` etc.) that point at other sets' printings
/// resolve as long as those sets' files are present in `sets_dir`.
pub fn extract_all_set_pools(
    sets_dir: &Path,
) -> Result<BTreeMap<String, LimitedSetPool>, ExtractionError> {
    let read_dir = std::fs::read_dir(sets_dir)
        .map_err(|e| ExtractionError::Other(format!("cannot read directory: {e}")))?;

    // Collect the `.json` entries, surfacing directory-entry read errors instead
    // of silently dropping them. Sort for a deterministic parse/progress/error
    // order regardless of the OS-dependent `read_dir` order.
    let mut entries: Vec<std::path::PathBuf> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for entry in read_dir {
        match entry {
            Ok(e) => {
                let path = e.path();
                if path.extension().is_some_and(|ext| ext == "json") {
                    entries.push(path);
                }
            }
            Err(e) => failures.push(format!("could not read a directory entry: {e}")),
        }
    }
    entries.sort();
    let total = entries.len();

    // Pass 1: parse every set file once. A cross-set UUID index needs them all
    // resident simultaneously (`specialGuest` etc. point at other sets' cards).
    // Collect every per-file failure (named by path) rather than aborting on the
    // first, so a corpus with several bad files reports them all in one run.
    let mut datas: Vec<MtgjsonSetData> = Vec::with_capacity(total);
    for (i, path) in entries.iter().enumerate() {
        let filename = path.file_stem().unwrap_or_default().to_string_lossy();
        eprintln!("[{}/{}] Parsing {filename}...", i + 1, total);
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(e) => {
                failures.push(format!("cannot read {}: {e}", path.display()));
                continue;
            }
        };
        match serde_json::from_str::<MtgjsonSetFile>(&content) {
            Ok(file) => datas.push(file.data),
            Err(e) => failures.push(format!("cannot parse {}: {e}", path.display())),
        }
    }

    if !failures.is_empty() {
        return Err(ExtractionError::Other(format!(
            "{} set file(s) could not be loaded:\n  - {}",
            failures.len(),
            failures.join("\n  - ")
        )));
    }

    let card_index = build_card_index(&datas);

    // Pass 2: extract a pool from each set whose booster has a `play` config.
    let mut pools = BTreeMap::new();
    for data in &datas {
        if let Some(pool) = extract_set_pool_indexed(data, &card_index) {
            eprintln!(
                "  -> {} ({}) — {} sheets, {} prints",
                pool.name,
                pool.code,
                pool.sheets.len(),
                pool.prints.len()
            );
            pools.insert(pool.code.to_lowercase(), pool);
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

    /// Pre-Play-Booster set: carries a legacy `draft` booster but no `play`.
    /// This is the shape of the entire pre-2024 back catalog (DOM, ELD, WAR, …).
    fn minimal_set_with_draft_booster() -> String {
        r#"{
            "data": {
                "code": "OLD",
                "name": "Old Set",
                "releaseDate": "2019-01-01",
                "booster": {
                    "draft": {
                        "sheets": {
                            "common": {
                                "cards": { "uuid-c1": 10, "uuid-c2": 10 },
                                "totalWeight": 20
                            }
                        },
                        "boosters": [
                            { "contents": { "common": 10 }, "weight": 1 }
                        ],
                        "boostersTotalWeight": 1
                    }
                },
                "cards": [
                    { "uuid": "uuid-c1", "name": "Old Common A", "rarity": "common", "number": "1", "setCode": "OLD", "boosterTypes": [], "supertypes": [] },
                    { "uuid": "uuid-c2", "name": "Old Common B", "rarity": "common", "number": "2", "setCode": "OLD", "boosterTypes": [], "supertypes": [] }
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
    fn test_extract_set_falls_back_to_draft_booster() {
        // Pre-Play-Booster sets have only `booster.draft`; they must still be
        // draftable. This covers the entire pre-2024 back catalog.
        let json = minimal_set_with_draft_booster();
        let pool = extract_set_pool(&json)
            .unwrap()
            .expect("set with only a draft booster should yield a pool");

        assert_eq!(pool.code, "OLD");
        assert_eq!(pool.sheets.len(), 1);
        assert_eq!(pool.sheets["common"].cards.len(), 2);
        assert_eq!(pool.pack_variants.len(), 1);
        assert_eq!(pool.prints.len(), 2);
        // Cards carry no `boosterTypes` tag yet are on the draft sheet, so they
        // are booster-eligible: eligibility is sheet membership, not a tag.
        assert!(pool.prints.iter().all(|p| p.booster_eligible));
    }

    #[test]
    fn test_extract_set_falls_back_to_default_booster() {
        // The oldest expansions (Ice Age, Antiquities, Legends, …) carry only a
        // `default` booster — no `play`/`draft`. They must still be draftable.
        let json = r#"{
            "data": {
                "code": "ICE",
                "name": "Ice Age",
                "releaseDate": "1995-06-01",
                "booster": {
                    "default": {
                        "sheets": {
                            "common": { "cards": { "uuid-c1": 1, "uuid-c2": 1 }, "totalWeight": 2 }
                        },
                        "boosters": [{ "contents": { "common": 2 }, "weight": 1 }],
                        "boostersTotalWeight": 1
                    }
                },
                "cards": [
                    { "uuid": "uuid-c1", "name": "Ice Common A", "rarity": "common", "number": "1", "setCode": "ICE", "boosterTypes": [], "supertypes": [] },
                    { "uuid": "uuid-c2", "name": "Ice Common B", "rarity": "common", "number": "2", "setCode": "ICE", "boosterTypes": [], "supertypes": [] }
                ]
            }
        }"#;

        let pool = extract_set_pool(json)
            .unwrap()
            .expect("set with only a default booster should yield a pool");
        assert_eq!(pool.code, "ICE");
        assert_eq!(pool.sheets["common"].cards.len(), 2);
        assert_eq!(pool.prints.len(), 2);
        assert!(pool.prints.iter().all(|p| p.booster_eligible));
    }

    #[test]
    fn test_play_booster_preferred_over_draft() {
        // A transitional set carrying both products must draft from `play`.
        let json = r#"{
            "data": {
                "code": "DUAL",
                "name": "Dual Set",
                "booster": {
                    "play": {
                        "sheets": { "p": { "cards": { "uuid-p": 1 }, "totalWeight": 1 } },
                        "boosters": [{ "contents": { "p": 1 }, "weight": 1 }],
                        "boostersTotalWeight": 1
                    },
                    "draft": {
                        "sheets": { "d": { "cards": { "uuid-d": 1 }, "totalWeight": 1 } },
                        "boosters": [{ "contents": { "d": 1 }, "weight": 1 }],
                        "boostersTotalWeight": 1
                    }
                },
                "cards": [
                    { "uuid": "uuid-p", "name": "Play Card", "rarity": "common", "number": "1", "setCode": "DUAL", "boosterTypes": [], "supertypes": [] },
                    { "uuid": "uuid-d", "name": "Draft Card", "rarity": "common", "number": "2", "setCode": "DUAL", "boosterTypes": [], "supertypes": [] }
                ]
            }
        }"#;

        let pool = extract_set_pool(json).unwrap().unwrap();
        assert!(
            pool.sheets.contains_key("p") && !pool.sheets.contains_key("d"),
            "play booster sheets must win over draft when both are present"
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

    // --- extract_all_set_pools (directory-level loading) ---

    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let pid = std::process::id();
        let dir = std::env::temp_dir().join(format!("phase_draft_core_{pid}_{tag}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(dir: &std::path::Path, name: &str, contents: &str) {
        std::fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn all_pools_empty_dir_is_ok_and_empty() {
        let dir = scratch_dir("empty");
        let pools = extract_all_set_pools(&dir).unwrap();
        assert!(pools.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_pools_loads_booster_set_and_skips_non_json_and_boosterless() {
        let dir = scratch_dir("valid");
        write_file(&dir, "tst.json", &minimal_set_with_booster());
        write_file(&dir, "prm.json", &minimal_set_without_booster());
        write_file(&dir, "README.txt", "not a set file");

        let pools = extract_all_set_pools(&dir).unwrap();

        // Only the set with a `booster.play` config yields a pool; the
        // boosterless set and the non-`.json` file are skipped.
        assert_eq!(pools.len(), 1);
        assert!(pools.contains_key("tst"));
        assert!(!pools.contains_key("prm"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_pools_reports_every_bad_file_in_one_error() {
        let dir = scratch_dir("bad");
        write_file(&dir, "good.json", &minimal_set_with_booster());
        write_file(&dir, "bad1.json", "{ not valid json");
        write_file(&dir, "bad2.json", r#"{"data": 123}"#);

        let err = extract_all_set_pools(&dir).unwrap_err();
        let msg = err.to_string();

        // Both bad files are named in a single aggregated error rather than the
        // load aborting on the first one.
        assert!(msg.contains("bad1.json"), "expected bad1.json in: {msg}");
        assert!(msg.contains("bad2.json"), "expected bad2.json in: {msg}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn all_pools_missing_directory_is_err() {
        let missing = std::env::temp_dir().join("phase_draft_core_missing_dir_xyz");
        let _ = std::fs::remove_dir_all(&missing);
        assert!(extract_all_set_pools(&missing).is_err());
    }
}
