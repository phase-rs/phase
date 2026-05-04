use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
}

/// Validate a Limited deck against the drafted pool.
///
/// Rules (per MTG Limited):
/// - Main deck must have at least `min_deck_size` cards (default 40)
/// - All non-basic cards must be present in the pool with sufficient copies
/// - Basic lands (from `basic_land_names`) are available in unlimited quantity
/// - No constructed legality check, no 4-copy limit
///
/// Returns Ok(()) on success, Err with all accumulated errors on failure.
pub fn validate_limited_deck(
    main_deck: &[String],
    pool: &[String],
    basic_land_names: &[&str],
    min_deck_size: usize,
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
        // Skip basic lands -- unlimited
        if basic_land_names.iter().any(|b| b == card_name) {
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

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40).is_ok());
    }

    #[test]
    fn too_few_cards() {
        let pool = pool_of(&["A", "B", "C"]);
        let deck = pool_of(&["A", "B", "C"]); // 3 cards, need 40
        let result = validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40);
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
        let result = validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40);
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
        let result = validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40);
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
        assert!(validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40).is_ok());
    }

    #[test]
    fn wastes_count_as_basic() {
        let pool: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        let mut deck: Vec<String> = (0..23).map(|i| format!("Card {i}")).collect();
        deck.extend(std::iter::repeat_n(s("Wastes"), 17));
        assert_eq!(deck.len(), 40);
        assert!(validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40).is_ok());
    }

    #[test]
    fn accumulates_multiple_errors() {
        let pool = pool_of(&["A"]);
        let deck = pool_of(&["A", "Not In Pool"]); // too few + not in pool
        let result = validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40);
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
        assert!(validate_limited_deck(&deck, &pool, STANDARD_BASIC_LANDS, 40).is_ok());
    }
}
