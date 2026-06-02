//! Deck payload bounds for legacy `CreateGame` / `JoinGame` wire paths.
//!
//! Settings-based create/join use `lobby_broker::guard_inbound`, but the legacy
//! deck-only API goes straight to `resolve_deck` without bounding client card
//! lists first.

use engine::starter_decks::DeckData;
use lobby_broker::validate_deck_payload;

/// Validate legacy create/join deck payloads before card-database resolution.
pub fn guard_legacy_deck(deck: &DeckData) -> Result<(), String> {
    validate_deck_payload("deck", deck)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_broker::inbound_guard::MAX_MAIN_DECK_ENTRIES;

    fn deck(main: &[&str], sideboard: &[&str], commander: &[&str]) -> DeckData {
        fn v(s: &[&str]) -> Vec<String> {
            s.iter().map(|x| x.to_string()).collect()
        }
        DeckData {
            main_deck: v(main),
            sideboard: v(sideboard),
            commander: v(commander),
            bracket_tier: Default::default(),
        }
    }

    #[test]
    fn legacy_deck_accepts_valid_payload() {
        assert!(guard_legacy_deck(&deck(&["Forest"], &[], &[])).is_ok());
    }

    #[test]
    fn legacy_deck_rejects_oversized_main() {
        let names: Vec<&str> = vec!["Card"; MAX_MAIN_DECK_ENTRIES + 1];
        let err = guard_legacy_deck(&deck(&names, &[], &[])).unwrap_err();
        assert!(err.contains("main_deck"));
    }
}
