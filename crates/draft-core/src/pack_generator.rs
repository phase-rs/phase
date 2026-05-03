use rand::Rng;

use crate::pack_source::PackSource;
use crate::set_pool::{LimitedSetPool, PackVariant, SheetDefinition, WeightedSheetChoice};
use crate::types::{DraftCardInstance, DraftPack};

/// Generates draft packs from a `LimitedSetPool` using weighted random selection.
/// Set-specific exceptions (bonus sheets, Mystical Archive, etc.) are expressed
/// as different sheet configurations in the pool data — no special-case code.
pub struct PackGenerator {
    pub set_pool: LimitedSetPool,
}

impl PackGenerator {
    pub fn new(set_pool: LimitedSetPool) -> Self {
        Self { set_pool }
    }

    /// Select a pack variant by weighted random from `pack_variants`.
    fn select_variant(&self, rng: &mut dyn rand::RngCore) -> &PackVariant {
        let idx = weighted_select(
            rng,
            self.set_pool.pack_variants_total_weight,
            self.set_pool
                .pack_variants
                .iter()
                .enumerate()
                .map(|(i, v)| (i, v.weight)),
        );
        &self.set_pool.pack_variants[idx]
    }

    /// Resolve which sheet name to use for a slot's choices via weighted selection.
    fn resolve_sheet_name<'a>(
        &self,
        rng: &mut dyn rand::RngCore,
        choices: &'a [WeightedSheetChoice],
    ) -> &'a str {
        if choices.len() == 1 {
            return &choices[0].sheet;
        }
        let total: u32 = choices.iter().map(|c| c.weight).sum();
        let idx = weighted_select(
            rng,
            total,
            choices.iter().enumerate().map(|(i, c)| (i, c.weight)),
        );
        &choices[idx].sheet
    }
}

impl PackSource for PackGenerator {
    fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, pack_number: u8) -> DraftPack {
        let variant = self.select_variant(rng);
        let mut cards = Vec::new();
        let mut card_index: u16 = 0;

        for slot in &variant.contents {
            let sheet_name = self.resolve_sheet_name(rng, &slot.choices);
            let sheet = match self.set_pool.sheets.get(sheet_name) {
                Some(s) => s,
                None => continue,
            };

            let indices = weighted_select_n(rng, sheet, slot.count as usize);
            for &idx in &indices {
                let card = &sheet.cards[idx];
                cards.push(DraftCardInstance {
                    instance_id: format!(
                        "{}-{}-{}-{}",
                        self.set_pool.code, seat, pack_number, card_index
                    ),
                    name: card.name.clone(),
                    set_code: card.set_code.clone(),
                    collector_number: card.collector_number.clone(),
                    rarity: format!("{:?}", card.rarity).to_lowercase(),
                });
                card_index += 1;
            }
        }

        DraftPack(cards)
    }
}

/// Select an index from a weighted distribution.
/// `weights` is an iterator of `(index, weight)` pairs.
/// `total_weight` is the precomputed sum of all weights.
fn weighted_select(
    rng: &mut dyn rand::RngCore,
    total_weight: u32,
    weights: impl Iterator<Item = (usize, u32)>,
) -> usize {
    let roll = rng.random_range(0..total_weight);
    let mut cumulative = 0u32;
    for (idx, w) in weights {
        cumulative += w;
        if roll < cumulative {
            return idx;
        }
    }
    // Fallback (should not happen with correct total_weight).
    0
}

