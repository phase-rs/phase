//! Curated named spells for scenario authoring.
//!
//! Only Lightning Bolt has a production cast step in the runner today (via
//! `GameScenario::add_bolt_to_hand` → `Effect::DealDamage`). The remaining
//! entries document intended future cast steps and their expected transitions.

use super::NamedSpell;

/// The workhorse: 3 damage to any target for {R} (CR 120 / Effect::DealDamage).
pub const LIGHTNING_BOLT: NamedSpell = NamedSpell {
    name: "Lightning Bolt",
    mana_value: 1,
    oracle: "Lightning Bolt deals 3 damage to any target.",
};

/// 3 damage to any target for {1}{R} — a sorcery-speed-free burn variant (future step).
pub const LIGHTNING_STRIKE: NamedSpell = NamedSpell {
    name: "Lightning Strike",
    mana_value: 2,
    oracle: "Lightning Strike deals 3 damage to any target.",
};

/// Prevent-all-combat-damage this turn (future prevention scenarios, CR 615).
pub const FOG: NamedSpell = NamedSpell {
    name: "Fog",
    mana_value: 1,
    oracle: "Prevent all combat damage that would be dealt this turn.",
};

/// All curated spells.
pub const ALL: &[NamedSpell] = &[LIGHTNING_BOLT, LIGHTNING_STRIKE, FOG];
