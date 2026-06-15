//! Layer 1 — Features: dumb structural data extracted from a deck.
//!
//! Each feature describes a class of cards or strategic axis present in a deck,
//! computed once per game. Features are pure data — detection happens
//! structurally over `CardFace` triggers, effects, and filters (no card-name
//! matching). See `features/tests/no_name_matching.rs` for the enforced
//! anti-pattern lint.

pub mod aggro_pressure;
pub mod aristocrats;
pub mod commitment;
pub mod control;
pub mod landfall;
pub mod lifegain;
pub mod mana_ramp;
pub mod plus_one_counters;
pub mod spellslinger_prowess;
pub mod tokens_wide;
pub mod tribal;

#[cfg(test)]
pub mod tests;

pub use aggro_pressure::AggroPressureFeature;
pub use aristocrats::AristocratsFeature;
pub use control::ControlFeature;
pub use landfall::LandfallFeature;
pub use lifegain::LifegainFeature;
pub use mana_ramp::ManaRampFeature;
pub use plus_one_counters::PlusOneCountersFeature;
pub use spellslinger_prowess::SpellslingerProwessFeature;
pub use tokens_wide::TokensWideFeature;
pub use tribal::TribalFeature;

use engine::game::bracket_estimate::CommanderBracketTier;

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
    pub lifegain: LifegainFeature,
    pub mana_ramp: ManaRampFeature,
    pub tribal: TribalFeature,
    pub control: ControlFeature,
    pub aristocrats: AristocratsFeature,
    pub aggro_pressure: AggroPressureFeature,
    pub tokens_wide: TokensWideFeature,
    pub plus_one_counters: PlusOneCountersFeature,
    pub spellslinger_prowess: SpellslingerProwessFeature,
    /// Declaration-derived: the deck's declared bracket tier. Unlike the
    /// other fields here, this is not structurally detected from card text —
    /// it is a per-deck declaration set at deck-analysis time from deck
    /// metadata. Stored as the full `CommanderBracketTier` (not a `bool`) so
    /// the design space stays open: `ComboLinePolicy::activation()` and
    /// `CedhKeepablesMulligan` gate on `== Cedh` today, but bracket-aware
    /// behavior for other tiers can read the same field without a new flag.
    pub bracket_tier: CommanderBracketTier,
}

impl DeckFeatures {
    /// Construct `DeckFeatures` from a deck. Walks each per-class detector
    /// (`landfall::detect`, `mana_ramp::detect`, ...) and records the declared
    /// `bracket_tier`.
    ///
    /// Per-class detectors are pure functions over `&[DeckEntry]`. The tier
    /// argument flows in from deck metadata at the AI-setup boundary.
    pub fn analyze(deck: &[engine::game::DeckEntry], tier: CommanderBracketTier) -> Self {
        let profile = crate::deck_profile::DeckProfile::analyze(deck);
        let archetype = match &profile.classification {
            crate::deck_profile::ArchetypeClassification::Pure(arch) => *arch,
            crate::deck_profile::ArchetypeClassification::Hybrid { primary, .. } => *primary,
        };
        let strategy = crate::strategy_profile::StrategyProfile::for_profile(&profile);
        Self {
            archetype,
            strategy,
            landfall: landfall::detect(deck),
            lifegain: lifegain::detect(deck),
            mana_ramp: mana_ramp::detect(deck),
            tribal: tribal::detect(deck),
            control: control::detect(deck),
            aristocrats: aristocrats::detect(deck),
            aggro_pressure: aggro_pressure::detect(deck),
            tokens_wide: tokens_wide::detect(deck),
            plus_one_counters: plus_one_counters::detect(deck),
            spellslinger_prowess: spellslinger_prowess::detect(deck),
            bracket_tier: tier,
        }
    }
}

#[cfg(test)]
mod cedh_field_tests {
    use super::*;

    #[test]
    fn default_features_tier_is_not_cedh() {
        let f = DeckFeatures::default();
        assert_ne!(f.bracket_tier, CommanderBracketTier::Cedh);
    }

    #[test]
    fn analyze_records_cedh_tier() {
        // Use an empty deck — structural features default to zero; bracket_tier
        // should follow only the tier argument.
        let f = DeckFeatures::analyze(&[], CommanderBracketTier::Cedh);
        assert_eq!(f.bracket_tier, CommanderBracketTier::Cedh);
    }

    #[test]
    fn analyze_records_non_cedh_tier() {
        for tier in [
            CommanderBracketTier::Exhibition,
            CommanderBracketTier::Core,
            CommanderBracketTier::Upgraded,
            CommanderBracketTier::Optimized,
        ] {
            let f = DeckFeatures::analyze(&[], tier);
            assert_eq!(f.bracket_tier, tier);
            assert_ne!(f.bracket_tier, CommanderBracketTier::Cedh);
        }
    }
}
