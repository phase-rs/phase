//! CR 400.11 + CR 400.11b: sealed booster packs as a source of cards from
//! outside the game.
//!
//! Opening a booster pack has no Comprehensive Rules entry — it is a physical
//! action the printed cards instruct a player to perform ("Open a sealed Magic
//! booster pack", Booster Tutor / Summon the Pack / A Container of Booster
//! Packs), and the cards it produces are outside the game until the effect
//! brings some of them in (CR 400.11b). This module is the digital substitute
//! for the physical pack: it stocks a per-game **shelf** of booster products
//! from the loaded card database and collates a fresh pack from one of them on
//! demand.
//!
//! # Why a shelf and not the whole corpus
//!
//! There is no `CardDatabase` at effect-resolution time — resolvers see only
//! `&mut GameState` — so any card a pack can produce must already be hydrated
//! into game state as a `CardFace`. Hydrating every printing of every set would
//! clone the entire card database (tens of thousands of faces) into every game.
//! Instead [`build_shelf`] stocks a bounded, deterministic sample of sets
//! ([`SHELF_PRODUCTS`]) with a bounded sample of each set's cards
//! ([`MAX_BUCKET`]). Packs are collated per resolution, so the number of packs a
//! game can open stays unbounded while the resident cost is proportional to the
//! shelf.
//!
//! # Rarity fidelity
//!
//! `CardFace::rarities` records every rarity a card has been printed at across
//! ALL sets, not its rarity in one particular set — the card-data export carries
//! no per-printing rarity. A card therefore stocks every bucket it has ever been
//! printed into, so a common that was later reprinted at rare can also appear in
//! a rare slot. Pack collation is otherwise the modern draft-booster skeleton
//! (ten commons, three uncommons, one rare-or-mythic), and a pack never repeats
//! a card. Recording per-printing rarity in the export would remove the
//! approximation without changing anything here but the bucketing rule.

use rand::seq::{IndexedRandom, SliceRandom};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use std::collections::BTreeMap;

use std::ops::ControlFlow;

use crate::database::card_db::CardDatabase;
use crate::types::ability::Effect;
use crate::types::ability_visit::{
    visit_ability_def, visit_replacement, visit_static, visit_trigger,
};
use crate::types::card::{CardFace, Rarity};
use crate::types::game_state::{BoosterProduct, BoosterShelf, GameState};

/// How many booster products a game stocks. Each `Effect::OpenBoosterPack`
/// resolution picks one at random, so this is the number of distinct sets a
/// single game can open packs from — not a cap on how many packs it can open.
pub const SHELF_PRODUCTS: usize = 8;

/// Maximum cards sampled into one product's rarity bucket. Caps the resident
/// cost of sets with very large card pools (compilation products such as "The
/// List" run to thousands of cards) without capping any pack's contents: every
/// slot count below is far under this bound.
pub const MAX_BUCKET: usize = 150;

/// Cards drawn from the commons bucket, matching the modern draft-booster
/// skeleton. Basic lands carry common rarity, so the land slot is naturally
/// part of this run rather than a slot of its own.
pub const COMMON_SLOTS: usize = 10;
/// Cards drawn from the uncommons bucket.
pub const UNCOMMON_SLOTS: usize = 3;
/// One in this many rare slots upgrades to a mythic rare, the printed rate for
/// modern draft boosters. Applied only when the product has mythics.
pub const MYTHIC_IN: u32 = 8;

/// Domain separator mixed into the shelf's seed so stocking the shelf consumes
/// its own deterministic stream instead of `GameState::rng`. The shelf is
/// rebuilt on every rehydrate (it is `#[serde(skip)]`); drawing from the game
/// RNG would advance that stream a different number of steps depending on how
/// many times a state had been restored.
const SHELF_SEED_SALT: u64 = 0xB005_7E12_5EA1_ED00;

/// Whether any card this game can reach carries `Effect::OpenBoosterPack`.
///
/// The gate on stocking the shelf: an ordinary game must not pay a full-corpus
/// scan, and a game that CAN open a pack must have its shelf ready before the
/// first resolution (there is no card database at resolution time). Scans the
/// same seed surface as the conjure registry — every object's printed face and
/// every deck-pool entry — through the shared `ability_visit` walkers, so a card
/// that only reaches the game from a sideboard or a companion slot still stocks
/// the shelf.
pub fn game_opens_booster_packs(state: &GameState, db: &CardDatabase) -> bool {
    let mut found = false;
    let mut visit = |effect: &Effect| {
        if matches!(effect, Effect::OpenBoosterPack { .. }) {
            found = true;
            return ControlFlow::Break(());
        }
        ControlFlow::Continue(())
    };

    for object in state.objects.values() {
        if let Some(printed_ref) = &object.printed_ref {
            if let Some(face) = db.get_face_by_printed_ref(printed_ref) {
                if face_opens_booster_packs(face, &mut visit).is_break() {
                    return true;
                }
            }
        }
    }
    for pool in &state.deck_pools {
        let entry_lists = [
            &pool.registered_main,
            &pool.registered_sideboard,
            &pool.current_main,
            &pool.current_sideboard,
            &pool.registered_companion,
            &pool.current_companion,
            &pool.registered_commander,
            &pool.current_commander,
        ];
        for entry_list in entry_lists {
            for entry in entry_list.iter() {
                if face_opens_booster_packs(&entry.card, &mut visit).is_break() {
                    return true;
                }
            }
        }
    }
    found
}

