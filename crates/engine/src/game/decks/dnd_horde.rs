//! D&D Horde (community format, hordemagic.com) — the Level 1 "Ooze" library for
//! [`crate::types::format::ChallengeDeck::DndHorde`].
//!
//! Data, not logic. Unlike the Cyberman Horde (which leans on ~200 predefined,
//! uniquely-named Universes Beyond tokens), the D&D Horde is built almost
//! entirely from real, already-implemented cards, so its coverage is high and it
//! is a good "second deck" proof that the deck-agnostic spine takes a new
//! challenge deck as a pure addition.
//!
//! Wave rule: this deck uses `WaveTermination::UntilRarityAtLeast(Uncommon)` —
//! commons and tokens build the board and the first uncommon-or-better card caps
//! the wave (see `game::horde::maybe_reveal_next`). The per-card rarity that
//! drives that gate is PINNED here in [`card_rarity`], because the runtime
//! card-data export does not carry rarity (`CardFace::rarities` is always empty);
//! without a pinned rarity the wave could never find an uncommon-or-better card
//! and would reveal the entire library every turn.
//!
//! Scope: this module is the **Level 1 Ooze** library (100 cards). The full D&D
//! Horde escalates through three tiered libraries (Ooze → Goblin/Skeleton →
//! Giant/Dragon); multi-tier progression and the deck's homebrew rules (legendary
//! phase-out, post-combat activation) are follow-up slices.

/// The non-token cards, as `(count, exact card name)` pairs. Each name resolves
/// against the runtime card database (`CardDatabase::get_face_by_name`); the
/// injection in `deck_loading::load_horde_library` panics on any name that fails
/// to resolve so a transcription error surfaces immediately.
///
/// 62 non-token cards (+ 38 Ooze tokens = 100).
pub const DND_OOZE_NONTOKEN_CARDS: &[(u32, &str)] = &[
    // Common creatures (20).
    (18, "Expanding Ooze"),
    (2, "Baleful Beholder"),
    // Uncommon+ creatures (25) — each ends a wave when revealed.
    (6, "Acidic Slime"),
    (1, "Biogenic Ooze"),
    (13, "Gelatinous Cube"),
    (1, "Ravenous Slime"),
    (1, "Predator Ooze"),
    (2, "Sludge Monster"),
    (1, "Uchuulon"),
    // Legendary creatures (2).
    (1, "Aeve, Progenitor Ooze"),
    (1, "Slurrk, All-Ingesting"),
    // Spells (13).
    (1, "Arms of Hadar"),
    (1, "Creeping Renaissance"),
    (2, "Crippling Fear"),
    (1, "Power Word Kill"),
    (6, "Slime Against Humanity"),
    (2, "Split the Party"),
    // Enchantments (2).
    (1, "Gutter Grime"),
    (1, "March of the World Ooze"),
];

/// The rarity of each non-token card, for the `UntilRarityAtLeast(Uncommon)`
/// wave rule (a wave ends when an uncommon-or-better card is revealed).
///
/// PINNED here rather than read from the card DB: the runtime `card-data.json`
/// export does not carry rarity (`CardFace::rarities` is always empty), so the
/// wave has no other way to tell a common (wave filler) from an uncommon-or-
/// better card (wave ender). Values are each card's LOWEST rarity across all
/// printings (a card ever printed common counts as common), computed from
/// Scryfall bulk data — the same "most-common-printing" reading the original
/// `card_face.rarities.iter().min()` intended.
///
/// Every name in [`DND_OOZE_NONTOKEN_CARDS`] must appear here; the
/// `every_card_has_a_pinned_rarity` test enforces it, so a card added without a
/// rarity fails loudly instead of silently making the wave run forever.
pub fn card_rarity(name: &str) -> Option<crate::types::card::Rarity> {
    use crate::types::card::Rarity;
    Some(match name {
        "Expanding Ooze"
        | "Baleful Beholder"
        | "Gelatinous Cube"
        | "Arms of Hadar"
        | "Power Word Kill"
        | "Slime Against Humanity" => Rarity::Common,
        "Acidic Slime" | "Slurrk, All-Ingesting" | "Split the Party" => Rarity::Uncommon,
        "Biogenic Ooze"
        | "Ravenous Slime"
        | "Predator Ooze"
        | "Sludge Monster"
        | "Uchuulon"
        | "Aeve, Progenitor Ooze"
        | "Creeping Renaissance"
        | "Crippling Fear"
        | "Gutter Grime" => Rarity::Rare,
        "March of the World Ooze" => Rarity::Mythic,
        _ => return None,
    })
}

