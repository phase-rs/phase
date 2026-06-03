//! Wire validation for legacy `JoinGame` frames.
//!
//! `guard_legacy_deck` bounds the deck payload, but `JoinGame` also carries a
//! `game_code` that was cloned through `resolve_deck` and session lookup without
//! the same bounds enforced on settings-based join paths.

use engine::starter_decks::DeckData;
use lobby_broker::validation::{validate_token, MAX_GAME_CODE_LEN};

use crate::legacy_deck_guard::guard_legacy_deck;

/// Validate legacy join wire fields before card-database resolution.
pub fn guard_legacy_join_game(game_code: &str, deck: &DeckData) -> Result<(), String> {
    validate_token("game_code", game_code, MAX_GAME_CODE_LEN)?;
    guard_legacy_deck(deck)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lobby_broker::inbound_guard::MAX_MAIN_DECK_ENTRIES;
    use lobby_broker::validation::MAX_GAME_CODE_LEN;

    fn deck(main: &[&str]) -> DeckData {
        DeckData {
            main_deck: main.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn legacy_join_accepts_valid_fields() {
        assert!(guard_legacy_join_game("ABC123", &deck(&["Forest"])).is_ok());
    }

    #[test]
    fn legacy_join_rejects_oversized_game_code() {
        let err = guard_legacy_join_game(&"x".repeat(MAX_GAME_CODE_LEN + 1), &deck(&["Forest"]))
            .unwrap_err();
        assert!(err.contains("game_code"));
    }

    #[test]
    fn legacy_join_rejects_oversized_deck() {
        let names: Vec<&str> = vec!["Card"; MAX_MAIN_DECK_ENTRIES + 1];
        let err = guard_legacy_join_game("ABC123", &deck(&names)).unwrap_err();
        assert!(err.contains("main_deck"));
    }
}
