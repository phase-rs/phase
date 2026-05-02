use crate::types::{DraftCardInstance, DraftPack};

/// Abstraction for pack generation. Phase 53 provides FixturePackSource for testing.
/// Phase 54 provides MtgjsonPackSource backed by draft-pools.json.
pub trait PackSource {
    fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, pack_number: u8) -> DraftPack;
}

/// Test fixture that generates packs with predictable card names.
/// Cards are named "{set_code} Card {seat}-{pack}-{i}" for traceability.
pub struct FixturePackSource {
    pub set_code: String,
    pub cards_per_pack: u8,
}

impl PackSource for FixturePackSource {
    fn generate_pack(&self, _rng: &mut dyn rand::RngCore, seat: u8, pack_number: u8) -> DraftPack {
        let cards = (0..self.cards_per_pack)
            .map(|i| DraftCardInstance {
                instance_id: format!("{}-{}-{}-{}", self.set_code, seat, pack_number, i),
                name: format!("{} Card {}-{}-{}", self.set_code, seat, pack_number, i),
                set_code: self.set_code.clone(),
                collector_number: format!("{}", i + 1),
                rarity: if i == 0 {
                    "rare".to_string()
                } else if i < 4 {
                    "uncommon".to_string()
                } else {
                    "common".to_string()
                },
            })
            .collect();
        DraftPack(cards)
    }
}
