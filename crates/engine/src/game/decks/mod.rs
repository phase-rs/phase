//! Concrete challenge-deck decklists for the Horde format.
//!
//! Each module is pure DATA — the (count, name) card lists and token bodies that
//! make up one [`crate::types::format::ChallengeDeck`]. The generic Horde spine
//! (`game::horde`) and the seat-scoped injection (`game::deck_loading`) consume
//! these; a new deck is a peer module here plus one `ChallengeDeck` variant.
pub mod cyberman_horde;
pub mod dnd_horde;
pub mod humans_godzilla_horde;
// LOTR "Two Towers" two-Horde decks (Sauron + Saruman). Verified card lists,
// wired to `ChallengeDeck::SauronHorde` / `ChallengeDeck::SarumanHorde` (plus
// the combined `ChallengeDeck::LotrTwoTowersHorde` picker identity): the
// uncommon+ wave reads per-card rarity pinned in each module, revealed Orc
// Army tokens amass one shared army, and milled legendaries recur via
// `HordeLegendaryDeath::EtbThenPhaseOut`. One gap remains: Saruman's four The
// Hobbit (`hob`) cards are not yet in the card DB snapshot and will fail to
// resolve until the DB is regenerated to include that set (see the NOTE on
// `saruman_horde::SARUMAN_HORDE_NONTOKEN_CARDS`).
pub mod saruman_horde;
pub mod sauron_horde;
pub mod slivers_horde;
pub mod turtle_teamup_horde;
pub mod zombies_horde;
