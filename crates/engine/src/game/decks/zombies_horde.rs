//! Zombies Horde (community format, hordemagic.com) — the decklist for
//! [`crate::types::format::ChallengeDeck::ZombiesHorde`].
//!
//! Data, not logic. 100 non-token cards plus 200 predefined Zombie / Zombie Giant
//! tokens, for a 300-card library — the same shape as the Cyberman Horde.
//!
//! Every card here is a real, already-implemented Magic card (all 38 distinct
//! names resolve against the card DB and none parse to an `Unimplemented`
//! effect), and both tokens are vanilla bodies catalogued at `fidelity = Full`.
//! That makes this deck fully playable on the existing spine.
//!
//! Wave rule: `WaveTermination::UntilNonToken` with a
//! [`crate::types::format::WaveCount::Snaking`] count of 1..=3. Per the published
//! rules the Horde "will Wave 1, putting out Tokens until it flips its first
//! nontoken card. Then ... Wave 2 ... and so on for Wave 3. On its next turn, it
//! will descend back down to Wave 2, snaking back and forth."

/// The non-token cards, as `(count, exact card name)` pairs. Each name resolves
/// against the runtime card database (`CardDatabase::get_face_by_name`); the
/// injection in `deck_loading::load_horde_library` panics on any name that fails
/// to resolve so a transcription error surfaces immediately.
///
/// 79 creatures + 21 spells/permanents = 100 non-token cards.
pub const ZOMBIES_HORDE_NONTOKEN_CARDS: &[(u32, &str)] = &[
    // Creatures (79).
    (3, "Corpse Knight"),
    (6, "Death Baron"),
    (5, "Diregraf Captain"),
    (3, "Dread Slaver"),
    (2, "Eternal Skylord"),
    (4, "Fleshbag Marauder"),
    (2, "Ghoultree"),
    (1, "Gleaming Overseer"),
    (2, "Grave Titan"),
    (3, "Gray Merchant of Asphodel"),
    (1, "Josu Vess, Lich Knight"),
    (2, "Lotleth Giant"),
    (1, "Mournwillow"),
    (3, "Noosegraf Mob"),
    (2, "Noxious Ghoul"),
    (4, "Plague Belcher"),
    (3, "Soulless One"),
    (5, "Unbreathing Horde"),
    (2, "Undead Alchemist"),
    (18, "Undead Servant"),
    (4, "Undead Warchief"),
    (1, "Vengeful Pharaoh"),
    (2, "Vulturous Zombie"),
    // Spells and non-creature permanents (21).
    (2, "Zombie Apocalypse"),
    (1, "End Hostilities"),
    (1, "Endless Ranks of the Dead"),
    (1, "Footbottom Feast"),
    (1, "Tainted Remedy"),
    (2, "Plague Wind"),
    (2, "Throne of the God-Pharaoh"),
    (1, "Graf Harvest"),
    (2, "Grave Betrayal"),
    (1, "Creeping Corrosion"),
    (1, "Aether Snap"),
    (1, "Army of the Damned"),
    (3, "Awaken the Erstwhile"),
    (1, "Barter in Blood"),
    (1, "Call to the Grave"),
];

/// The predefined tokens, as `(count, token selector)` pairs. Both selectors are
/// preset **ids**: "Zombie" maps to six distinct bodies in the catalog (2/2, 3/3,
/// 1/1, */*, plus Decayed and Menace variants), so a name lookup is ambiguous and
/// returns `None`. Pinning ids resolves the intended vanilla bodies
/// deterministically. `load_horde_library` resolves a selector by name first,
/// then falls back to preset id.
///
/// Both bodies are catalogued at `fidelity = Full` with no rules text, so they
/// round-trip through the library→battlefield reveal without losing anything.
///
/// 187 Zombie + 13 Zombie Giant = 200 tokens.
pub const ZOMBIES_HORDE_TOKENS: &[(u32, &str)] = &[
    // 2/2 black Zombie, vanilla (M11 #3 body — the canonical Zombie token).
    (187, "011a9246-7f7c-50c7-ab99-3fc13469c13b"),
    // 5/5 black Zombie Giant, vanilla (BBD #4 — the catalog's only such body).
    (13, "2b955244-b333-5888-916c-7013fa64a011"),
];

/// Total non-token card count (for coverage assertions / library-size sanity).
pub fn nontoken_card_count() -> u32 {
    ZOMBIES_HORDE_NONTOKEN_CARDS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

/// Total token count.
pub fn token_count() -> u32 {
    ZOMBIES_HORDE_TOKENS.iter().map(|(count, _)| *count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_is_three_hundred_cards() {
        // The published deck is 100 non-token + 200 token cards.
        assert_eq!(nontoken_card_count(), 100);
        assert_eq!(token_count(), 200);
        assert_eq!(nontoken_card_count() + token_count(), 300);
    }

    /// Every token selector must resolve through the same name-then-preset-id
    /// fallback `deck_loading::load_horde_library` uses, so a mistyped or retired
    /// id fails here rather than panicking at game start.
    #[test]
    fn token_selectors_resolve_to_vanilla_zombie_bodies() {
        let expected = [("Zombie", 2, 2), ("Zombie Giant", 5, 5)];
        for ((_, selector), (name, power, toughness)) in ZOMBIES_HORDE_TOKENS.iter().zip(expected) {
            let body = crate::game::token_presets::known_token_body_by_name(selector)
                .or_else(|| {
                    crate::game::token_presets::known_token_preset_by_id(selector).map(|p| &p.body)
                })
                .unwrap_or_else(|| {
                    panic!("Zombies Horde token selector '{selector}' must resolve to a body")
                });
            assert_eq!(body.display_name, name);
            assert_eq!(body.power, Some(power));
            assert_eq!(body.toughness, Some(toughness));
        }
    }

    /// Both tokens must be VANILLA. `TokenCharacteristics` carries no channel for
    /// non-keyword abilities, so a token with printed rules text would silently
    /// enter the battlefield without them (the tracked token-abilities gap). This
    /// deck is playable today precisely because neither token needs one — pin
    /// that, so a future re-pin to an ability-bearing printing fails loudly.
    #[test]
    fn tokens_are_vanilla_so_nothing_is_silently_dropped() {
        for (_, selector) in ZOMBIES_HORDE_TOKENS {
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
