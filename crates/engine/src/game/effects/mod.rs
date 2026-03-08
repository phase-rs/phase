use std::collections::HashMap;

use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

pub mod change_zone;
pub mod counter;
pub mod counters;
pub mod deal_damage;
pub mod destroy;
pub mod discard;
pub mod draw;
pub mod life;
pub mod pump;
pub mod sacrifice;
pub mod tap_untap;
pub mod token;

/// A function pointer type for effect handlers.
pub type EffectHandler = fn(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError>;

/// Build the registry mapping api_type strings to handler functions.
pub fn build_registry() -> HashMap<String, EffectHandler> {
    let mut registry: HashMap<String, EffectHandler> = HashMap::new();
    registry.insert("DealDamage".to_string(), deal_damage::resolve);
    registry.insert("Draw".to_string(), draw::resolve);
    registry.insert("ChangeZone".to_string(), change_zone::resolve);
    registry.insert("Pump".to_string(), pump::resolve);
    registry.insert("Destroy".to_string(), destroy::resolve);
    registry.insert("Counter".to_string(), counter::resolve);
    registry.insert("Token".to_string(), token::resolve);
    registry.insert("GainLife".to_string(), life::resolve_gain);
    registry.insert("LoseLife".to_string(), life::resolve_lose);
    registry.insert("Tap".to_string(), tap_untap::resolve_tap);
    registry.insert("Untap".to_string(), tap_untap::resolve_untap);
    registry.insert("AddCounter".to_string(), counters::resolve_add);
    registry.insert("RemoveCounter".to_string(), counters::resolve_remove);
    registry.insert("Sacrifice".to_string(), sacrifice::resolve);
    registry.insert("DiscardCard".to_string(), discard::resolve);
    registry
}

/// Look up the api_type in the registry and call the handler.
pub fn resolve_effect(
    registry: &HashMap<String, EffectHandler>,
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let handler = registry
        .get(&ability.api_type)
        .ok_or_else(|| EffectError::Unregistered(ability.api_type.clone()))?;
    handler(state, ability, events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn make_ability(api_type: &str) -> ResolvedAbility {
        ResolvedAbility {
            api_type: api_type.to_string(),
            params: HashMap::new(),
            targets: vec![],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        }
    }

    #[test]
    fn registry_has_15_entries() {
        let registry = build_registry();
        assert_eq!(registry.len(), 15);
    }

    #[test]
    fn registry_contains_all_effect_types() {
        let registry = build_registry();
        let expected = [
            "DealDamage",
            "Draw",
            "ChangeZone",
            "Pump",
            "Destroy",
            "Counter",
            "Token",
            "GainLife",
            "LoseLife",
            "Tap",
            "Untap",
            "AddCounter",
            "RemoveCounter",
            "Sacrifice",
            "DiscardCard",
        ];
        for name in &expected {
            assert!(registry.contains_key(*name), "missing: {}", name);
        }
    }

    #[test]
    fn resolve_effect_returns_unregistered_for_unknown() {
        let registry = build_registry();
        let mut state = GameState::new_two_player(42);
        let ability = make_ability("NonExistentEffect");
        let mut events = Vec::new();
        let result = resolve_effect(&registry, &mut state, &ability, &mut events);
        assert_eq!(
            result,
            Err(EffectError::Unregistered("NonExistentEffect".to_string()))
        );
    }
}
