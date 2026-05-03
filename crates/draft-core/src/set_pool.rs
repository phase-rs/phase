use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

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
    pub weight: u32,
}

/// A named sheet of cards (e.g., "common", "uncommon", "rareMythic").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetDefinition {
    pub cards: Vec<SheetCard>,
    pub total_weight: u32,
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
