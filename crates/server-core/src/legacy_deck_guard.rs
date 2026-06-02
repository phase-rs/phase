//! Deck payload bounds for legacy `CreateGame` / `JoinGame` wire paths.
//!
//! Settings-based create/join use `lobby_broker::guard_inbound`, but the legacy
//! deck-only API goes straight to `resolve_deck` without bounding client card
//! lists first.

use engine::starter_decks::DeckData;

pub const MAX_MAIN_DECK_ENTRIES: usize = 500;
pub const MAX_SIDEBOARD_ENTRIES: usize = 100;
pub const MAX_COMMANDER_ENTRIES: usize = 4;
pub const MAX_DECK_CARD_NAME_LEN: usize = 256;

fn validate_card_name(field: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{field} must not be empty"));
    }
    if name.len() > MAX_DECK_CARD_NAME_LEN {
        return Err(format!(
            "{field} must be at most {MAX_DECK_CARD_NAME_LEN} bytes"
        ));
    }
    if name.chars().any(|c| c.is_control()) {
        return Err(format!("{field} must not contain control characters"));
    }
    Ok(())
}

fn validate_deck_list(field: &str, cards: &[String], max_entries: usize) -> Result<(), String> {
    if cards.len() > max_entries {
        return Err(format!(
            "{field} must contain at most {max_entries} entries"
        ));
    }
    for (index, name) in cards.iter().enumerate() {
        validate_card_name(&format!("{field}[{index}]"), name)?;
    }
    Ok(())
}

/// Validate legacy create/join deck payloads before card-database resolution.
pub fn guard_legacy_deck(deck: &DeckData) -> Result<(), String> {
    validate_deck_list("deck.main_deck", &deck.main_deck, MAX_MAIN_DECK_ENTRIES)?;
    validate_deck_list("deck.sideboard", &deck.sideboard, MAX_SIDEBOARD_ENTRIES)?;
    validate_deck_list("deck.commander", &deck.commander, MAX_COMMANDER_ENTRIES)?;
    Ok(())
}