/// Pick `count` unique indices from a sheet by weighted selection without replacement.
fn weighted_select_n(
    rng: &mut dyn rand::RngCore,
    sheet: &SheetDefinition,
    count: usize,
) -> Vec<usize> {
    let available = sheet.cards.len();
    let count = count.min(available);

    // Build mutable pool of (original_index, weight).
    let mut pool: Vec<(usize, u32)> = sheet
        .cards
        .iter()
        .enumerate()
        .map(|(i, c)| (i, c.weight))
        .collect();
    let mut total: u32 = sheet.total_weight;
    let mut result = Vec::with_capacity(count);

    for _ in 0..count {
        let roll = rng.random_range(0..total);
        let mut cumulative = 0u32;
        let mut pick_pos = 0;
        for (pos, &(_, w)) in pool.iter().enumerate() {
            cumulative += w;
            if roll < cumulative {
                pick_pos = pos;
                break;
            }
        }
        let (orig_idx, weight) = pool.swap_remove(pick_pos);
        total -= weight;
        result.push(orig_idx);
    }

    result
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, HashSet};

    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    use super::*;
    use crate::set_pool::{PackSlot, Rarity, SheetCard};

    fn make_sheet_cards(
        prefix: &str,
        set_code: &str,
        count: usize,
        rarity: Rarity,
        weight: u32,
    ) -> Vec<SheetCard> {
        (0..count)
            .map(|i| SheetCard {
                name: format!("{prefix}_{i}"),
                set_code: set_code.to_string(),
                collector_number: format!("{}", i + 1),
                rarity,
                weight,
            })
            .collect()
    }

    fn single_choice(sheet: &str) -> Vec<WeightedSheetChoice> {
        vec![WeightedSheetChoice {
            sheet: sheet.to_string(),
            weight: 1,
        }]
    }

    /// Standard test pool: 20 commons, 10 uncommons, 5 rares + 2 mythics.
    /// Single pack variant: 10 common + 3 uncommon + 1 rareMythic = 14 cards.
    fn test_pool() -> LimitedSetPool {
        let common_cards = make_sheet_cards("TST_common", "TST", 20, Rarity::Common, 1);
        let uncommon_cards = make_sheet_cards("TST_uncommon", "TST", 10, Rarity::Uncommon, 1);
        let mut rare_mythic_cards = make_sheet_cards("TST_rare", "TST", 5, Rarity::Rare, 7);
        rare_mythic_cards.extend(make_sheet_cards("TST_mythic", "TST", 2, Rarity::Mythic, 1));

        let mut sheets = BTreeMap::new();
        sheets.insert(
            "common".to_string(),
            SheetDefinition {
                total_weight: 20,
                foil: false,
                balance_colors: false,
                cards: common_cards,
            },
        );
        sheets.insert(
            "uncommon".to_string(),
            SheetDefinition {
                total_weight: 10,
                foil: false,
                balance_colors: false,
                cards: uncommon_cards,
            },
        );
        sheets.insert(
            "rareMythic".to_string(),
            SheetDefinition {
                total_weight: 37, // 5*7 + 2*1
                foil: false,
                balance_colors: false,
                cards: rare_mythic_cards,
            },
        );

        LimitedSetPool {
            code: "TST".to_string(),
            name: "Test Set".to_string(),
            release_date: None,
            pack_variants: vec![PackVariant {
                contents: vec![
                    PackSlot {
                        slot: "common".to_string(),
                        count: 10,
                        choices: single_choice("common"),
                    },
                    PackSlot {
                        slot: "uncommon".to_string(),
                        count: 3,
                        choices: single_choice("uncommon"),
                    },
                    PackSlot {
                        slot: "rareMythic".to_string(),
                        count: 1,
                        choices: single_choice("rareMythic"),
                    },
                ],
                weight: 1,
            }],
            pack_variants_total_weight: 1,
            sheets,
            prints: vec![],
            basic_lands: vec![],
        }
    }

    /// Two-variant pool: variant 1 (weight 9) is standard, variant 2 (weight 1) includes bonus sheet.
    fn two_variant_pool() -> LimitedSetPool {
        let common_cards = make_sheet_cards("TV2_common", "TV2", 15, Rarity::Common, 1);
        let uncommon_cards = make_sheet_cards("TV2_uncommon", "TV2", 8, Rarity::Uncommon, 1);
        let rare_cards = make_sheet_cards("TV2_rare", "TV2", 5, Rarity::Rare, 1);
        let bonus_cards = make_sheet_cards("BONUS_card", "STA", 3, Rarity::Rare, 1);

        let mut sheets = BTreeMap::new();
        sheets.insert(
            "common".to_string(),
            SheetDefinition {
                total_weight: 15,
                foil: false,
                balance_colors: false,
                cards: common_cards,
            },
        );
        sheets.insert(
            "uncommon".to_string(),
            SheetDefinition {
                total_weight: 8,
                foil: false,
                balance_colors: false,
                cards: uncommon_cards,
            },
        );
        sheets.insert(
            "rareMythic".to_string(),
            SheetDefinition {
                total_weight: 5,
                foil: false,
                balance_colors: false,
                cards: rare_cards,
            },
        );
        sheets.insert(
            "bonus".to_string(),
            SheetDefinition {
                total_weight: 3,
                foil: false,
                balance_colors: false,
                cards: bonus_cards,
            },
        );

        LimitedSetPool {
            code: "TV2".to_string(),
            name: "Two Variant Set".to_string(),
            release_date: None,
            pack_variants: vec![
                PackVariant {
                    contents: vec![
                        PackSlot {
                            slot: "common".to_string(),
                            count: 10,
                            choices: single_choice("common"),
                        },
                        PackSlot {
                            slot: "uncommon".to_string(),
                            count: 3,
                            choices: single_choice("uncommon"),
                        },
                        PackSlot {
                            slot: "rareMythic".to_string(),
                            count: 1,
                            choices: single_choice("rareMythic"),
                        },
                    ],
                    weight: 9,
                },
                PackVariant {
                    contents: vec![
                        PackSlot {
                            slot: "common".to_string(),
                            count: 9,
                            choices: single_choice("common"),
                        },
                        PackSlot {
                            slot: "uncommon".to_string(),
                            count: 3,
                            choices: single_choice("uncommon"),
                        },
                        PackSlot {
                            slot: "rareMythic".to_string(),
                            count: 1,
                            choices: single_choice("rareMythic"),
                        },
                        PackSlot {
                            slot: "bonus".to_string(),
                            count: 1,
                            choices: single_choice("bonus"),
                        },
                    ],
                    weight: 1,
                },
            ],
            pack_variants_total_weight: 10,
            sheets,
            prints: vec![],
            basic_lands: vec![],
        }
    }

    #[test]
    fn test_deterministic_generation() {
        let gen = PackGenerator::new(test_pool());
        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let mut rng2 = ChaCha8Rng::seed_from_u64(42);
        let pack1 = gen.generate_pack(&mut rng1, 0, 0);
        let pack2 = gen.generate_pack(&mut rng2, 0, 0);
        assert_eq!(pack1, pack2);
    }

    #[test]
    fn test_correct_pack_size() {
        let gen = PackGenerator::new(test_pool());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let pack = gen.generate_pack(&mut rng, 0, 0);
        // 10 common + 3 uncommon + 1 rareMythic = 14
        assert_eq!(pack.0.len(), 14);
    }

    #[test]
    fn test_no_duplicate_cards_in_pack() {
        let gen = PackGenerator::new(test_pool());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let pack = gen.generate_pack(&mut rng, 0, 0);
        let ids: HashSet<_> = pack.0.iter().map(|c| &c.instance_id).collect();
        assert_eq!(ids.len(), pack.0.len());
        // Also verify card names are unique (no duplicate cards from same sheet slot).
        let names: HashSet<_> = pack.0.iter().map(|c| &c.name).collect();
        assert_eq!(names.len(), pack.0.len());
    }

    #[test]
    fn test_variant_weight_distribution() {
        let gen = PackGenerator::new(two_variant_pool());
        let mut bonus_count = 0;
        let iterations = 2000;
        for seed in 0..iterations {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let pack = gen.generate_pack(&mut rng, 0, 0);
            if pack.0.iter().any(|c| c.name.starts_with("BONUS_")) {
                bonus_count += 1;
            }
        }
        // Expected ~10% = ~200, allow 100-350 for statistical stability.
        assert!(
            (100..=350).contains(&bonus_count),
            "Expected ~200 bonus packs out of {iterations}, got {bonus_count}"
        );
    }

    #[test]
    fn test_different_seats_different_packs() {
        let gen = PackGenerator::new(test_pool());
        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let mut rng2 = ChaCha8Rng::seed_from_u64(42);
        let pack_a = gen.generate_pack(&mut rng1, 0, 0);
        let pack_b = gen.generate_pack(&mut rng2, 1, 0);
        // Instance IDs differ due to seat encoding.
        assert_ne!(pack_a.0[0].instance_id, pack_b.0[0].instance_id);
    }

    #[test]
    fn test_set_code_matches() {
        let gen = PackGenerator::new(test_pool());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let pack = gen.generate_pack(&mut rng, 0, 0);
        for card in &pack.0 {
            assert_eq!(card.set_code, "TST");
        }
    }

    #[test]
    fn test_rarity_from_sheet() {
        let gen = PackGenerator::new(test_pool());
        // Generate many packs — the last card in each is from rareMythic sheet.
        for seed in 0..100u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let pack = gen.generate_pack(&mut rng, 0, 0);
            let rare_card = &pack.0[13]; // index 13 = slot 3 (rareMythic), count 1
            assert!(
                rare_card.rarity == "rare" || rare_card.rarity == "mythic",
                "Expected rare or mythic, got '{}' for card '{}'",
                rare_card.rarity,
                rare_card.name
            );
        }
    }

    #[test]
    fn test_bonus_sheet_variant() {
        let gen = PackGenerator::new(two_variant_pool());
        let mut found_bonus = false;
        for seed in 0..100u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let pack = gen.generate_pack(&mut rng, 0, 0);
            if pack.0.iter().any(|c| c.name.starts_with("BONUS_")) {
                found_bonus = true;
                break;
            }
        }
        assert!(
            found_bonus,
            "Expected at least one pack with bonus sheet card in 100 iterations"
        );
    }
}
