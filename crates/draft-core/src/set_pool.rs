use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use engine::types::card::DraftEffect;

/// Rarity of a card printing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Mythic,
    Special,
    Bonus,
}

/// A weighted choice of which sheet fills a pack slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WeightedSheetChoice {
    pub sheet: String,
    pub weight: u32,
}

/// A slot in a draft pack (e.g., "common" slot with count 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackSlot {
    pub slot: String,
    pub count: u8,
    pub choices: Vec<WeightedSheetChoice>,
}

/// A card entry within a sheet, with its selection weight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetCard {
    pub name: String,
    pub set_code: String,
    pub collector_number: String,
    pub rarity: Rarity,
    pub weight: u64,
    /// Color identity letters, e.g. ["W", "U"]. Populated from MTGJSON at extraction.
    #[serde(default)]
    pub colors: Vec<String>,
    /// Converted mana cost. Populated from MTGJSON at extraction.
    #[serde(default)]
    pub cmc: u8,
    /// Full type line, e.g. "Creature — Human Wizard". Populated from MTGJSON at extraction.
    #[serde(default)]
    pub type_line: String,
    /// Draft-time effect parsed from the card's Oracle text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_effect: Option<DraftEffect>,
}

/// A named sheet of cards (e.g., "common", "uncommon", "rareMythic").
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetDefinition {
    pub cards: Vec<SheetCard>,
    pub total_weight: u64,
    /// MTGJSON's `allowDuplicates`: repeated pulls from this sheet may select
    /// the same card printing.
    #[serde(default)]
    pub allow_duplicates: bool,
    /// MTGJSON's `fixed`: every weighted card position in this sheet is
    /// included instead of being randomly selected.
    #[serde(default)]
    pub fixed: bool,
    pub foil: bool,
    pub balance_colors: bool,
}

/// A single pack variant with slot-to-sheet mappings and probability weight.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackVariant {
    pub contents: Vec<PackSlot>,
    pub weight: u32,
}

/// A card printing relevant to Limited/Draft.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitedCardPrint {
    pub print_id: String,
    pub name: String,
    pub set_code: String,
    pub collector_number: String,
    pub rarity: Rarity,
    pub booster_eligible: bool,
}

/// Full draft pool data for a single set.
#[derive(Debug, Clone, Serialize)]
pub struct LimitedSetPool {
    pub code: String,
    pub name: String,
    pub release_date: Option<String>,
    pub pack_variants: Vec<PackVariant>,
    pub pack_variants_total_weight: u32,
    pub sheets: BTreeMap<String, SheetDefinition>,
    pub prints: Vec<LimitedCardPrint>,
    pub basic_lands: Vec<String>,
}

/// Normalize booster pack variants where MTGJSON collation data omitted non-card
/// items (such as tokens or marketing inserts) that originally shared a print sheet
/// with playable cards, causing some booster variants to report fewer slots than the
/// booster's actual physical size.
///
/// In Unglued (UGL, 1998), each booster pack contained 10 cards: 1 rare, 2 uncommons,
/// 6 commons, and 1 basic land. On physical print sheets, token cards were inserted
/// replacing an uncommon in 2/5 of packs. MTGJSON omitted the token card from the
/// draft booster sheet data, resulting in an incomplete 9-card variant (with 1 uncommon
/// instead of 2). In limited play (draft and sealed), tokens are not drafted (CR 108.1),
/// and every booster pack must open uniformly with the full 10 playable cards (including
/// 2 uncommons).
pub fn normalize_pack_variants(set_code: &str, pack_variants: &mut [PackVariant]) {
    if set_code.eq_ignore_ascii_case("UGL") {
        for variant in pack_variants {
            for slot in &mut variant.contents {
                if slot.slot == "uncommon" && slot.count < 2 {
                    slot.count = 2;
                }
            }
        }
    }
}

impl<'de> Deserialize<'de> for LimitedSetPool {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawLimitedSetPool {
            code: String,
            name: String,
            release_date: Option<String>,
            pack_variants: Vec<PackVariant>,
            pack_variants_total_weight: u32,
            sheets: BTreeMap<String, SheetDefinition>,
            prints: Vec<LimitedCardPrint>,
            basic_lands: Vec<String>,
        }

        let mut raw = RawLimitedSetPool::deserialize(deserializer)?;
        normalize_pack_variants(&raw.code, &mut raw.pack_variants);
        Ok(LimitedSetPool {
            code: raw.code,
            name: raw.name,
            release_date: raw.release_date,
            pack_variants: raw.pack_variants,
            pack_variants_total_weight: raw.pack_variants_total_weight,
            sheets: raw.sheets,
            prints: raw.prints,
            basic_lands: raw.basic_lands,
        })
    }
}

