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
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        sizes
            .all(|other| other == size)
            .then(|| u8::try_from(size).ok())
            .flatten()
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
}
