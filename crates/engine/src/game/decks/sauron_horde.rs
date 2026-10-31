//! The Lord of the Rings "Sauron, the Dark Lord Horde" — the concrete decklist
//! for the `ChallengeDeck::SauronHorde` Horde seat.
//!
//! Data, not logic. The Horde seat's engine-supplied library is ~300 cards:
//! real Universes Beyond (LTR / LTC) non-token cards resolved from the card DB
//! by name, plus predefined Orc Army / Wraith tokens materialized from
//! `game::token_presets::known_token_body_by_name`. Tokens can never be cast
//! (CR 111), so they wait in the library and enter the battlefield when revealed
//! (see `game::horde::reveal_library_token`); non-tokens are free-cast on reveal.

/// The non-token cards, as `(count, exact card name)` pairs. Each name resolves
/// against the runtime card database (`CardDatabase::get_face_by_name`); the
/// injection in `deck_loading::load_horde_library` panics on any name that fails
/// to resolve so a transcription error surfaces immediately rather than shipping
/// a silently-truncated library.
///
/// 170 creatures (incl. the legendary "boss" cards + commander) + 23 spells +
/// 1 artifact + 4 enchantments = 198 non-token cards.
pub const SAURON_HORDE_NONTOKEN_CARDS: &[(u32, &str)] = &[
    // Commander + legendary "boss" cards (18).
    (1, "Sauron, the Dark Lord"),
    (1, "Sauron, Lord of the Rings"),
    (1, "Sauron, the Lidless Eye"),
    (1, "Sauron, the Necromancer"),
    (1, "Witch-king of Angmar"),
    (1, "Witch-king, Bringer of Ruin"),
    (1, "Grond, the Gatebreaker"),
    (1, "Lord of the Nazgûl"),
    (1, "The One Ring"),
    (1, "Shelob, Child of Ungoliant"),
    (4, "Gothmog, Morgul Lieutenant"),
    (4, "Mauhúr, Uruk-hai Captain"),
    // Creatures (152).
    (18, "Warg Rider"),
    (11, "Corsairs of Umbar"),
    (3, "Gruesome Scourger"),
    (11, "Dunland Crebain"),
    (3, "Easterling Vanguard"),
    (1, "Goblin Cratermaker"),
    (3, "Grishnákh, Brash Instigator"),
    (1, "Hostage Taker"),
    (1, "Inferno Titan"),
    (11, "Merciless Executioner"),
    (6, "Mordor Trebuchet"),
    (5, "Moria Marauder"),
    (9, "Nazgûl"),
    (8, "Oliphaunt"),
    (4, "Olog-hai Crusher"),
    (14, "Orcish Siegemaster"),
    (1, "Rampaging War Mammoth"),
    (3, "Ringwraiths"),
    (1, "Scourge of the Throne"),
    (6, "Troll of Khazad-dûm"),
    (10, "Uruk-hai Berserker"),
    (10, "Voracious Fell Beast"),
    (12, "Warbeast of Gorgoroth"),
    // Instants / sorceries (23).
    (1, "Living Death"),
    (1, "Decree of Pain"),
    (1, "Horses of the Bruinen"),
    (6, "Sam's Desperate Rescue"),
    (4, "The Black Breath"),
    (4, "Claim the Precious"),
    (2, "Breaking of the Fellowship"),
    (4, "Ranger's Firebrand"),
    // Artifacts (1).
    (1, "Thorn of Amethyst"),
    // Enchantments (4).
    (3, "One Ring to Rule Them All"),
    (1, "In the Darkness Bind Them"),
];

