use std::collections::HashSet;

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use crate::pack_source::PackSource;
use crate::set_pool::{
    LimitedSetPool, PackVariant, SheetCard, SheetDefinition, WeightedSheetChoice,
};
use crate::types::{entry_for_pack, DraftCardInstance, DraftError, DraftPack};

/// Generates draft packs from `LimitedSetPool`s using weighted random selection.
/// Set-specific exceptions (bonus sheets, Mystical Archive, etc.) are expressed
/// as different sheet configurations in the pool data — no special-case code.
///
/// A draft opens one booster per seat and pack round. Uniform layouts select
/// one set per round; Chaos layouts persist one set per `(seat, round)`. The
/// generator therefore holds distinct pools plus resolved pool indexes.
pub struct PackGenerator {
    /// Distinct set pools this generator can draw from.
    pools: Vec<LimitedSetPool>,
    layout: GeneratorLayout,
}

/// Pool indexes rather than codes keep generation's hot path to a direct
/// lookup while [`PackGenerator`] construction remains the one boundary that
/// resolves pool identities.
enum GeneratorLayout {
    /// Every seat opens the same set in a round; a short sequence repeats its
    /// last entry for the legacy single-set shorthand.
    UniformByRound(Vec<usize>),
    /// Exact `(seat, round)` pool indexes persisted by `SetLayout::Chaos`.
    Chaos(Vec<Vec<usize>>),
}

impl PackGenerator {
    /// A generator whose every booster comes from one set.
    pub fn new(set_pool: LimitedSetPool) -> Self {
        Self {
            pools: vec![set_pool],
            layout: GeneratorLayout::UniformByRound(vec![0]),
        }
    }

