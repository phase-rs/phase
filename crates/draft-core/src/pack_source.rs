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

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn fixture(cards_per_pack: u8) -> FixturePackSource {
        FixturePackSource {
            set_code: "TST".to_string(),
            cards_per_pack,
        }
    }

    #[test]
    fn generates_correct_number_of_cards() {
        let source = fixture(14);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let pack = source.generate_pack(&mut rng, 0, 0);
        assert_eq!(pack.0.len(), 14);
    }

    #[test]
    fn same_seed_same_seat_same_pack_is_deterministic() {
        let source = fixture(14);
        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let mut rng2 = ChaCha8Rng::seed_from_u64(42);
        let pack1 = source.generate_pack(&mut rng1, 3, 1);
        let pack2 = source.generate_pack(&mut rng2, 3, 1);
        assert_eq!(pack1, pack2);
    }

    #[test]
    fn different_seats_produce_different_packs() {
        let source = fixture(14);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let pack_a = source.generate_pack(&mut rng, 0, 0);
        let pack_b = source.generate_pack(&mut rng, 1, 0);
        // Different seats produce different instance IDs
        assert_ne!(pack_a.0[0].instance_id, pack_b.0[0].instance_id);
    }

    #[test]
    fn card_naming_convention() {
        let source = fixture(3);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let pack = source.generate_pack(&mut rng, 2, 1);
        assert_eq!(pack.0[0].instance_id, "TST-2-1-0");
        assert_eq!(pack.0[0].name, "TST Card 2-1-0");
        assert_eq!(pack.0[0].set_code, "TST");
        assert_eq!(pack.0[0].rarity, "rare");
        assert_eq!(pack.0[1].rarity, "uncommon");
        assert_eq!(pack.0[2].rarity, "uncommon");
    }
}
