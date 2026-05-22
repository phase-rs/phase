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
pub mod spellslinger_prowess;
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
pub use spellslinger_prowess::SpellslingerProwessFeature;
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
    pub spellslinger_prowess: SpellslingerProwessFeature,
    /// Declaration-derived: `true` iff the deck's declared bracket tier is
    /// `CommanderBracketTier::Cedh`. Unlike the other fields here, this is
    /// not structurally detected from card text — it is a per-deck
    /// declaration set at deck-analysis time from deck metadata. Used by
    /// `ComboLinePolicy::activation()` and `CedhKeepablesMulligan` as a
    /// gating signal.
    pub is_cedh: bool,
}

impl DeckFeatures {
    /// Construct `DeckFeatures` from a deck. Walks each per-class detector
    /// (`landfall::detect`, `mana_ramp::detect`, ...) and sets `is_cedh`
    /// from the declared bracket tier.
    ///
    /// Per-class detectors are pure functions over `&[DeckEntry]`. The tier
    /// argument flows in from deck metadata at the AI-setup boundary.
    pub fn analyze(
        deck: &[engine::game::DeckEntry],
        tier: engine::game::bracket_estimate::CommanderBracketTier,
    ) -> Self {
        use engine::game::bracket_estimate::CommanderBracketTier;
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
            mana_ramp: mana_ramp::detect(deck),
            tribal: tribal::detect(deck),
            control: control::detect(deck),
            aristocrats: aristocrats::detect(deck),
            aggro_pressure: aggro_pressure::detect(deck),
            tokens_wide: tokens_wide::detect(deck),
            plus_one_counters: plus_one_counters::detect(deck),
            spellslinger_prowess: spellslinger_prowess::detect(deck),
            is_cedh: tier == CommanderBracketTier::Cedh,
        }
    }
}

#[cfg(test)]
mod cedh_field_tests {
    use super::*;

    #[test]
    fn default_features_is_not_cedh() {
        let f = DeckFeatures::default();
        assert!(!f.is_cedh);
    }

    #[test]
    fn analyze_with_cedh_tier_sets_is_cedh() {
        use engine::game::bracket_estimate::CommanderBracketTier;
        // Use an empty deck — structural features default to zero; is_cedh
        // should follow only the tier argument.
        let f = DeckFeatures::analyze(&[], CommanderBracketTier::Cedh);
        assert!(f.is_cedh, "Cedh tier must set is_cedh = true");
    }

    #[test]
    fn analyze_with_non_cedh_tier_leaves_is_cedh_false() {
        use engine::game::bracket_estimate::CommanderBracketTier;
        for tier in [
            CommanderBracketTier::Exhibition,
            CommanderBracketTier::Core,
            CommanderBracketTier::Upgraded,
            CommanderBracketTier::Optimized,
        ] {
            let f = DeckFeatures::analyze(&[], tier);
            assert!(
                !f.is_cedh,
                "non-Cedh tier ({tier:?}) must leave is_cedh = false"
            );
        }
    }
}
