//! Curated named creatures for scenario authoring.
//!
//! Real card names are used only where their printed P/T and keywords are
//! accurate; keyword-bearing archetypes that don't map cleanly to a well-known
//! vanilla card use descriptive synthetic names so no real card's Oracle text is
//! misrepresented.

use super::NamedCreature;

/// Vanilla 2/2 — the canonical "bear" test body.
pub const GRIZZLY_BEARS: NamedCreature = NamedCreature {
    name: "Grizzly Bears",
    power: 2,
    toughness: 2,
    keywords: &[],
};

/// Vanilla 2/1.
pub const SAVANNAH_LIONS: NamedCreature = NamedCreature {
    name: "Savannah Lions",
    power: 2,
    toughness: 1,
    keywords: &[],
};

/// Vanilla 2/2 — at exactly Lightning Bolt's 3 damage it still dies (boundary).
pub const GRAY_OGRE: NamedCreature = NamedCreature {
    name: "Gray Ogre",
    power: 2,
    toughness: 2,
    keywords: &[],
};

/// Synthetic zero-toughness body used to exercise CR 704.5f directly.
pub const ZERO_TOUGHNESS: NamedCreature = NamedCreature {
    name: "Zero Toughness",
    power: 1,
    toughness: 0,
    keywords: &[],
};

/// Real 2/2 flier — for flying/blocking-legality scenarios.
pub const WIND_DRAKE: NamedCreature = NamedCreature {
    name: "Wind Drake",
    power: 2,
    toughness: 2,
    keywords: &["Flying"],
};

/// Synthetic 4/4 trampler — excess-combat-damage scenarios.
pub const TEST_TRAMPLER: NamedCreature = NamedCreature {
    name: "Test Trampler",
    power: 4,
    toughness: 4,
    keywords: &["Trample"],
};

/// Real 1/1 deathtouch — CR 704.5h interaction scenarios.
pub const TYPHOID_RATS: NamedCreature = NamedCreature {
    name: "Typhoid Rats",
    power: 1,
    toughness: 1,
    keywords: &["Deathtouch"],
};

/// Synthetic indestructible 4/4 — CR 702.12 / survives lethal damage.
pub const TEST_INDESTRUCTIBLE: NamedCreature = NamedCreature {
    name: "Test Indestructible",
    power: 4,
    toughness: 4,
    keywords: &["Indestructible"],
};

/// All curated creatures, for lookup/iteration.
pub const ALL: &[NamedCreature] = &[
    GRIZZLY_BEARS,
    SAVANNAH_LIONS,
    GRAY_OGRE,
    ZERO_TOUGHNESS,
    WIND_DRAKE,
    TEST_TRAMPLER,
    TYPHOID_RATS,
    TEST_INDESTRUCTIBLE,
];