/// Run `visit` over every effect reachable from one card face's ability set.
fn face_opens_booster_packs<F>(face: &CardFace, visit: &mut F) -> ControlFlow<()>
where
    F: FnMut(&Effect) -> ControlFlow<()>,
{
    for ability in &face.abilities {
        visit_ability_def(ability, visit)?;
    }
    for trigger in &face.triggers {
        visit_trigger(trigger, visit)?;
    }
    for static_def in &face.static_abilities {
        visit_static(static_def, visit)?;
    }
    for replacement in &face.replacements {
        visit_replacement(replacement, visit)?;
    }
    ControlFlow::Continue(())
}

/// Stock a game's booster shelf from the loaded card database.
///
/// Deterministic in `(seed, db)`: the same game seed and card database always
/// produce the same shelf, so a restored or peer-rebuilt state shelves the same
/// products. Returns an empty shelf when no set in the database can fill a pack.
pub fn build_shelf(db: &CardDatabase, seed: u64) -> BoosterShelf {
    let mut rng = ChaCha20Rng::seed_from_u64(seed ^ SHELF_SEED_SALT);

    // Bucket every FRONT face by (set code, rarity). Back faces are stored in
    // the face index beside their fronts and inherit the whole card's
    // printings, so including them would deal the same physical card twice.
    let mut by_set: BTreeMap<&str, RarityBuckets<'_>> = BTreeMap::new();
    for (key, face) in db.face_iter() {
        if !db.is_front_face_key(key) {
            continue;
        }
        for set_code in db.printings_for_key(key) {
            let buckets = by_set.entry(set_code.as_str()).or_default();
            for rarity in &face.rarities {
                match rarity {
                    Rarity::Common => buckets.commons.push(face),
                    Rarity::Uncommon => buckets.uncommons.push(face),
                    Rarity::Rare => buckets.rares.push(face),
                    Rarity::Mythic => buckets.mythics.push(face),
                    // Special/bonus printings (Timeshifted sheets, box toppers,
                    // The List slots) are a print-run convention, not a CR
                    // rarity skeleton, so they fill no draft-booster slot.
                    Rarity::Special | Rarity::Bonus => {}
                }
            }
        }
    }

    // A set is a booster product only if it can actually fill every slot of a
    // pack. This is a structural test on the card pool, never a curated list of
    // set codes: a set that cannot deal ten distinct commons was not a draft
    // booster product, whatever its code.
    let mut candidates: Vec<(&str, RarityBuckets<'_>)> = by_set
        .into_iter()
        .filter(|(_, buckets)| buckets.can_fill_a_pack())
        .collect();

    // `candidates` is already in set-code order (`BTreeMap`), so the shuffle is
    // the only source of order and depends solely on the seeded stream.
    candidates.shuffle(&mut rng);
    candidates.truncate(SHELF_PRODUCTS);

    let mut products: Vec<BoosterProduct> = candidates
        .into_iter()
        .map(|(set_code, buckets)| BoosterProduct {
            set_code: set_code.to_string(),
            commons: sample_bucket(buckets.commons, &mut rng),
            uncommons: sample_bucket(buckets.uncommons, &mut rng),
            rares: sample_bucket(buckets.rares, &mut rng),
            mythics: sample_bucket(buckets.mythics, &mut rng),
        })
        .collect();
    // Deterministic shelf order regardless of the shuffle, so a product index is
    // stable for logs and tests.
    products.sort_by(|a, b| a.set_code.cmp(&b.set_code));
    BoosterShelf { products }
}

/// Collate one booster pack from `product`, drawing from `rng`.
///
/// Returns the pack in deal order (rare, then uncommons, then commons).
///
/// Deal-rare-first is load-bearing for two reasons: a card that is both this
/// product's only rare and one of its commons fills the slot that has no
/// substitute, and the AI candidate window (`SELECTION_POOL_CAP`) truncates
/// from the front — reversing for a commons-first display layout would push
/// the rare out of that window. Presentation order belongs in the modal.
/// No card appears twice: a pack is a stack of distinct physical cards, and
/// the aggregated-rarity bucketing (see the module docs) can otherwise put
/// the same card in two buckets of the same product.
pub fn collate_pack(product: &BoosterProduct, rng: &mut impl Rng) -> Vec<CardFace> {
    let mut pack: Vec<CardFace> = Vec::with_capacity(COMMON_SLOTS + UNCOMMON_SLOTS + 1);

    let rare_bucket = if !product.mythics.is_empty() && rng.random_ratio(1, MYTHIC_IN) {
        &product.mythics
    } else {
        &product.rares
    };
    deal_distinct(rare_bucket, 1, &mut pack, rng);
    deal_distinct(&product.uncommons, UNCOMMON_SLOTS, &mut pack, rng);
    deal_distinct(&product.commons, COMMON_SLOTS, &mut pack, rng);
    pack
}

/// Cards of one set grouped by the pack slot they can fill, borrowed from the
/// card database while the shelf is being stocked.
#[derive(Default)]
struct RarityBuckets<'a> {
    commons: Vec<&'a CardFace>,
    uncommons: Vec<&'a CardFace>,
    rares: Vec<&'a CardFace>,
    mythics: Vec<&'a CardFace>,
}