    /// A generator that opens one booster per entry of `sequence`, in pack
    /// order. `pools` supplies the distinct sets; `sequence` names which of
    /// them fills each booster, so the same set may appear more than once.
    pub fn for_sequence(
        pools: Vec<LimitedSetPool>,
        sequence: &[String],
    ) -> Result<Self, DraftError> {
        if sequence.is_empty() {
            return Err(DraftError::InvalidPackSequence {
                reason: "a draft must open at least one pack".to_string(),
            });
        }
        let resolved = sequence
            .iter()
            .map(|code| {
                pools
                    .iter()
                    .position(|pool| pool.code.eq_ignore_ascii_case(code))
                    .ok_or_else(|| DraftError::InvalidPackSequence {
                        reason: format!("no pool data was supplied for set '{code}'"),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            pools,
            layout: GeneratorLayout::UniformByRound(resolved),
        })
    }

    /// Deterministically choose one candidate set for every `(seat, round)`.
    /// Assignment uses its own seeded stream so changing card collation cannot
    /// change which sets an already-created Chaos pod contained.
    pub fn chaos_assignments(
        candidate_codes: &[String],
        seat_count: u8,
        pack_count: u8,
        rng_seed: u64,
    ) -> Result<Vec<Vec<String>>, DraftError> {
        if candidate_codes.is_empty() {
            return Err(DraftError::InvalidPackSequence {
                reason: "a Chaos draft must name at least one candidate set".to_string(),
            });
        }
        let mut rng = ChaCha20Rng::seed_from_u64(rng_seed);
        Ok((0..seat_count)
            .map(|_| {
                (0..pack_count)
                    .map(|_| candidate_codes[rng.random_range(0..candidate_codes.len())].clone())
                    .collect()
            })
            .collect())
    }

    /// A Chaos generator indexed by its persisted `(seat, round)` assignment.
    /// The candidate list is checked even when a deterministic draw happened
    /// not to select one of its entries, so no selectable pool can evade
    /// preflight at the construction boundary.
    pub fn for_chaos(
        pools: Vec<LimitedSetPool>,
        candidate_codes: &[String],
        assignments: &[Vec<String>],
        seat_count: u8,
        pack_count: u8,
    ) -> Result<Self, DraftError> {
        if candidate_codes.is_empty() {
            return Err(DraftError::InvalidPackSequence {
                reason: "a Chaos draft must name at least one candidate set".to_string(),
            });
        }
        if assignments.len() != usize::from(seat_count)
            || assignments
                .iter()
                .any(|rounds| rounds.len() != usize::from(pack_count))
        {
            return Err(DraftError::InvalidPackSequence {
                reason: "Chaos assignments must be exact seat-by-round entries".to_string(),
            });
        }

        let pool_index = |code: &str| {
            pools
                .iter()
                .position(|pool| pool.code.eq_ignore_ascii_case(code))
                .ok_or_else(|| DraftError::InvalidPackSequence {
                    reason: format!("no pool data was supplied for set '{code}'"),
                })
        };
        let mut declared_pack_size = None;
        for candidate in candidate_codes {
            let index = pool_index(candidate)?;
            let size = pools[index].cards_per_pack().ok_or_else(|| {
                DraftError::InvalidPackSequence {
                    reason: format!(
                        "Chaos candidate set '{}' has no single MTGJSON pack size across its booster variants",
                        pools[index].code
                    ),
                }
            })?;
            if let Some(expected) = declared_pack_size {
                if expected != size {
                    return Err(DraftError::InvalidPackSequence {
                        reason: format!(
                            "Chaos candidate sets must share one MTGJSON pack size; {} has {size}, expected {expected}",
                            pools[index].code
                        ),
                    });
                }
            } else {
                declared_pack_size = Some(size);
            }
        }
        let resolved = assignments
            .iter()
            .enumerate()
            .map(|(seat, rounds)| {
                rounds
                    .iter()
                    .map(|code| {
                        if !candidate_codes
                            .iter()
                            .any(|candidate| candidate.eq_ignore_ascii_case(code))
                        {
                            return Err(DraftError::InvalidPackSequence {
                                reason: format!(
                                    "Chaos assignment '{code}' for seat {seat} is not a candidate set"
                                ),
                            });
                        }
                        pool_index(code)
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self {
            pools,
            layout: GeneratorLayout::Chaos(resolved),
        })
    }

    /// The pool filling this seat's booster for `pack_number`.
    fn pool_for_pack(&self, seat: u8, pack_number: u8) -> &LimitedSetPool {
        let index = match &self.layout {
            GeneratorLayout::UniformByRound(sequence) => entry_for_pack(sequence, pack_number)
                .copied()
                .expect("PackGenerator is constructed with a non-empty sequence"),
            GeneratorLayout::Chaos(assignments) => assignments
                .get(usize::from(seat))
                .and_then(|rounds| rounds.get(usize::from(pack_number)))
                .copied()
                .expect("PackGenerator validates exact Chaos assignment dimensions"),
        };
        &self.pools[index]
    }

    /// Select a pack variant by weighted random from `pack_variants`.
    fn select_variant<'a>(
        &self,
        rng: &mut dyn rand::RngCore,
        pool: &'a LimitedSetPool,
    ) -> &'a PackVariant {
        let idx = weighted_select(
            rng,
            u64::from(pool.pack_variants_total_weight),
            pool.pack_variants
                .iter()
                .enumerate()
                .map(|(i, v)| (i, u64::from(v.weight))),
        );
        &pool.pack_variants[idx]
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
        let total: u64 = choices.iter().map(|c| u64::from(c.weight)).sum();
        let idx = weighted_select(
            rng,
            total,
            choices
                .iter()
                .enumerate()
                .map(|(i, c)| (i, u64::from(c.weight))),
        );
        &choices[idx].sheet
    }

    /// Largest non-foil, non-empty sheet referenced by `variant` — the sheet to
    /// pull a replacement card from when another slot's sheet came up short.
    fn backfill_sheet<'a>(
        &self,
        pool: &'a LimitedSetPool,
        variant: &PackVariant,
    ) -> Option<&'a SheetDefinition> {
        variant
            .contents
            .iter()
            .flat_map(|slot| slot.choices.iter())
            .filter_map(|choice| pool.sheets.get(&choice.sheet))
            .filter(|sheet| !sheet.foil && !sheet.cards.is_empty())
            .max_by_key(|sheet| sheet.cards.len())
    }
}

impl PackSource for PackGenerator {
    fn generate_pack(&self, rng: &mut dyn rand::RngCore, seat: u8, pack_number: u8) -> DraftPack {
        let pool = self.pool_for_pack(seat, pack_number);
        let variant = self.select_variant(rng, pool);

        // A booster always contains a fixed number of cards. Some sheets a variant
        // references — e.g. "specialGuest", "theList", "mysticalArchive" — resolve
        // to zero cards when their printings aren't part of this set's MTGJSON
        // export; in real boosters that slot is just an extra card off the main
        // sheet. Track the declared size and top up any shortfall so packs stay
        // uniform — an unequal pack stalls the pick/pass rotation (every seat must
        // exhaust its pack on the same pick).
        let target_size: usize = variant.contents.iter().map(|s| s.count as usize).sum();

        let mut picks: Vec<&SheetCard> = Vec::with_capacity(target_size);
        for slot in &variant.contents {
            let sheet_name = self.resolve_sheet_name(rng, &slot.choices);
            let Some(sheet) = pool.sheets.get(sheet_name) else {
                continue;
            };
            for idx in select_sheet_cards(rng, sheet, slot.count as usize) {
                picks.push(&sheet.cards[idx]);
            }
        }

        if picks.len() < target_size {
            // `backfill_sheet` returns `None` only if the variant references no
            // non-foil non-empty sheet at all — impossible for real boosters
            // (every variant has a `common` slot whose printings live in the
            // set's own file). If it ever happened, the pack would stay short;
            // the `draft-wasm` empty-pack `continue` is the bot-side backstop.
            if let Some(sheet) = self.backfill_sheet(pool, variant) {
                let shortfall = target_size - picks.len();
                let extra = {
                    let used: HashSet<(&str, &str)> = picks
                        .iter()
                        .map(|c| (c.set_code.as_str(), c.collector_number.as_str()))
                        .collect();
                    weighted_select_n_excluding(rng, sheet, shortfall, &used)
                };
                for idx in extra {
                    picks.push(&sheet.cards[idx]);
                }
            }
        }

        let cards = picks
            .into_iter()
            .enumerate()
            .map(|(card_index, card)| DraftCardInstance {
                instance_id: format!("{}-{}-{}-{}", pool.code, seat, pack_number, card_index),
                name: card.name.clone(),
                set_code: card.set_code.clone(),
                collector_number: card.collector_number.clone(),
                rarity: format!("{:?}", card.rarity).to_lowercase(),
                colors: card.colors.clone(),
                cmc: card.cmc,
                type_line: card.type_line.clone(),
                draft_effect: card.draft_effect,
            })
            .collect();

        DraftPack(cards)
    }
}

/// Select an index from a weighted distribution.
/// `weights` is an iterator of `(index, weight)` pairs.
/// `total_weight` is the precomputed sum of all weights.
fn weighted_select(
    rng: &mut dyn rand::RngCore,
    total_weight: u64,
    weights: impl Iterator<Item = (usize, u64)>,
) -> usize {
    let roll = rng.random_range(0..total_weight);
    let mut cumulative = 0u64;
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
    weighted_select_n_excluding(rng, sheet, count, &HashSet::new())
}

/// Select card positions using the collation behavior MTGJSON declares for a
/// sheet. Fixed sheets emit every weighted position; duplicate-permitting
/// sheets draw each slot independently; all other sheets draw distinct cards.
fn select_sheet_cards(
    rng: &mut dyn rand::RngCore,
    sheet: &SheetDefinition,
    count: usize,
) -> Vec<usize> {
    if sheet.fixed {
        return sheet
            .cards
            .iter()
            .enumerate()
            .flat_map(|(index, card)| std::iter::repeat_n(index, card.weight as usize))
            .collect();
    }

    if sheet.allow_duplicates {
        return (0..count)
            .map(|_| {
                weighted_select(
                    rng,
                    sheet.total_weight,
                    sheet
                        .cards
                        .iter()
                        .enumerate()
                        .map(|(i, card)| (i, card.weight)),
                )
            })
            .collect();
    }

    weighted_select_n(rng, sheet, count)
}

/// Like [`weighted_select_n`], but skips any card whose `(set_code, collector_number)`
/// identity is in `exclude` — used to top up a short pack without duplicating a
/// printing already in it.
fn weighted_select_n_excluding(
    rng: &mut dyn rand::RngCore,
    sheet: &SheetDefinition,
    count: usize,
    exclude: &HashSet<(&str, &str)>,
) -> Vec<usize> {
    // Build mutable pool of (original_index, weight), dropping excluded cards.
    let mut pool: Vec<(usize, u64)> = sheet
        .cards
        .iter()
        .enumerate()
        .filter(|(_, c)| !exclude.contains(&(c.set_code.as_str(), c.collector_number.as_str())))
        .map(|(i, c)| (i, c.weight))
        .collect();
    let count = count.min(pool.len());
    let mut total: u64 = pool.iter().map(|&(_, w)| w).sum();
    let mut result = Vec::with_capacity(count);

    for _ in 0..count {
        let roll = rng.random_range(0..total);
        let mut cumulative = 0u64;
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
        weight: u64,
    ) -> Vec<SheetCard> {
        (0..count)
            .map(|i| SheetCard {
                name: format!("{prefix}_{i}"),
                set_code: set_code.to_string(),
                collector_number: format!("{}", i + 1),
                rarity,
                weight,
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
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
                ..Default::default()
            },
        );
        sheets.insert(
            "uncommon".to_string(),
            SheetDefinition {
                total_weight: 10,
                foil: false,
                balance_colors: false,
                cards: uncommon_cards,
                ..Default::default()
            },
        );
        sheets.insert(
            "rareMythic".to_string(),
            SheetDefinition {
                total_weight: 37, // 5*7 + 2*1
                foil: false,
                balance_colors: false,
                cards: rare_mythic_cards,
                ..Default::default()
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

    /// A one-sheet pool of `size` commons under `code` — enough to prove which
    /// set filled a given pack, and to give a multi-set sequence packs of
    /// genuinely different sizes.
    fn sized_pool(code: &str, size: u8) -> LimitedSetPool {
        let mut sheets = BTreeMap::new();
        sheets.insert(
            "common".to_string(),
            SheetDefinition {
                total_weight: 40,
                foil: false,
                balance_colors: false,
                cards: make_sheet_cards(&format!("{code}_common"), code, 40, Rarity::Common, 1),
                ..Default::default()
            },
        );
        LimitedSetPool {
            code: code.to_string(),
            name: format!("{code} Set"),
            release_date: None,
            pack_variants: vec![PackVariant {
                contents: vec![PackSlot {
                    slot: "common".to_string(),
                    count: size,
                    choices: single_choice("common"),
                }],
                weight: 1,
            }],
            pack_variants_total_weight: 1,
            sheets,
            prints: vec![],
            basic_lands: vec![],
        }
    }

    #[test]
    fn a_pack_sequence_draws_each_pack_from_the_set_that_names_it() {
        let generator = PackGenerator::for_sequence(
            vec![sized_pool("AAA", 15), sized_pool("BBB", 14)],
            &["AAA".to_string(), "BBB".to_string(), "AAA".to_string()],
        )
        .expect("every named set has pool data");
        let mut rng = ChaCha8Rng::seed_from_u64(7);

        let packs: Vec<DraftPack> = (0..3)
            .map(|pack| generator.generate_pack(&mut rng, 0, pack))
            .collect();

        let sets: Vec<&str> = packs
            .iter()
            .map(|pack| pack.0[0].set_code.as_str())
            .collect();
        assert_eq!(sets, ["AAA", "BBB", "AAA"]);
        // A multi-set draft mixes MTGJSON booster sizes.
        assert_eq!(
            packs.iter().map(|pack| pack.0.len()).collect::<Vec<_>>(),
            [15, 14, 15]
        );
    }

    #[test]
    fn chaos_assignments_are_deterministic_and_replayable() {
        let candidates = vec!["AAA".to_string(), "BBB".to_string(), "CCC".to_string()];
        let first = PackGenerator::chaos_assignments(&candidates, 4, 3, 99).unwrap();
        let replay = PackGenerator::chaos_assignments(&candidates, 4, 3, 99).unwrap();

        assert_eq!(first, replay);
        assert_eq!(first.len(), 4);
        assert!(first.iter().all(|rounds| rounds.len() == 3));
        assert!(first.iter().flatten().all(|code| candidates.contains(code)));
    }

    #[test]
    fn chaos_generator_uses_the_exact_seat_and_round_assignment() {
        let assignments = vec![
            vec!["AAA".to_string(), "BBB".to_string()],
            vec!["BBB".to_string(), "AAA".to_string()],
        ];
        let generator = PackGenerator::for_chaos(
            vec![sized_pool("AAA", 15), sized_pool("BBB", 15)],
            &["AAA".to_string(), "BBB".to_string()],
            &assignments,
            2,
            2,
        )
        .unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(7);

        assert_eq!(generator.generate_pack(&mut rng, 0, 0).0[0].set_code, "AAA");
        assert_eq!(generator.generate_pack(&mut rng, 0, 1).0[0].set_code, "BBB");
        assert_eq!(generator.generate_pack(&mut rng, 1, 0).0[0].set_code, "BBB");
        assert_eq!(generator.generate_pack(&mut rng, 1, 1).0[0].set_code, "AAA");
    }

    #[test]
    fn a_single_set_generator_fills_every_pack_from_its_one_pool() {
        let generator = PackGenerator::new(sized_pool("AAA", 15));
        let mut rng = ChaCha8Rng::seed_from_u64(7);

        // Pack 5 is far past the one-element sequence: repeat-last resolves it.
        for pack in [0u8, 1, 2, 5] {
            let generated = generator.generate_pack(&mut rng, 0, pack);
            assert_eq!(generated.0[0].set_code, "AAA");
            assert_eq!(generated.0.len(), 15);
        }
    }

    #[test]
    fn a_pack_sequence_naming_an_unsupplied_set_is_rejected() {
        let error = PackGenerator::for_sequence(
            vec![sized_pool("AAA", 15)],
            &["AAA".to_string(), "MISSING".to_string()],
        )
        .err()
        .expect("the sequence names a set with no pool");

        assert!(
            matches!(&error, DraftError::InvalidPackSequence { reason } if reason.contains("MISSING")),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn an_empty_pack_sequence_is_rejected() {
        assert!(matches!(
            PackGenerator::for_sequence(vec![sized_pool("AAA", 15)], &[]).err(),
            Some(DraftError::InvalidPackSequence { .. })
        ));
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
                ..Default::default()
            },
        );
        sheets.insert(
            "uncommon".to_string(),
            SheetDefinition {
                total_weight: 8,
                foil: false,
                balance_colors: false,
                cards: uncommon_cards,
                ..Default::default()
            },
        );
        sheets.insert(
            "rareMythic".to_string(),
            SheetDefinition {
                total_weight: 5,
                foil: false,
                balance_colors: false,
                cards: rare_cards,
                ..Default::default()
            },
        );
        sheets.insert(
            "bonus".to_string(),
            SheetDefinition {
                total_weight: 3,
                foil: false,
                balance_colors: false,
                cards: bonus_cards,
                ..Default::default()
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

    /// DFT-shaped two-variant pool where the special slot's sheet resolved to
    /// zero cards (its printings live in another set's MTGJSON file). Mirrors the
    /// real "common count drops by 1 when the special slot is present" balance:
    /// variant A = 13 common + 1 rare (14, no special); variant B = 12 common +
    /// 1 rare + 1 specialGuest (14 declared, but specialGuest is empty so it
    /// produces 13 — the generator must backfill it to 14). A and B must be
    /// indistinguishable in size, exactly like Arena (a no-Special-Guest pack and
    /// a backfilled one are both 14, 13 of them commons).
    fn empty_special_sheet_pool() -> LimitedSetPool {
        let common_cards = make_sheet_cards("ESS_common", "ESS", 30, Rarity::Common, 1);
        let rare_cards = make_sheet_cards("ESS_rare", "ESS", 5, Rarity::Rare, 1);

        let mut sheets = BTreeMap::new();
        sheets.insert(
            "common".to_string(),
            SheetDefinition {
                total_weight: 30,
                foil: false,
                balance_colors: false,
                cards: common_cards,
                ..Default::default()
            },
        );
        sheets.insert(
            "rareMythic".to_string(),
            SheetDefinition {
                total_weight: 5,
                foil: false,
                balance_colors: false,
                cards: rare_cards,
                ..Default::default()
            },
        );
        // The empty sheet — present in the data, but no cards survived extraction.
        sheets.insert(
            "specialGuest".to_string(),
            SheetDefinition {
                total_weight: 0,
                foil: false,
                balance_colors: false,
                cards: vec![],
                ..Default::default()
            },
        );

        let common_slot = |count| PackSlot {
            slot: "common".to_string(),
            count,
            choices: single_choice("common"),
        };
        let rare_slot = PackSlot {
            slot: "rareMythic".to_string(),
            count: 1,
            choices: single_choice("rareMythic"),
        };

        LimitedSetPool {
            code: "ESS".to_string(),
            name: "Empty Special Sheet Set".to_string(),
            release_date: None,
            pack_variants: vec![
                PackVariant {
                    contents: vec![common_slot(13), rare_slot.clone()],
                    weight: 3,
                },
                PackVariant {
                    contents: vec![
                        common_slot(12),
                        rare_slot,
                        PackSlot {
                            slot: "specialGuest".to_string(),
                            count: 1,
                            choices: single_choice("specialGuest"),
                        },
                    ],
                    weight: 1,
                },
            ],
            pack_variants_total_weight: 4,
            sheets,
            prints: vec![],
            basic_lands: vec![],
        }
    }

    #[test]
    fn test_empty_sheet_slot_is_backfilled() {
        let gen = PackGenerator::new(empty_special_sheet_pool());
        // Variant B (specialGuest slot) is weight 1/4, so ~20 of 80 seeds roll it
        // — its backfill path is exercised even though the result is, by design,
        // indistinguishable from variant A's pack.
        for seed in 0..80u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let pack = gen.generate_pack(&mut rng, 0, 0);
            assert_eq!(pack.0.len(), 14, "seed {seed}: pack size");
            // 13 commons + 1 rare regardless of which variant was rolled — a
            // broken backfill on variant B would leave 12 commons (and a 13-card
            // pack, caught above).
            let common_count = pack.0.iter().filter(|c| c.rarity == "common").count();
            assert_eq!(common_count, 13, "seed {seed}: common count");
            let names: HashSet<_> = pack.0.iter().map(|c| &c.name).collect();
            assert_eq!(names.len(), pack.0.len(), "seed {seed}: duplicate card");
            let instance_ids: HashSet<_> = pack.0.iter().map(|c| &c.instance_id).collect();
            assert_eq!(
                instance_ids.len(),
                pack.0.len(),
                "seed {seed}: duplicate id"
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

    #[test]
    fn fixed_sheet_emits_every_weighted_card_position() {
        let sheet = SheetDefinition {
            cards: vec![
                SheetCard {
                    name: "Repeated".to_string(),
                    set_code: "TST".to_string(),
                    collector_number: "1".to_string(),
                    rarity: Rarity::Common,
                    weight: 2,
                    colors: vec![],
                    cmc: 0,
                    type_line: String::new(),
                    draft_effect: None,
                },
                SheetCard {
                    name: "Single".to_string(),
                    set_code: "TST".to_string(),
                    collector_number: "2".to_string(),
                    rarity: Rarity::Common,
                    weight: 1,
                    colors: vec![],
                    cmc: 0,
                    type_line: String::new(),
                    draft_effect: None,
                },
            ],
            total_weight: 3,
            fixed: true,
            ..Default::default()
        };
        let mut rng = ChaCha8Rng::seed_from_u64(1);

        assert_eq!(select_sheet_cards(&mut rng, &sheet, 3), vec![0, 0, 1]);
    }

    #[test]
    fn duplicate_permitting_sheet_can_repeat_a_card() {
        let sheet = SheetDefinition {
            cards: vec![SheetCard {
                name: "Only Card".to_string(),
                set_code: "TST".to_string(),
                collector_number: "1".to_string(),
                rarity: Rarity::Common,
                weight: 1,
                colors: vec![],
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            }],
            total_weight: 1,
            allow_duplicates: true,
            ..Default::default()
        };
        let mut rng = ChaCha8Rng::seed_from_u64(1);

        assert_eq!(select_sheet_cards(&mut rng, &sheet, 2), vec![0, 0]);
    }
}
