//! Shared ability/card fixtures for engine unit tests.
//!
//! Kept behind `#[cfg(test)]` so these helpers don't bloat production builds.
//! Prefer adding fixtures here instead of duplicating them across per-module
//! test submodules.

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
    QuantityExpr, TargetFilter,
};
use crate::types::mana::ManaColor;

/// Brushland's colored mana ability: `{T}: Add {G} or {W}.` with a damage
/// continuation `~ deals 1 damage to you.` The damage sub-ability is the
/// canonical painland pattern — `AbilityKind::Spell` (resolution continuation,
/// not independently activatable) with `Effect::DealDamage` targeting
/// `TargetFilter::Controller`.
pub(crate) fn brushland_colored_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![ManaColor::Green, ManaColor::White],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap)
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
            damage_source: None,
        },
    ))
}
