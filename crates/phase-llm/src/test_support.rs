//! Shared fixtures for this crate's tests.

use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind, Effect, QuantityExpr};

/// A mana rock's ability: `{T}: Add {C}` × `count` (Sol Ring at 2).
pub(crate) fn mana_rock_ability(count: i32) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: engine::types::ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: count },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap)
}
