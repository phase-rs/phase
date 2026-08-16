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
            excess: None,
        },
    ))
}

/// The five distinct filter shapes carried by an `Effect::Mana` target in the
/// shipping card set, each paired with the role the parser stamps. Ten of the
/// eleven fixture entries are CONTEXT-REF recipients; Carpet of Flowers is the
/// sole count source. Canonical home shared by the `ability_rw` and
/// `mana_abilities` test modules so the two role matrices cannot drift.
pub(crate) fn mana_fixture_roles() -> Vec<(&'static str, crate::types::ability::ManaTargetRole)> {
    use crate::types::ability::{ControllerRef, ManaTargetRole, TargetFilter, TypedFilter};
    let recipient = |f: TargetFilter| ManaTargetRole::Recipient { recipient: f };
    vec![
        (
            "Belbe / Blinkmoth Urn",
            recipient(TargetFilter::ScopedPlayer),
        ),
        (
            "Bubbling Muck / High Tide / Mana Flare",
            recipient(TargetFilter::TriggeringPlayer),
        ),
        (
            "Fertile Ground / Utopia Sprawl / Wild Growth / Shimmerwilds Growth",
            recipient(TargetFilter::ParentTargetController),
        ),
        (
            "Spectral Searchlight",
            recipient(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::ChosenPlayer { index: 0 }),
            )),
        ),
        (
            "Carpet of Flowers",
            ManaTargetRole::CountSource {
                count_source: TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                ),
            },
        ),
    ]
}