/// The rarity of each non-token card, for the `UntilRarityAtLeast(Uncommon)`
/// wave rule (a wave ends when an uncommon-or-better card is revealed).
///
/// PINNED here rather than read from the card DB: the runtime `card-data.json`
/// export does not carry rarity (`CardFace::rarities` is always empty), so the
/// wave has no other way to tell a common (wave filler) from an uncommon-or-
/// better card (wave ender). Values are each card's LOWEST rarity across all
/// printings (a card ever printed common counts as common), resolved from
/// Scryfall bulk data — the same "most-common-printing" reading the original
/// `card_face.rarities.iter().min()` intended.
///
/// Every name in [`SAURON_HORDE_NONTOKEN_CARDS`] must appear here; the
/// `every_card_has_a_pinned_rarity` test enforces it, so a card added without a
/// rarity fails loudly instead of silently making the wave run forever.
pub fn card_rarity(name: &str) -> Option<crate::types::card::Rarity> {
    use crate::types::card::Rarity;
    Some(match name {
        "Dunland Crebain"
        | "Easterling Vanguard"
        | "Mordor Trebuchet"
        | "Oliphaunt"
        | "Olog-hai Crusher"
        | "Troll of Khazad-dûm"
        | "Uruk-hai Berserker"
        | "Warbeast of Gorgoroth"
        | "Sam's Desperate Rescue"
        | "The Black Breath"
        | "Claim the Precious"
        | "Breaking of the Fellowship" => Rarity::Common,
        "Grond, the Gatebreaker"
        | "Gothmog, Morgul Lieutenant"
        | "Mauhúr, Uruk-hai Captain"
        | "Gruesome Scourger"
        | "Goblin Cratermaker"
        | "Grishnákh, Brash Instigator"
        | "Merciless Executioner"
        | "Nazgûl"
        | "Voracious Fell Beast"
        | "Horses of the Bruinen"
        | "Ranger's Firebrand" => Rarity::Uncommon,
        "Sauron, the Necromancer"
        | "Witch-king, Bringer of Ruin"
        | "Lord of the Nazgûl"
        | "Shelob, Child of Ungoliant"
        | "Warg Rider"
        | "Corsairs of Umbar"
        | "Hostage Taker"
        | "Inferno Titan"
        | "Moria Marauder"
        | "Orcish Siegemaster"
        | "Rampaging War Mammoth"
        | "Ringwraiths"
        | "Scourge of the Throne"
        | "Living Death"
        | "Decree of Pain"
        | "Thorn of Amethyst"
        | "One Ring to Rule Them All"
        | "In the Darkness Bind Them" => Rarity::Rare,
        "Sauron, the Dark Lord"
        | "Sauron, Lord of the Rings"
        | "Sauron, the Lidless Eye"
        | "Witch-king of Angmar"
        | "The One Ring" => Rarity::Mythic,
        _ => return None,
    })
}

/// The predefined tokens, as `(count, preset token name)` pairs. Each name is
/// resolved to a `TokenCharacteristics` body via
/// `token_presets::known_token_body_by_name`; the body is materialized into a
/// library-resident token object (see `deck_loading::create_horde_library_token`).
///
/// 77 Orc Army + 28 Wraith = 105 tokens.
pub const SAURON_HORDE_TOKENS: &[(u32, &str)] = &[(77, "Orc Army"), (28, "Wraith")];

/// Total non-token card count (for coverage assertions / library-size sanity).
pub fn nontoken_card_count() -> u32 {
    SAURON_HORDE_NONTOKEN_CARDS
        .iter()
        .map(|(count, _)| *count)
        .sum()
}

/// Total token count.
pub fn token_count() -> u32 {
    SAURON_HORDE_TOKENS.iter().map(|(count, _)| *count).sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decklist_totals_are_303() {
        assert_eq!(nontoken_card_count(), 198, "198 non-token cards");
        assert_eq!(token_count(), 105, "105 tokens");
        assert_eq!(nontoken_card_count() + token_count(), 303);
    }

    /// EVERY non-token card must have a pinned rarity. A card without one gets
    /// `None`, which the `UntilRarityAtLeast` wave treats as "never ends" — so a
    /// single missing entry makes the Horde reveal its ENTIRE library every turn.
    /// This is the guard that catches that regression.
    #[test]
    fn every_card_has_a_pinned_rarity() {
        for (_, name) in SAURON_HORDE_NONTOKEN_CARDS {
            assert!(
                card_rarity(name).is_some(),
                "Sauron Horde card '{name}' has no pinned rarity — its wave would never end"
            );
        }
    }

    /// The wave can only terminate if at least one card is at or above the
    /// threshold (Uncommon). If every card were common, `UntilRarityAtLeast`
    /// would reveal the whole library — so assert the deck actually has enders.
    #[test]
    fn deck_has_uncommon_or_better_wave_enders() {
        use crate::types::card::Rarity;
        let enders = SAURON_HORDE_NONTOKEN_CARDS
            .iter()
            .filter(|(_, name)| card_rarity(name).is_some_and(|r| r >= Rarity::Uncommon))
            .count();
        assert!(
            enders > 0,
            "the Sauron Horde must have uncommon-or-better cards to end its waves"
        );
    }

    /// Every token preset name must resolve to a single unambiguous body — the
    /// injection depends on `known_token_body_by_name` returning `Some`.
    #[test]
    fn token_preset_names_resolve() {
        for (_, name) in SAURON_HORDE_TOKENS {
            assert!(
                crate::game::token_presets::known_token_body_by_name(name).is_some(),
                "token preset '{name}' must resolve to a unique body"
            );
        }
    }
}
