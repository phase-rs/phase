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
