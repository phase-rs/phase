use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Standard basic land names that are always available in unlimited quantity.
pub const STANDARD_BASIC_LANDS: &[&str] =
    &["Plains", "Island", "Swamp", "Mountain", "Forest", "Wastes"];

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
