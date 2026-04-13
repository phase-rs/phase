//! Layer 1 — Features: dumb structural data extracted from a deck.
//!
//! Each feature describes a class of cards or strategic axis present in a deck,
//! computed once per game. Features are pure data — detection happens
//! structurally over `CardFace` triggers, effects, and filters (no card-name
//! matching). See `features/tests/no_name_matching.rs` for the enforced
//! anti-pattern lint.

pub mod aggro_pressure;
pub mod aristocrats;
pub mod control;
pub mod landfall;
pub mod mana_ramp;
pub mod plus_one_counters;
pub mod tokens_wide;
pub mod tribal;

#[cfg(test)]
pub mod tests;

pub use aggro_pressure::AggroPressureFeature;
pub use aristocrats::AristocratsFeature;
pub use control::ControlFeature;
pub use landfall::LandfallFeature;
pub use mana_ramp::ManaRampFeature;
pub use plus_one_counters::PlusOneCountersFeature;
pub use tokens_wide::TokensWideFeature;
pub use tribal::TribalFeature;

use crate::deck_profile::DeckArchetype;
use crate::strategy_profile::StrategyProfile;

/// Aggregated structural features detected from a single player's deck.
///
/// Carries the deck's strategic archetype + strategy profile alongside the
/// per-class feature data — policies use these in `activation()` to compute
/// archetype- and turn-phase-sensitive weighting without consulting
/// `AiContext` directly.
#[derive(Debug, Clone, Default)]
pub struct DeckFeatures {
    pub archetype: DeckArchetype,
    pub strategy: StrategyProfile,
    pub landfall: LandfallFeature,
    pub mana_ramp: ManaRampFeature,
    pub tribal: TribalFeature,
    pub control: ControlFeature,
    pub aristocrats: AristocratsFeature,
    pub aggro_pressure: AggroPressureFeature,
    pub tokens_wide: TokensWideFeature,
    pub plus_one_counters: PlusOneCountersFeature,
}
