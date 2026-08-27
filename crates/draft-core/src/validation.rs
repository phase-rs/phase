use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// Deep-path import by design: `engine::game::mod` re-exports `deck_validation`'s
// public surface, but this phase must not edit that file.
use engine::game::deck_validation::GrantableCommanderFiller;

use crate::types::DeckAddableCards;

/// Standard basic land names that are always available in unlimited quantity.
/// CR 100.2a: basic lands are exempt from copy limits. All cards with the
/// Basic supertype are listed here (five originals, Wastes, and all
/// Snow-Covered variants).
pub const STANDARD_BASIC_LANDS: &[&str] = &[
    "Plains",
    "Island",
    "Swamp",
    "Mountain",
    "Forest",
    "Wastes",
    "Snow-Covered Plains",
    "Snow-Covered Island",
    "Snow-Covered Swamp",
    "Snow-Covered Mountain",
    "Snow-Covered Forest",
];

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum LimitedDeckError {
    #[error("deck has {actual} cards, minimum is {minimum}")]
    TooFewCards { actual: usize, minimum: usize },
    #[error("card '{name}' is not in the drafted pool")]
    NotInPool { name: String },
    #[error("card '{name}' used {requested} times but only {available} in pool")]
    ExceedsPoolCount {
        name: String,
        requested: u32,
        available: u32,
    },
    /// CR 903.13e: the drafted cards ARE the pool; the cap applies only to the
    /// copies a player *adds* on top of it, so `pooled + granted` is the legal
    /// total. `pooled` and `added` are carried separately because a shape that
    /// carried only a combined total could not tell a drafted copy from an
    /// added one -- and any rule written against that shape rejects a legal
    /// deck, since these cards are printed in the granting sets' own boosters.
    #[error(
        "card '{name}': {pooled} drafted and {added} added, but at most {granted} may be added"
    )]
    FillerExceedsGrant {
        name: String,
        pooled: u32,
        added: u32,
        granted: u32,
    },
    /// CR 903.13e: "but only if those cards are used as the player's
    /// commander(s)" -- the condition attaches to the ADDED copies, never to
    /// drafted ones.
    #[error("card '{name}': {added} copy(ies) added beyond the {pooled} drafted, but only {designated} designated as commander(s)")]
    FillerNotUsedAsCommander {
        name: String,
        pooled: u32,
        added: u32,
        designated: u32,
    },
    /// CR 702.124h: "You may designate two legendary CARDS as your commander
    /// rather than one if each of them has partner." Two commanders are two
    /// CARDS, so a name designated N times must be backed by N copies in the
    /// deck. This is MULTIPLICITY, not membership: the counts live in the type
    /// precisely so a consumer cannot collapse the rule back into a set test.
    #[error("card '{name}' is designated as commander {designated} time(s) but the deck contains {in_deck} copy(ies)")]
    CommanderNotInDeck {
        name: String,
        designated: u32,
        in_deck: u32,
    },
    /// CR 702.124g: "no partner ability or combination of partner abilities can
    /// ever let a player have more than two commanders." Checked on the
    /// submitted list alone -- it needs neither the deck nor the pool, so
    /// `apply_submit_deck` raises it rather than this function.
    #[error("{designated} commanders designated, but at most {maximum} are allowed")]
    TooManyCommanders { designated: usize, maximum: usize },
    /// CR 903.3: "Each deck has a legendary card designated as its commander."
    /// The FLOOR to `TooManyCommanders`'s CR 702.124g cap. The two are not one
    /// comparator-parameterized rule: they are raised by different authorities
    /// (the cap by `apply_submit_deck`, on the payload alone; this floor inside
    /// `validate_limited_deck`) and under different control-flow regimes (the
    /// cap early-`return`s; this one PUSHES, so it cannot mask
    /// `CommanderNotInDeck`).
    #[error("{designated} commander(s) designated, but {minimum} are required")]
    TooFewCommanders { designated: usize, minimum: usize },
}