impl LimitedSetPool {
    /// Returns the MTGJSON-declared card count when every pack variant has the
    /// same number of slots. Set-backed draft and sealed sessions need this
    /// value for their public state and persisted-pool invariants.
    pub fn cards_per_pack(&self) -> Option<u8> {
        let mut sizes = self.pack_variants.iter().map(|variant| {
            variant
                .contents
                .iter()
                .map(|slot| u16::from(slot.count))
                .sum::<u16>()
        });
        let size = sizes.next()?;
        if sizes.all(|other| other == size) {
            return u8::try_from(size).ok();
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn variant(card_count: u8) -> PackVariant {
        PackVariant {
            contents: vec![PackSlot {
                slot: "common".to_string(),
                count: card_count,
                choices: vec![],
            }],
            weight: 1,
        }
    }

    #[test]
    fn cards_per_pack_uses_the_shared_variant_slot_total() {
        let pool = LimitedSetPool {
            code: "TST".to_string(),
            name: "Test".to_string(),
            release_date: None,
            pack_variants: vec![variant(15), variant(15)],
            pack_variants_total_weight: 2,
            sheets: BTreeMap::new(),
            prints: vec![],
            basic_lands: vec![],
        };

        assert_eq!(pool.cards_per_pack(), Some(15));
    }

    #[test]
    fn cards_per_pack_rejects_mixed_variant_sizes() {
        let pool = LimitedSetPool {
            code: "TST".to_string(),
            name: "Test".to_string(),
            release_date: None,
            pack_variants: vec![variant(14), variant(15)],
            pack_variants_total_weight: 2,
            sheets: BTreeMap::new(),
            prints: vec![],
            basic_lands: vec![],
        };

        assert_eq!(pool.cards_per_pack(), None);
    }

    #[test]
    fn unglued_pool_with_mismatched_variants_reports_no_cards_per_pack() {
        let pool = LimitedSetPool {
            code: "UGL".to_string(),
            name: "Unglued".to_string(),
            release_date: None,
            pack_variants: vec![variant(10), variant(11)],
            pack_variants_total_weight: 2,
            sheets: BTreeMap::new(),
            prints: vec![],
            basic_lands: vec![],
        };

        assert_eq!(pool.cards_per_pack(), None);
    }

    #[test]
    fn unglued_pool_normalizes_and_reports_ten_cards_per_pack() {
        let mut variants = vec![
            PackVariant {
                contents: vec![
                    PackSlot {
                        slot: "basic".to_string(),
                        count: 1,
                        choices: vec![],
                    },
                    PackSlot {
                        slot: "common".to_string(),
                        count: 6,
                        choices: vec![],
                    },
                    PackSlot {
                        slot: "rare".to_string(),
                        count: 1,
                        choices: vec![],
                    },
                    PackSlot {
                        slot: "uncommon".to_string(),
                        count: 2,
                        choices: vec![],
                    },
                ],
                weight: 3,
            },
            PackVariant {
                contents: vec![
                    PackSlot {
                        slot: "basic".to_string(),
                        count: 1,
                        choices: vec![],
                    },
                    PackSlot {
                        slot: "common".to_string(),
                        count: 6,
                        choices: vec![],
                    },
                    PackSlot {
                        slot: "rare".to_string(),
                        count: 1,
                        choices: vec![],
                    },
                    PackSlot {
                        slot: "uncommon".to_string(),
                        count: 1,
                        choices: vec![],
                    },
                ],
                weight: 2,
            },
        ];

        normalize_pack_variants("UGL", &mut variants);

        // Variant 1 uncommon slot is normalized to 2
        assert_eq!(
            variants[1]
                .contents
                .iter()
                .find(|s| s.slot == "uncommon")
                .unwrap()
                .count,
            2
        );

        let pool = LimitedSetPool {
            code: "UGL".to_string(),
            name: "Unglued".to_string(),
            release_date: None,
            pack_variants: variants,
            pack_variants_total_weight: 5,
            sheets: BTreeMap::new(),
            prints: vec![],
            basic_lands: vec![],
        };

        assert_eq!(pool.cards_per_pack(), Some(10));
    }

    #[test]
    fn deserializing_unglued_pool_normalizes_variants() {
        let json = r#"{
            "code": "UGL",
            "name": "Unglued",
            "release_date": null,
            "pack_variants": [
                {
                    "contents": [
                        { "slot": "basic", "count": 1, "choices": [] },
                        { "slot": "common", "count": 6, "choices": [] },
                        { "slot": "rare", "count": 1, "choices": [] },
                        { "slot": "uncommon", "count": 2, "choices": [] }
                    ],
                    "weight": 3
                },
                {
                    "contents": [
                        { "slot": "basic", "count": 1, "choices": [] },
                        { "slot": "common", "count": 6, "choices": [] },
                        { "slot": "rare", "count": 1, "choices": [] },
                        { "slot": "uncommon", "count": 1, "choices": [] }
                    ],
                    "weight": 2
                }
            ],
            "pack_variants_total_weight": 5,
            "sheets": {},
            "prints": [],
            "basic_lands": []
        }"#;

        let pool: LimitedSetPool = serde_json::from_str(json).expect("deserialization succeeds");
        assert_eq!(pool.cards_per_pack(), Some(10));
        assert_eq!(
            pool.pack_variants[0]
                .contents
                .iter()
                .find(|s| s.slot == "uncommon")
                .unwrap()
                .count,
            2
        );
        assert_eq!(
            pool.pack_variants[1]
                .contents
                .iter()
                .find(|s| s.slot == "uncommon")
                .unwrap()
                .count,
            2
        );
    }
}