/// The predefined tokens, as `(count, token selector)` pairs. The selector is a
/// preset **id** (a UUID) rather than a display name: "Ooze" has six distinct
/// same-name bodies in the catalog (1/1, 2/2, 3/3, */*, …), so
/// `known_token_body_by_name("Ooze")` is ambiguous and returns `None`. Pinning
/// the id resolves the intended body deterministically. `load_horde_library`
/// resolves a selector by name first, then falls back to preset id.
///
/// The intended token is the self-replicating Ooze — a 2/2 green Ooze with
/// "When this creature dies, create two 1/1 green Ooze creature tokens"
/// (Scryfall SLD #2819; the same body is catalogued from its earlier M11 / PCA /
/// NCC printings, which is what the id below points at — SLD is an art reprint
/// and is not itself in the preset catalog). The dies trigger is what makes this
/// deck's swarm resilient, so it is load-bearing, NOT flavor: the plain 2/2
/// vanilla Ooze is a different card and must not be substituted.
///
/// 38 Ooze tokens.
pub const DND_OOZE_TOKENS: &[(u32, &str)] = &[
    // 2/2 green Ooze with the "dies → two 1/1 Oozes" trigger (M11 #5 printing of
    // the SLD #2819 body). See module doc for why an id, not the name "Ooze".
    (38, "6d30428c-f846-584a-8458-55de11d00213"),
];

/// Total non-token card count (for coverage assertions / library-size sanity).
pub fn nontoken_card_count() -> u32 {
    DND_OOZE_NONTOKEN_CARDS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

/// Total token count.
pub fn token_count() -> u32 {
    DND_OOZE_TOKENS.iter().map(|(count, _)| *count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ooze_library_is_one_hundred_cards() {
        // The published Level 1 Ooze Horde is 100 cards (62 non-token + 38 token).
        assert_eq!(nontoken_card_count(), 62);
        assert_eq!(token_count(), 38);
        assert_eq!(nontoken_card_count() + token_count(), 100);
    }

    /// EVERY non-token card must have a pinned rarity. A card without one gets
    /// `None`, which the `UntilRarityAtLeast` wave treats as "never ends" — so a
    /// single missing entry makes the Horde reveal its ENTIRE library every turn
    /// (the live bug this pinning fixes). This is the guard that would have caught
    /// that regression.
    #[test]
    fn every_card_has_a_pinned_rarity() {
        for (_, name) in DND_OOZE_NONTOKEN_CARDS {
            assert!(
                card_rarity(name).is_some(),
                "D&D Horde card '{name}' has no pinned rarity — its wave would never end"
            );
        }
    }

    /// The wave can only terminate if at least one card is at or above the
    /// threshold (Uncommon). If every card were common, `UntilRarityAtLeast`
    /// would reveal the whole library — so assert the deck actually has enders.
    #[test]
    fn deck_has_uncommon_or_better_wave_enders() {
        use crate::types::card::Rarity;
        let enders = DND_OOZE_NONTOKEN_CARDS
            .iter()
            .filter(|(_, name)| card_rarity(name).is_some_and(|r| r >= Rarity::Uncommon))
            .count();
        assert!(
            enders > 0,
            "the D&D Horde must have uncommon-or-better cards to end its waves"
        );
    }

    /// Every token selector must resolve through the same name-then-preset-id
    /// fallback `deck_loading::load_horde_library` uses, so a mistyped or retired
    /// id fails here rather than panicking at game start.
    #[test]
    fn token_selectors_resolve_to_the_intended_body() {
        for (_, selector) in DND_OOZE_TOKENS {
            let body = crate::game::token_presets::known_token_body_by_name(selector)
                .or_else(|| {
                    crate::game::token_presets::known_token_preset_by_id(selector).map(|p| &p.body)
                })
                .unwrap_or_else(|| {
                    panic!("D&D Horde token selector '{selector}' must resolve to a body")
                });
            assert_eq!(body.display_name, "Ooze");
            assert_eq!(body.power, Some(2));
            assert_eq!(body.toughness, Some(2));
        }
    }

    /// Pin that the pinned id is the SELF-REPLICATING Ooze (the SLD #2819 body),
    /// not one of the catalog's several vanilla 2/2 Oozes. The dies trigger is
    /// load-bearing for this deck's swarm, so a silent swap to a vanilla body
    /// would change how the deck plays. That trigger IS installed on reveal (the
    /// library token carries this preset id, and `horde::reveal_library_token`
    /// materializes the catalog `rules_text` abilities — see
    /// `horde::tests::revealed_library_token_keeps_its_catalog_dies_trigger`);
    /// this test guards the intent by pinning that the id is the ability-bearing
    /// printing, so a re-pin to a vanilla body fails loudly.
    #[test]
    fn ooze_token_is_the_self_replicating_printing() {
        let (_, selector) = DND_OOZE_TOKENS[0];
        let preset = crate::game::token_presets::known_token_preset_by_id(selector)
            .expect("pinned Ooze preset id must exist in the catalog");
        let rules = preset
            .rules_text
            .as_deref()
            .expect("the self-replicating Ooze printing carries rules text");
        assert!(
            rules.contains("dies") && rules.contains("1/1 green Ooze"),
            "pinned Ooze must be the 'when this dies, create two 1/1 green Ooze' \
             printing, got: {rules:?}"
        );
    }
}
