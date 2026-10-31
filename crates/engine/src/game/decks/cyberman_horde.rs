//! Doctor Who "Cyberman Horde" — the concrete decklist for
//! [`crate::types::format::ChallengeDeck::CybermanHorde`].
//!
//! Data, not logic. The Horde seat's engine-supplied library is ~300 cards:
//! ~100 real Universes Beyond (WHO) non-token cards resolved from the card DB by
//! name, plus ~200 predefined Cyberman/Dalek tokens materialized from
//! `game::token_presets::known_token_body_by_name`. Tokens can never be cast
//! (CR 111), so they wait in the library and enter the battlefield when revealed
//! (see `game::horde::reveal_library_token`); non-tokens are free-cast on reveal.
//!
//! The ~10 planar cards of the published deck are intentionally omitted: they
//! drag in Planechase integration and the deck is fully playable without them.

/// The non-token cards, as `(count, exact card name)` pairs. Each name resolves
/// against the runtime card database (`CardDatabase::get_face_by_name`); the
/// injection in `deck_loading::load_horde_library` panics on any name that fails
/// to resolve so a transcription error surfaces immediately rather than shipping
/// a silently-truncated library.
///
/// 50 creatures + 50 spells = 100 non-token cards.
pub const CYBERMAN_HORDE_NONTOKEN_CARDS: &[(u32, &str)] = &[
    // Creatures (50).
    (1, "Ashad, the Lone Cyberman"),
    (1, "Cult of Skaro"),
    (10, "Cyberman Patrol"),
    (10, "Cybermen Squadron"),
    (10, "Dalek Drone"),
    (10, "Dalek Squadron"),
    (1, "Davros, Dalek Creator"),
    (1, "Missy"),
    (1, "The Cyber-Controller"),
    (1, "The Dalek Emperor"),
    (1, "The Master, Gallifrey's End"),
    (1, "The Master, Multiplied"),
    (1, "The Rani"),
    (1, "The Valeyard"),
    // Spells (50).
    (10, "Blasphemous Act"),
    (15, "Cyber Conversion"),
    (10, "Cybership"),
    (10, "Death in Heaven"),
    (5, "Exterminate!"),
];

/// The predefined tokens, as `(count, preset token name)` pairs. Each name is
/// resolved to a `TokenCharacteristics` body via
/// `token_presets::known_token_body_by_name`; the body is materialized into a
/// library-resident token object (see `deck_loading::create_horde_library_token`).
///
/// 100 Cyberman + 100 Dalek = 200 tokens.
pub const CYBERMAN_HORDE_TOKENS: &[(u32, &str)] = &[(100, "Cyberman"), (100, "Dalek")];

/// Total non-token card count (for coverage assertions / library-size sanity).
pub fn nontoken_card_count() -> u32 {
    CYBERMAN_HORDE_NONTOKEN_CARDS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

/// Total token count.
pub fn token_count() -> u32 {
    CYBERMAN_HORDE_TOKENS.iter().map(|(count, _)| *count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decklist_totals_are_300() {
        assert_eq!(nontoken_card_count(), 100, "100 non-token cards");
        assert_eq!(token_count(), 200, "200 tokens");
        assert_eq!(nontoken_card_count() + token_count(), 300);
    }

    /// Every token preset name must resolve to a single unambiguous body — the
    /// injection depends on `known_token_body_by_name` returning `Some`.
    #[test]
    fn token_preset_names_resolve() {
        for (_, name) in CYBERMAN_HORDE_TOKENS {
            assert!(
                crate::game::token_presets::known_token_body_by_name(name).is_some(),
                "token preset '{name}' must resolve to a unique body"
            );
        }
    }
}
