//! Humans and Godzilla Horde (community format, hordemagic.com) — the decklist
//! for [`crate::types::format::ChallengeDeck::HumansGodzillaHorde`].
//!
//! Data, not logic. 100 non-token cards plus 200 predefined Human Soldier tokens
//! = a 300-card library, matching the page's stated count exactly.
//!
//! Coverage: all 30 distinct card names resolve against the card DB. Two 1-copy
//! cards carry partial parse gaps (Yidaro, Wandering Monster and Zilortha,
//! Strength Incarnate); at one copy each out of 300 they are not build-gating.
//! The bulk token is a single vanilla body at `fidelity = Full`.
//!
//! Wave rule: the community default from hordemagic.com's basic rules — "Waves
//! end when an UNCOMMON, RARE or MYTHIC card is cast" — i.e.
//! `WaveTermination::UntilRarityAtLeast(Rarity::Uncommon)`. This page states no
//! deck-specific rules of its own and defers to those basic rules.
//!
//! Note: the source page also hosts five SURVIVOR commander decks (Brokkos,
//! Illuna, Nethroi, Snapdax, Vadrok). Those are defender content, not Horde
//! library content, and are deliberately not represented here.

/// The non-token cards, as `(count, exact card name)` pairs. Each name resolves
/// against the runtime card database (`CardDatabase::get_face_by_name`); the
/// injection in `deck_loading::load_horde_library` panics on any name that fails
/// to resolve so a transcription error surfaces immediately.
///
/// 8 titans + 73 creatures + 19 spells = 100 non-token cards.
pub const HUMANS_GODZILLA_HORDE_NONTOKEN_CARDS: &[(u32, &str)] = &[
    // The Titans (8) — the deck's Godzilla-series bombs.
    (1, "Ghalta, Primal Hunger"),
    (1, "Kogla and Yidaro"),
    (1, "Kogla, the Titan Ape"),
    (1, "Luminous Broodmoth"),
    (1, "Titanoth Rex"),
    (1, "Void Beckoner"),
    (1, "Yidaro, Wandering Monster"),
    (1, "Zilortha, Strength Incarnate"),
    // Creatures (73) — the human army the titans tower over.
    (3, "Anim Pakal, Thousandth Moon"),
    (1, "General Kudro of Drannith"),
    (15, "Gryff Rider"),
    (7, "Gryffwing Cavalry"),
    (1, "Jirina Kudro"),
    (1, "Katilda and Lier"),
    (1, "King Darien XLVIII"),
    (9, "Knights of Dol Amroth"),
    (1, "Kyler, Sigardian Emissary"),
    (3, "Odric, Lunarch Marshal"),
    (15, "Parish-Blade Trainee"),
    (10, "Precinct Captain"),
    (4, "Savior of Ollenbock"),
    (1, "Torens, Fist of the Angels"),
    (1, "Trynn, Champion of Freedom"),
    // Spells (19).
    (5, "Captain's Call"),
    (1, "Descend upon the Sinful"),
    (1, "Divine Sacrament"),
    (4, "Glorious Anthem"),
    (5, "Increasing Devotion"),
    (2, "Planar Cleansing"),
    (1, "Play of the Game"),
];

/// The predefined tokens, as `(count, token selector)` pairs. Pinned by preset
/// **id**: "Human Soldier" maps to TWO distinct bodies in the catalog — a 1/1
/// vanilla and a 1/1 with Training — so a name lookup is ambiguous and returns
/// `None`. The bulk tokens are the vanilla body.
///
/// The Training variant is deliberately NOT used here even though the deck runs
/// one Torens, Fist of the Angels (which creates it). That card makes its own
/// tokens at runtime through the ordinary token pipeline; the 200 library tokens
/// are the plain body.
///
/// 200 Human Soldier tokens.
pub const HUMANS_GODZILLA_HORDE_TOKENS: &[(u32, &str)] = &[
    // 1/1 white Human Soldier, vanilla (INR #3 body, 23 printings).
    (200, "171f64e4-ac89-5b3a-a021-cb3317c639b7"),
];

/// Total non-token card count (for coverage assertions / library-size sanity).
pub fn nontoken_card_count() -> u32 {
    HUMANS_GODZILLA_HORDE_NONTOKEN_CARDS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

/// Total token count.
pub fn token_count() -> u32 {
    HUMANS_GODZILLA_HORDE_TOKENS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_is_three_hundred_cards() {
        // 8 titans + 73 creatures + 19 spells = 100 non-token, + 200 tokens.
        assert_eq!(nontoken_card_count(), 100);
        assert_eq!(token_count(), 200);
        assert_eq!(nontoken_card_count() + token_count(), 300);
    }

    /// The token selector must resolve through the same name-then-preset-id
    /// fallback `deck_loading::load_horde_library` uses. "Human Soldier" is
    /// AMBIGUOUS by name (vanilla vs Training), so this also pins that the id
    /// lands on the intended vanilla body rather than the Training one.
    #[test]
    fn token_selector_resolves_to_the_vanilla_human_soldier() {
        for (_, selector) in HUMANS_GODZILLA_HORDE_TOKENS {
            let body = crate::game::token_presets::known_token_body_by_name(selector)
                .or_else(|| {
                    crate::game::token_presets::known_token_preset_by_id(selector).map(|p| &p.body)
                })
                .unwrap_or_else(|| {
                    panic!("Humans & Godzilla token selector '{selector}' must resolve to a body")
                });
            assert_eq!(body.display_name, "Human Soldier");
            assert_eq!(body.power, Some(1));
            assert_eq!(body.toughness, Some(1));
        }
    }

    /// The token must be VANILLA — specifically NOT the Training printing, whose
    /// Training keyword the catalog records as an unmodelled ability
    /// (`PartialMissingAbilities`). `TokenCharacteristics` carries no channel for
    /// non-keyword abilities, so picking that body would silently drop Training
    /// on all 200 tokens. Pin it so a future re-pin fails loudly.
    #[test]
    fn token_is_vanilla_not_the_training_printing() {
        for (_, selector) in HUMANS_GODZILLA_HORDE_TOKENS {
            let preset = crate::game::token_presets::known_token_preset_by_id(selector)
                .expect("pinned token preset id must exist in the catalog");
            assert!(
                preset.rules_text.is_none(),
                "{selector} must be the vanilla body, but carries rules text: {:?}",
                preset.rules_text
            );
            assert!(
                preset.body.keywords.is_empty(),
                "{selector} must be the vanilla body, but carries keywords: {:?}",
                preset.body.keywords
            );
        }
    }
}