impl RarityBuckets<'_> {
    /// Whether this set can deal a full pack. The rare slot accepts a mythic,
    /// so the two buckets are checked together.
    fn can_fill_a_pack(&self) -> bool {
        self.commons.len() >= COMMON_SLOTS
            && self.uncommons.len() >= UNCOMMON_SLOTS
            && self.rares.len() + self.mythics.len() >= 1
    }
}

/// Hydrate up to [`MAX_BUCKET`] faces of one bucket into owned card data.
/// Buckets at or under the cap are taken whole (in database order, which
/// `face_iter` derives from a `HashMap` — so the bucket is sorted first to keep
/// the shelf independent of iteration order); larger buckets are sampled.
fn sample_bucket(mut bucket: Vec<&CardFace>, rng: &mut impl Rng) -> Vec<CardFace> {
    bucket.sort_by(|a, b| a.name.cmp(&b.name));
    if bucket.len() > MAX_BUCKET {
        bucket = bucket
            .choose_multiple(rng, MAX_BUCKET)
            .copied()
            .collect::<Vec<_>>();
        bucket.sort_by(|a, b| a.name.cmp(&b.name));
    }
    bucket.into_iter().cloned().collect()
}

/// Deal up to `count` cards from `bucket` into `pack`, skipping any card
/// already in the pack. Deals as many as the bucket can supply — a product that
/// passed [`RarityBuckets::can_fill_a_pack`] always supplies every slot, and a
/// short deal is preferable to looping forever on a degenerate bucket.
fn deal_distinct(bucket: &[CardFace], count: usize, pack: &mut Vec<CardFace>, rng: &mut impl Rng) {
    // Sample without replacement across the whole bucket, then take the first
    // `count` draws that are not already in the pack. `choose_multiple` yields
    // distinct bucket entries, so the only duplicates left to filter are
    // cross-bucket ones (a card recorded at two rarities).
    let mut dealt = 0;
    for face in bucket.choose_multiple(rng, bucket.len()) {
        if dealt == count {
            return;
        }
        if pack.iter().any(|existing| existing.name == face.name) {
            continue;
        }
        pack.push(face.clone());
        dealt += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card_type::{CardType, CoreType};
    use serde_json::json;

    /// One card-data export entry, built from a real `CardFace` so the shelf
    /// builder is exercised through the production export path
    /// (`CardDatabase::from_json_str`) rather than a hand-built index.
    /// `CardExportEntry` flattens the face, so serializing the face and adding
    /// the entry-level `printings` field yields a complete entry.
    fn entry(name: &str, printings: &[&str], rarities: &[Rarity]) -> serde_json::Value {
        let face = CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            rarities: rarities.iter().copied().collect(),
            ..Default::default()
        };
        let mut value = serde_json::to_value(&face).expect("a card face serializes");
        value["printings"] = json!(printings);
        value
    }

    /// As [`entry`], for one face of a multi-face card. `face_index` is what
    /// tells the database which face is the front.
    fn face_entry(
        name: &str,
        printings: &[&str],
        rarities: &[Rarity],
        oracle_id: &str,
        face_index: usize,
    ) -> serde_json::Value {
        let mut value = entry(name, printings, rarities);
        value["scryfall_oracle_id"] = json!(oracle_id);
        value["face_index"] = json!(face_index);
        value
    }

    fn db_from(entries: serde_json::Map<String, serde_json::Value>) -> CardDatabase {
        CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string())
            .expect("synthetic export parses")
    }

    /// A database with one set that can fill a pack (`FUL`) and one that cannot
    /// (`THN` — too few commons).
    fn db_with_one_fillable_set() -> CardDatabase {
        let mut entries = serde_json::Map::new();
        for i in 0..12 {
            entries.insert(
                format!("full common {i}"),
                entry(&format!("Full Common {i}"), &["FUL"], &[Rarity::Common]),
            );
        }
        for i in 0..5 {
            entries.insert(
                format!("full uncommon {i}"),
                entry(&format!("Full Uncommon {i}"), &["FUL"], &[Rarity::Uncommon]),
            );
        }
        entries.insert(
            "full rare".to_string(),
            entry("Full Rare", &["FUL"], &[Rarity::Rare]),
        );
        for i in 0..3 {
            entries.insert(
                format!("thin common {i}"),
                entry(&format!("Thin Common {i}"), &["THN"], &[Rarity::Common]),
            );
        }
        entries.insert(
            "thin rare".to_string(),
            entry("Thin Rare", &["THN"], &[Rarity::Rare]),
        );
        db_from(entries)
    }

    /// A set becomes a booster product only if its card pool can actually fill
    /// every slot of a pack. This is a structural test on the pool — never a
    /// curated list of set codes.
    #[test]
    fn only_sets_that_can_fill_a_pack_are_shelved() {
        let shelf = build_shelf(&db_with_one_fillable_set(), 7);
        let codes: Vec<&str> = shelf
            .products
            .iter()
            .map(|product| product.set_code.as_str())
            .collect();
        assert_eq!(codes, vec!["FUL"], "THN cannot deal ten distinct commons");
    }

    /// `booster_shelf` is `#[serde(skip)]` and rebuilt on every rehydrate, so
    /// two builds from the same seed and database must shelve the same products
    /// with the same cards — otherwise a restore or a peer rebuild would shelve
    /// a different game.
    #[test]
    fn shelf_is_deterministic_in_seed_and_database() {
        let db = db_with_one_fillable_set();
        assert_eq!(build_shelf(&db, 42), build_shelf(&db, 42));
    }

    /// A pack is a stack of distinct physical cards, dealt to the modern
    /// draft-booster skeleton. The rare is first so the AI candidate window
    /// includes it (`SELECTION_POOL_CAP` is 12; a reversed 14-card pack would
    /// drop the rare).
    #[test]
    fn a_collated_pack_is_the_full_skeleton_with_no_repeats() {
        let db = db_with_one_fillable_set();
        let shelf = build_shelf(&db, 3);
        let product = &shelf.products[0];
        let mut rng = ChaCha20Rng::seed_from_u64(11);
        let pack = collate_pack(product, &mut rng);

        assert_eq!(pack[0].name, "Full Rare");
        assert_eq!(pack.len(), COMMON_SLOTS + UNCOMMON_SLOTS + 1);
        let distinct: std::collections::BTreeSet<&str> =
            pack.iter().map(|face| face.name.as_str()).collect();
        assert_eq!(
            distinct.len(),
            pack.len(),
            "no card appears twice: {pack:?}"
        );
        assert!(
            pack.iter()
                .all(|face| face.card_type.core_types.contains(&CoreType::Creature)),
            "pack cards carry their printed characteristics"
        );
    }

    /// A back face is stored in the face index beside its front and inherits the
    /// whole card's printings, so shelving it would deal one physical card
    /// twice — once per face.
    #[test]
    fn back_faces_are_not_shelved() {
        let mut entries = serde_json::Map::new();
        for i in 0..12 {
            entries.insert(
                format!("dfc common {i}"),
                face_entry(
                    &format!("DFC Common {i}"),
                    &["DFC"],
                    &[Rarity::Common],
                    &format!("oracle-{i}"),
                    0,
                ),
            );
            entries.insert(
                format!("dfc common {i} back"),
                face_entry(
                    &format!("DFC Common {i} Back"),
                    &["DFC"],
                    &[Rarity::Common],
                    &format!("oracle-{i}"),
                    1,
                ),
            );
        }
        for i in 0..5 {
            entries.insert(
                format!("dfc uncommon {i}"),
                entry(&format!("DFC Uncommon {i}"), &["DFC"], &[Rarity::Uncommon]),
            );
        }
        entries.insert(
            "dfc rare".to_string(),
            entry("DFC Rare", &["DFC"], &[Rarity::Rare]),
        );

        let shelf = build_shelf(&db_from(entries), 5);
        let product = shelf
            .products
            .iter()
            .find(|product| product.set_code == "DFC")
            .expect("DFC can fill a pack from its twelve front-face commons");
        assert!(
            product
                .commons
                .iter()
                .all(|face| !face.name.ends_with("Back")),
            "back faces must not stock a bucket: {:?}",
            product
                .commons
                .iter()
                .map(|face| face.name.as_str())
                .collect::<Vec<_>>()
        );
    }
}
