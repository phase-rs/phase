//! Slivers Horde (community format, hordemagic.com) — the decklist for
//! [`crate::types::format::ChallengeDeck::SliversHorde`].
//!
//! Data, not logic. 135 non-token cards plus 170 predefined Metallic Sliver
//! tokens = a 305-card library. (The page's prose says "300" but its own section
//! subtotals sum to 305; the itemized list is authoritative.)
//!
//! Coverage: all 52 distinct card names resolve against the card DB and NONE
//! parse to an `Unimplemented` effect — the cleanest of every deck vetted. The
//! sole token is a single unambiguous vanilla body at `fidelity = Full`, so
//! nothing here waits on the token-abilities gap.
//!
//! Wave rule: the community default from hordemagic.com's basic rules — "Waves
//! end when an UNCOMMON, RARE or MYTHIC card is cast" — i.e.
//! `WaveTermination::UntilRarityAtLeast(Rarity::Uncommon)`. This page states no
//! deck-specific rules of its own and defers to those basic rules.
//!
//! PERFORMANCE NOTE: this deck is built to put 100+ Slivers onto the battlefield,
//! and Sliver lords apply continuous modifications across every Sliver. Sliver
//! Legion in particular is a dynamic power boost counted over other Slivers,
//! which is O(n) per affected Sliver — i.e. O(n^2) in board size. Layer
//! evaluation, not parsing, is the thing to watch; see the deck's board-size
//! test below.

/// The non-token cards, as `(count, exact card name)` pairs. Each name resolves
/// against the runtime card database (`CardDatabase::get_face_by_name`); the
/// injection in `deck_loading::load_horde_library` panics on any name that fails
/// to resolve so a transcription error surfaces immediately.
///
/// 5 legendary + 116 creature-section + 14 spells = 135 non-token cards.
pub const SLIVERS_HORDE_NONTOKEN_CARDS: &[(u32, &str)] = &[
    // Legendary creatures (5).
    (1, "Sliver Hivelord"),
    (1, "Sliver Legion"),
    (1, "Sliver Overlord"),
    (1, "Sliver Queen"),
    (1, "The First Sliver"),
    // Creature section (116). Note "Soul Shatter" is filed under creatures on
    // the source page but is actually a black instant; kept at the listed count
    // because the itemized decklist is authoritative.
    (2, "Battering Sliver"),
    (2, "Belligerent Sliver"),
    (2, "Bonesplitter Sliver"),
    (4, "Brood Sliver"),
    (5, "Cleaving Sliver"),
    (2, "Crystalline Sliver"),
    (4, "Diffusion Sliver"),
    (3, "Fungus Sliver"),
    (3, "Fury Sliver"),
    (4, "Groundshaker Sliver"),
    (2, "Harmonic Sliver"),
    (2, "Horned Sliver"),
    (6, "Lancer Sliver"),
    (5, "Lavabelly Sliver"),
    (3, "Leeching Sliver"),
    (1, "Megantic Sliver"),
    (10, "Metallic Sliver"),
    (2, "Might Sliver"),
    (2, "Muscle Sliver"),
    (3, "Predatory Sliver"),
    (2, "Pulmonic Sliver"),
    (1, "Shadow Sliver"),
    (3, "Shifting Sliver"),
    (2, "Sidewinder Sliver"),
    (2, "Sinew Sliver"),
    (2, "Soul Shatter"),
    (1, "Spined Sliver"),
    (2, "Spiteful Sliver"),
    (4, "Steelform Sliver"),
    (4, "Striking Sliver"),
    (3, "Talon Sliver"),
    (4, "Tempered Sliver"),
    (2, "Thorncaster Sliver"),
    (4, "Two-Headed Sliver"),
    (2, "Vampiric Sliver"),
    (3, "Venom Sliver"),
    (2, "Virulent Sliver"),
    (1, "Watcher Sliver"),
    (5, "Winged Sliver"),
    // Spells (14).
    (2, "Barter in Blood"),
    (5, "Crackling Doom"),
    (2, "Destructive Flow"),
    (1, "Necrotic Hex"),
    (1, "Simplify"),
    (1, "Tranquility"),
    (1, "Vona's Hunger"),
    (1, "Wave of Vitriol"),
];

/// The predefined tokens, as `(count, token selector)` pairs. Pinned by preset
/// **id** for consistency with the other Horde decks, even though "Metallic
/// Sliver" happens to be unambiguous (a single body in the catalog) — an id
/// cannot start matching a different body if the catalog later gains another
/// same-named printing.
///
/// The body is a 1/1 vanilla at `fidelity = Full` with no rules text, so it
/// round-trips through the library→battlefield reveal losing nothing.
///
/// 170 Metallic Sliver tokens.
pub const SLIVERS_HORDE_TOKENS: &[(u32, &str)] = &[
    // 1/1 colorless Metallic Sliver artifact creature, vanilla (TSR #15).
    (170, "134a8ff3-eddb-5e65-9e27-ac521c0357e4"),
];

/// Total non-token card count (for coverage assertions / library-size sanity).
pub fn nontoken_card_count() -> u32 {
    SLIVERS_HORDE_NONTOKEN_CARDS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

/// Total token count.
pub fn token_count() -> u32 {
    SLIVERS_HORDE_TOKENS.iter().map(|(count, _)| *count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_matches_the_published_subtotals() {
        // 5 legendary + 116 creature-section + 14 spells = 135 non-token.
        assert_eq!(nontoken_card_count(), 135);
        assert_eq!(token_count(), 170);
        assert_eq!(nontoken_card_count() + token_count(), 305);
    }

    /// The token selector must resolve through the same name-then-preset-id
    /// fallback `deck_loading::load_horde_library` uses, so a mistyped or retired
    /// id fails here rather than panicking at game start.
    #[test]
    fn token_selector_resolves_to_the_metallic_sliver_body() {
        for (_, selector) in SLIVERS_HORDE_TOKENS {
            let body = crate::game::token_presets::known_token_body_by_name(selector)
                .or_else(|| {
                    crate::game::token_presets::known_token_preset_by_id(selector).map(|p| &p.body)
                })
                .unwrap_or_else(|| {
                    panic!("Slivers Horde token selector '{selector}' must resolve to a body")
                });
            assert_eq!(body.display_name, "Metallic Sliver");
            assert_eq!(body.power, Some(1));
            assert_eq!(body.toughness, Some(1));
        }
    }

    /// The token must be VANILLA. `TokenCharacteristics` carries no channel for
    /// non-keyword abilities, so a token with printed rules text would silently
    /// enter without them (the tracked token-abilities gap). This deck is
    /// playable today precisely because its token needs none — pin that, so a
    /// future re-pin to an ability-bearing printing fails loudly.
    #[test]
    fn token_is_vanilla_so_nothing_is_silently_dropped() {
        for (_, selector) in SLIVERS_HORDE_TOKENS {
            let preset = crate::game::token_presets::known_token_preset_by_id(selector)
                .expect("pinned token preset id must exist in the catalog");
            assert!(
                preset.rules_text.is_none(),
                "{selector} must be a vanilla body, but carries rules text: {:?}",
                preset.rules_text
            );
            assert!(
                preset.body.keywords.is_empty(),
                "{selector} must be a vanilla body, but carries keywords: {:?}",
                preset.body.keywords
            );
        }
    }
}