/// Validate a Limited deck against the drafted pool.
///
/// Rules (per MTG Limited):
/// - CR 100.2b: main deck must have at least `min_deck_size` cards (default 40)
/// - All non-basic cards must be present in the pool with sufficient copies
/// - Basic lands (from `basic_land_names`) are available in unlimited quantity
/// - No constructed legality check, no 4-copy limit
/// - CR 903.13e: a `granted_filler` name may be used up to `max_copies` times
///   ABOVE the pool count, and only if those added copies are designated as
///   commanders
/// - CR 702.124h: every name in `commanders` must be backed by at least as many
///   copies in `main_deck` as it is designated times
/// - CR 903.3: at least `commanders_required` names must be designated
///
/// Name comparison is exact `==` throughout. Every key here -- the pool
/// multiset, the deck multiset, the designation multiset and the
/// `granted_filler` match -- is the raw submitted string, and draft-core's
/// strings all originate from `DraftCardInstance.name`, so casing is uniform
/// within a session and exact equality is achievable rather than merely strict.
/// A case-insensitive filler match against a case-sensitive pool lookup would
/// let a differently-cased name take the grant while its pooled copies stayed
/// invisible.
///
/// Returns Ok(()) on success, Err with all accumulated errors on failure.
pub fn validate_limited_deck(
    main_deck: &[String],
    pool: &[String],
    addable_cards: &DeckAddableCards,
    min_deck_size: usize,
    // CR 903.13e: the filler this session's booster set grants, if any.
    granted_filler: Option<&GrantableCommanderFiller>,
    // CR 903.13e: "but only if those cards are used as the player's commander(s)".
    commanders: &[String],
    // CR 903.3: how many commanders a deck built from this pool must designate.
    // `0` for the four CR 905.1a kinds. Supplied by the caller, never derived
    // here — the validator stays kind-agnostic.
    commanders_required: usize,
) -> Result<(), Vec<LimitedDeckError>> {
    let mut errors = Vec::new();

    // 1. Check minimum deck size
    if main_deck.len() < min_deck_size {
        errors.push(LimitedDeckError::TooFewCards {
            actual: main_deck.len(),
            minimum: min_deck_size,
        });
    }

    // 2. Build pool multiset (card name -> available count)
    let mut pool_counts: HashMap<&str, u32> = HashMap::new();
    for card in pool {
        *pool_counts.entry(card.as_str()).or_insert(0) += 1;
    }

    // 3. Build deck multiset (card name -> requested count)
    let mut deck_counts: HashMap<&str, u32> = HashMap::new();
    for card in main_deck {
        *deck_counts.entry(card.as_str()).or_insert(0) += 1;
    }

    // 4. Validate each non-basic card against pool
    for (card_name, requested) in &deck_counts {
        // CR 903.13e: the granted filler is checked BEFORE the `is_addable`
        // exemption, never through it. `is_addable` names are pool-EXEMPT, i.e.
        // unlimited -- correct for basic lands and exactly wrong for a filler,
        // whose whole point is a cap of `max_copies`.
        //
        // The drafted copies are ordinary pool cards: these cards are printed
        // in the granting sets' own draft boosters, so only the copies ABOVE
        // the pool count are "added", and only those are capped and
        // commander-conditioned.
        if let Some(filler) = granted_filler.filter(|f| f.card_name.as_str() == *card_name) {
            let pooled = pool_counts.get(card_name).copied().unwrap_or(0);
            let added = requested.saturating_sub(pooled);
            if added > filler.max_copies {
                errors.push(LimitedDeckError::FillerExceedsGrant {
                    name: card_name.to_string(),
                    pooled,
                    added,
                    granted: filler.max_copies,
                });
            }
            // CR 903.13e: "... but only if those cards are used as the player's
            // commander(s)". Copies are fungible, so the condition is satisfied
            // when at least as many copies of this name are designated as were
            // added.
            let designated = commanders
                .iter()
                .filter(|c| c.as_str() == *card_name)
                .count() as u32;
            if added > designated {
                errors.push(LimitedDeckError::FillerNotUsedAsCommander {
                    name: card_name.to_string(),
                    pooled,
                    added,
                    designated,
                });
            }
            // The pool multiset below cannot express "pooled + granted", which
            // is why this arm returns rather than falling through to it.
            continue;
        }

        // Skip configured addable cards -- unlimited
        if addable_cards.is_addable(card_name) {
            continue;
        }

        match pool_counts.get(card_name) {
            None => {
                errors.push(LimitedDeckError::NotInPool {
                    name: card_name.to_string(),
                });
            }
            Some(&available) if *requested > available => {
                errors.push(LimitedDeckError::ExceedsPoolCount {
                    name: card_name.to_string(),
                    requested: *requested,
                    available,
                });
            }
            _ => {} // Valid -- pool has enough copies
        }
    }

    // 5. CR 702.124h: "You may designate two legendary CARDS as your commander
    // rather than one if each of them has partner." Two commanders are two
    // CARDS, and CR 903.3 says the same for the one-commander case ("Each deck
    // has a legendary CARD designated as its commander"). The guard is
    // therefore a MULTISET comparison and not a membership test: `commanders`
    // may name a card at most as many times as the deck contains it.
    //
    // A membership test accepts ONE filler in the deck with that same name
    // designated TWICE -- and the CR 903.13e arm above then reads that as
    // satisfying its commander-only condition, correctly, because it is
    // answering a different question (`added = 1 <= 2`, `designated = 2 >= 1`).
    // This guard is the only thing that rejects it.
    //
    // It reads `deck_counts`, which step 3 built and the loop never mutates, so
    // placement after the loop is a choice: it keeps every failure accumulating
    // into the same `Vec`, because this function reports all failures rather
    // than the first.
    let mut designated_counts: HashMap<&str, u32> = HashMap::new();
    for name in commanders {
        *designated_counts.entry(name.as_str()).or_insert(0) += 1;
    }
    for (name, designated) in &designated_counts {
        let in_deck = deck_counts.get(name).copied().unwrap_or(0);
        if *designated > in_deck {
            errors.push(LimitedDeckError::CommanderNotInDeck {
                name: (*name).to_string(),
                designated: *designated,
                in_deck,
            });
        }
    }

    // 6. CR 903.3: "Each deck has a legendary card designated as its
    // commander." A floor, not a cap -- CR 702.124g's cap is
    // `TooManyCommanders`, raised by `apply_submit_deck` on the payload alone
    // with an early `return`. This one PUSHES: the function reports ALL
    // failures, and returning here would mask `CommanderNotInDeck`.
    if commanders.len() < commanders_required {
        errors.push(LimitedDeckError::TooFewCommanders {
            designated: commanders.len(),
            minimum: commanders_required,
        });
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addable() -> DeckAddableCards {
        DeckAddableCards::standard_basics()
    }

    fn s(name: &str) -> String {
        name.to_string()
    }

    fn pool_of(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| s(n)).collect()
    }

    #[test]
    fn valid_40_card_deck() {
        let pool: Vec<String> = (0..45).map(|i| format!("Card {i}")).collect();
        let deck: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();
        assert!(validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0).is_ok());
    }

    #[test]
    fn too_few_cards() {
        let pool = pool_of(&["A", "B", "C"]);
        let deck = pool_of(&["A", "B", "C"]); // 3 cards, need 40
        let result = validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0);
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            LimitedDeckError::TooFewCards {
                actual: 3,
                minimum: 40
            }
        )));
    }

    #[test]
    fn card_not_in_pool() {
        let pool: Vec<String> = (0..45).map(|i| format!("Card {i}")).collect();
        let mut deck: Vec<String> = (0..39).map(|i| format!("Card {i}")).collect();
        deck.push(s("Not In Pool"));
        let result = validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0);
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            LimitedDeckError::NotInPool { name } if name == "Not In Pool"
        )));
    }

    #[test]
    fn exceeds_pool_count() {
        // Pool has 2 copies of "Rare Card", deck uses 3
        let mut pool: Vec<String> = (0..45).map(|i| format!("Card {i}")).collect();
        pool.push(s("Rare Card"));
        pool.push(s("Rare Card"));
        let mut deck: Vec<String> = (0..37).map(|i| format!("Card {i}")).collect();
        deck.extend([s("Rare Card"), s("Rare Card"), s("Rare Card")]);
        let result = validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0);
        let errors = result.unwrap_err();
        assert!(errors.iter().any(|e| matches!(
            e,
            LimitedDeckError::ExceedsPoolCount { name, requested: 3, available: 2 }
            if name == "Rare Card"
        )));
    }

    #[test]
    fn unlimited_basic_lands() {
        // Pool has no basic lands, but deck has 17 Plains
        let pool: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        let mut deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        deck.extend(std::iter::repeat_n(s("Plains"), 10));
        deck.extend(std::iter::repeat_n(s("Island"), 7));
        assert_eq!(deck.len(), 40);
        assert!(validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0).is_ok());
    }

    #[test]
    fn wastes_count_as_basic() {
        let pool: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        let mut deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        deck.extend(std::iter::repeat_n(s("Wastes"), 17));
        assert_eq!(deck.len(), 40);
        assert!(validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0).is_ok());
    }

    #[test]
    fn accumulates_multiple_errors() {
        let pool = pool_of(&["A"]);
        let deck = pool_of(&["A", "Not In Pool"]); // too few + not in pool
        let result = validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0);
        let errors = result.unwrap_err();
        assert!(
            errors.len() >= 2,
            "expected at least 2 errors, got {errors:?}"
        );
        assert!(errors
            .iter()
            .any(|e| matches!(e, LimitedDeckError::TooFewCards { .. })));
        assert!(errors
            .iter()
            .any(|e| matches!(e, LimitedDeckError::NotInPool { .. })));
    }

    #[test]
    fn pool_duplicates_allowed_up_to_pool_count() {
        // Pool has 2 copies, deck uses exactly 2 -- should be fine
        let mut pool: Vec<String> = (0..38).map(|i| format!("Card {i}")).collect();
        pool.extend([s("Dupe"), s("Dupe")]);
        let mut deck: Vec<String> = (0..38).map(|i| format!("Card {i}")).collect();
        deck.extend([s("Dupe"), s("Dupe")]);
        assert!(validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0).is_ok());
    }

    // ---------------------------------------------------------------------
    // CR 903.13e grantable filler + CR 702.124h designation multiset.
    //
    // Every case below drives `validate_limited_deck` directly with an
    // explicit `pool`, because the pool is the axis under test. The filler's
    // NAME is read back from the engine's CR-quoting table rather than typed
    // in, so these tests cannot drift from it -- and so no card name literal
    // appears here at all.
    //
    // Every pre-existing case takes `commanders_required: 0`. These are unit
    // rows over a KIND-AGNOSTIC function's parameters, not kind fixtures, and
    // each was written before the CR 903.3 floor existed -- so none is about
    // the floor. `0` makes `commanders.len() < 0` unsatisfiable, so the new
    // guard cannot fire and no row's verdict, subject or error `Vec` changes.
    // Floor coverage comes from the rows that pass a NONZERO value below, and
    // from the `apply`-level row in `session.rs`.
    // ---------------------------------------------------------------------

    /// The CMM concession, read from the engine's table. Panics if the table
    /// stops granting a filler for a set CR 903.13e names, which is itself the
    /// assertion that the two layers agree.
    fn cmm_filler() -> GrantableCommanderFiller {
        engine::game::deck_validation::draft_set_concessions("CMM")
            .filler
            .expect("CR 903.13e names Commander Masters as a granting set")
    }

    /// A 60-card deck: `extra` copies of the filler plus filler-free padding.
    fn deck_with_filler(filler: &GrantableCommanderFiller, copies: usize) -> Vec<String> {
        let mut deck: Vec<String> = (0..60 - copies).map(|i| format!("Card {i}")).collect();
        deck.extend(std::iter::repeat_n(filler.card_name.clone(), copies));
        assert_eq!(deck.len(), 60, "fixture must be a 60-card deck");
        deck
    }

    /// A pool holding every padding card plus `pooled` drafted filler copies.
    fn pool_with_filler(filler: &GrantableCommanderFiller, pooled: usize) -> Vec<String> {
        let mut pool: Vec<String> = (0..60).map(|i| format!("Card {i}")).collect();
        pool.extend(std::iter::repeat_n(filler.card_name.clone(), pooled));
        pool
    }

    /// U7 row 3 -- positive reach-guard. `(pooled = 0, requested = 2,
    /// designated = 2)`: two ADDED fillers, both designated, nothing drafted.
    /// Every negative below pairs to this, so none of them can pass by the
    /// filler branch never being reached.
    #[test]
    fn filler_two_added_copies_both_designated_is_ok() {
        let filler = cmm_filler();
        let deck = deck_with_filler(&filler, 2);
        let pool = pool_with_filler(&filler, 0);
        let commanders = vec![filler.card_name.clone(), filler.card_name.clone()];
        assert!(
            validate_limited_deck(&deck, &pool, &addable(), 60, Some(&filler), &commanders, 0)
                .is_ok()
        );
    }

    /// U7 row 3b -- CR 702.124h, paired to row 3 on ONE axis. Same deck size,
    /// same designations, `requested` 2 -> 1: `(pooled = 0, requested = 1,
    /// designated = 2)`. One filler card in the deck, that name designated
    /// twice.
    ///
    /// This is discriminating precisely BECAUSE the CR 903.13e filler arm
    /// accepts it on its own terms -- `added = 1 <= 2` satisfies the cap and
    /// `designated = 2 >= added = 1` satisfies the commander-only condition --
    /// so the multiset guard is the only thing that can reject it. Written as
    /// a membership test (`main_deck.contains(name)`) this case passes and the
    /// test is red for exactly the defect it exists to catch.
    #[test]
    fn one_deck_copy_designated_twice_is_rejected() {
        let filler = cmm_filler();
        let deck = deck_with_filler(&filler, 1);
        let pool = pool_with_filler(&filler, 0);
        let commanders = vec![filler.card_name.clone(), filler.card_name.clone()];
        let errors =
            validate_limited_deck(&deck, &pool, &addable(), 60, Some(&filler), &commanders, 0)
                .unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                LimitedDeckError::CommanderNotInDeck {
                    designated: 2,
                    in_deck: 1,
                    ..
                }
            )),
            "expected CommanderNotInDeck {{ designated: 2, in_deck: 1 }}, got {errors:?}"
        );
        // The filler arm itself must NOT have complained: it is answering a
        // different question and answering it correctly. If either filler
        // variant fires here, the arithmetic has drifted.
        assert!(
            !errors.iter().any(|e| matches!(
                e,
                LimitedDeckError::FillerExceedsGrant { .. }
                    | LimitedDeckError::FillerNotUsedAsCommander { .. }
            )),
            "the CR 903.13e arm must accept this case on its own terms: {errors:?}"
        );
    }

    /// Sibling of row 3b: the absent-name case. One comparison, both classes --
    /// `in_deck == 0` (absent) and `0 < in_deck < designated` (under-supplied)
    /// are the same variant.
    #[test]
    fn designating_a_card_absent_from_the_deck_is_rejected() {
        let pool: Vec<String> = (0..60).map(|i| format!("Card {i}")).collect();
        let deck: Vec<String> = (0..60).map(|i| format!("Card {i}")).collect();
        let commanders = vec![s("Never Drafted")];
        let errors =
            validate_limited_deck(&deck, &pool, &addable(), 60, None, &commanders, 0).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                LimitedDeckError::CommanderNotInDeck { name, designated: 1, in_deck: 0 }
                    if name == "Never Drafted"
            )),
            "expected CommanderNotInDeck {{ designated: 1, in_deck: 0 }}, got {errors:?}"
        );
    }

    /// U7 row 4 -- CR 903.13e's cap still bites. Three fillers, none drafted:
    /// `added = 3 > 2`.
    #[test]
    fn three_added_fillers_exceed_the_grant() {
        let filler = cmm_filler();
        let deck = deck_with_filler(&filler, 3);
        let pool = pool_with_filler(&filler, 0);
        let commanders = vec![filler.card_name.clone(), filler.card_name.clone()];
        let errors =
            validate_limited_deck(&deck, &pool, &addable(), 60, Some(&filler), &commanders, 0)
                .unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                LimitedDeckError::FillerExceedsGrant {
                    pooled: 0,
                    added: 3,
                    granted: 2,
                    ..
                }
            )),
            "expected FillerExceedsGrant {{ pooled: 0, added: 3, granted: 2 }}, got {errors:?}"
        );
    }

    /// U7 row 5 -- CR 903.13e's commander-only condition still bites on
    /// genuinely ADDED copies.
    #[test]
    fn added_fillers_not_designated_are_rejected() {
        let filler = cmm_filler();
        let deck = deck_with_filler(&filler, 2);
        let pool = pool_with_filler(&filler, 0);
        let errors =
            validate_limited_deck(&deck, &pool, &addable(), 60, Some(&filler), &[], 0).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                LimitedDeckError::FillerNotUsedAsCommander {
                    pooled: 0,
                    added: 2,
                    designated: 0,
                    ..
                }
            )),
            "expected FillerNotUsedAsCommander {{ pooled: 0, added: 2, designated: 0 }}, got {errors:?}"
        );
    }

    /// U7 row 6 -- legal deck alpha: DRAFTED and played in the 99. One filler
    /// in the deck, one in the pool, nothing designated. `added = 0`, so
    /// neither CR 903.13e condition applies.
    ///
    /// A pool-blind rule returns `FillerNotUsedAsCommander` here, on a legal
    /// deck. Same counts as row 5 with only the POOL changed.
    #[test]
    fn drafted_filler_played_in_the_ninety_nine_is_ok() {
        let filler = cmm_filler();
        let deck = deck_with_filler(&filler, 1);
        let pool = pool_with_filler(&filler, 1);
        assert!(validate_limited_deck(&deck, &pool, &addable(), 60, Some(&filler), &[], 0).is_ok());
    }

    /// U7 row 7 -- legal deck beta: drafted one, ADDED two. `added = 2 <= 2`
    /// and `designated = 2 >= 2`.
    ///
    /// A pool-blind cap returns `FillerExceedsGrant` here. Same counts as
    /// row 4 with only the POOL changed -- one axis, opposite verdicts.
    #[test]
    fn drafted_one_filler_plus_two_added_is_ok() {
        let filler = cmm_filler();
        let deck = deck_with_filler(&filler, 3);
        let pool = pool_with_filler(&filler, 1);
        let commanders = vec![filler.card_name.clone(), filler.card_name.clone()];
        assert!(
            validate_limited_deck(&deck, &pool, &addable(), 60, Some(&filler), &commanders, 0)
                .is_ok()
        );
    }

    /// U7 row 8 -- hostile fixture: the SAME deck under a session whose set
    /// grants nothing. The filler arm must be genuinely gated on the grant, so
    /// these fall through to today's pool-multiset branches and get today's
    /// honest messages.
    #[test]
    fn without_a_grant_the_filler_is_an_ordinary_pool_card() {
        let filler = cmm_filler();

        let deck = deck_with_filler(&filler, 2);
        let pool = pool_with_filler(&filler, 0);
        let errors = validate_limited_deck(&deck, &pool, &addable(), 60, None, &[], 0).unwrap_err();
        assert!(
            errors.iter().any(
                |e| matches!(e, LimitedDeckError::NotInPool { name } if *name == filler.card_name)
            ),
            "expected NotInPool, got {errors:?}"
        );

        let pool = pool_with_filler(&filler, 1);
        let errors = validate_limited_deck(&deck, &pool, &addable(), 60, None, &[], 0).unwrap_err();
        assert!(
            errors.iter().any(|e| matches!(
                e,
                LimitedDeckError::ExceedsPoolCount { name, requested: 2, available: 1 }
                    if *name == filler.card_name
            )),
            "expected ExceedsPoolCount {{ requested: 2, available: 1 }}, got {errors:?}"
        );
    }

    /// The filler is pool-RELATIVE while `is_addable` names are pool-EXEMPT.
    /// Different mechanisms, and this proves they were not merged: an addable
    /// basic is unlimited under the very same call in which the filler is
    /// capped at two.
    #[test]
    fn filler_cap_does_not_leak_onto_addable_basics() {
        let filler = cmm_filler();
        let mut deck: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();
        deck.extend(std::iter::repeat_n(s("Plains"), 17));
        deck.extend(std::iter::repeat_n(filler.card_name.clone(), 3));
        let pool: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();
        let commanders = vec![filler.card_name.clone(), filler.card_name.clone()];
        let errors =
            validate_limited_deck(&deck, &pool, &addable(), 40, Some(&filler), &commanders, 0)
                .unwrap_err();
        // The filler is capped ...
        assert!(errors
            .iter()
            .any(|e| matches!(e, LimitedDeckError::FillerExceedsGrant { .. })));
        // ... and the 17 pool-absent Plains are still unlimited.
        assert!(
            !errors
                .iter()
                .any(|e| matches!(e, LimitedDeckError::NotInPool { name } if name == "Plains")),
            "basic lands must stay pool-exempt: {errors:?}"
        );
    }
    // -------------------------------------------------------------------
    // PF3 / U26 — CR 903.3's designation floor, at the PARAMETER level.
    //
    // These are the unit rows over `commanders_required`. They are paired
    // with, and do not substitute for, the `apply`-level row in `session.rs`:
    // entering here cannot see whether the CALLER supplies the kind's value.
    // -------------------------------------------------------------------

    /// VM row 4 — the floor is read FROM THE PARAMETER.
    ///
    /// Revert step 6 and the empty designation below is accepted.
    #[test]
    fn a_designation_floor_of_one_refuses_an_empty_designation() {
        let pool: Vec<String> = (0..45).map(|i| format!("Card {i}")).collect();
        let deck: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();

        let errors = validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 1)
            .expect_err("CR 903.3: a deck that must designate one, designates none");
        assert!(
            errors.contains(&LimitedDeckError::TooFewCommanders {
                designated: 0,
                minimum: 1,
            }),
            "{errors:?}"
        );

        // Paired positive reach-guard: the SAME shape with one backed
        // designation passes. Without it, a validator that refused every
        // Commander-shaped call would satisfy the negative above.
        assert!(
            validate_limited_deck(&deck, &pool, &addable(), 40, None, &[s("Card 0")], 1).is_ok(),
            "one backed designation satisfies the floor"
        );

        // Hostile sibling — a floor of ZERO must accept an empty designation.
        // This is the row that keeps Premier/Traditional/Sealed submissions
        // working, and it is the reach-guard proving the floor is READ rather
        // than assumed: a guard that ignored the parameter would red here.
        assert!(
            validate_limited_deck(&deck, &pool, &addable(), 40, None, &[], 0).is_ok(),
            "CR 905.1a kinds designate no commander"
        );
    }

    /// VM row 5 — the floor PUSHES, so it cannot mask `CommanderNotInDeck`.
    ///
    /// Multi-authority. The load-bearing case is the one where BOTH authorities
    /// fail at once — CR 903.3's floor and CR 702.124h's multiset rule — because
    /// that is the only shape in which step 6's control-flow regime is
    /// observable: with the floor satisfied, a `push` and an early `return` are
    /// indistinguishable. Turn step 6 into `return Err(vec![..])` and the second
    /// assertion below reds with `[TooFewCommanders { designated: 1, minimum: 2 }]`,
    /// the accumulated `CommanderNotInDeck` having been discarded.
    #[test]
    fn the_designation_floor_does_not_mask_an_unbacked_designation() {
        let pool: Vec<String> = (0..45).map(|i| format!("Card {i}")).collect();
        let deck: Vec<String> = (0..40).map(|i| format!("Card {i}")).collect();

        // "Card 44" is in the POOL but not in the DECK, so CR 702.124h's
        // multiset rule fires. The floor is SATISFIED here (1 >= 1): this half
        // proves the new guard did not displace the existing one.
        let errors = validate_limited_deck(&deck, &pool, &addable(), 40, None, &[s("Card 44")], 1)
            .expect_err("CR 702.124h: the designation must be backed by a copy in the deck");
        assert!(
            errors.contains(&LimitedDeckError::CommanderNotInDeck {
                name: s("Card 44"),
                designated: 1,
                in_deck: 0,
            }),
            "{errors:?}"
        );

        // BOTH authorities fail: one designation against a CR 702.124h partner
        // floor of two, and that one designation is itself unbacked. The
        // function reports ALL failures into ONE `Vec`, so both must be present.
        let errors = validate_limited_deck(&deck, &pool, &addable(), 40, None, &[s("Card 44")], 2)
            .expect_err("both the floor and the multiset rule are violated");
        assert!(
            errors.contains(&LimitedDeckError::TooFewCommanders {
                designated: 1,
                minimum: 2,
            }) && errors.contains(&LimitedDeckError::CommanderNotInDeck {
                name: s("Card 44"),
                designated: 1,
                in_deck: 0,
            }),
            "step 6 must PUSH, not short-circuit: {errors:?}"
        );

        // Paired positive: the same call with the name IN the deck and the
        // floor satisfied yields neither error, so the assertions above
        // discriminate rather than firing on any input at all.
        assert!(
            validate_limited_deck(&deck, &pool, &addable(), 40, None, &[s("Card 0")], 1).is_ok(),
            "a backed designation raises neither error"
        );
    }
}
