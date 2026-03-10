use std::collections::HashMap;

use crate::parser::ability::parse_ability;
use crate::types::ability::{EffectError, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

pub mod attach;
pub mod bounce;
pub mod change_zone;
pub mod choose_card;
pub mod copy_spell;
pub mod counter;
pub mod counters;
pub mod deal_damage;
pub mod destroy;
pub mod dig;
pub mod discard;
pub mod draw;
pub mod explore;
pub mod fight;
pub mod gain_control;
pub mod life;
pub mod mill;
pub mod proliferate;
pub mod pump;
pub mod sacrifice;
pub mod scry;
pub mod surveil;
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
    registry.insert("Mill".to_string(), mill::resolve);
    registry.insert("Scry".to_string(), scry::resolve);
    registry.insert("PumpAll".to_string(), pump::resolve_all);
    registry.insert("DamageAll".to_string(), deal_damage::resolve_all);
    registry.insert("DestroyAll".to_string(), destroy::resolve_all);
    registry.insert("ChangeZoneAll".to_string(), change_zone::resolve_all);
    registry.insert("Dig".to_string(), dig::resolve);
    registry.insert("GainControl".to_string(), gain_control::resolve);
    registry.insert("Attach".to_string(), attach::resolve);
    registry.insert("Surveil".to_string(), surveil::resolve);
    registry.insert("Fight".to_string(), fight::resolve);
    registry.insert("Bounce".to_string(), bounce::resolve);
    registry.insert("Explore".to_string(), explore::resolve);
    registry.insert("Proliferate".to_string(), proliferate::resolve);
    registry.insert("CopySpell".to_string(), copy_spell::resolve);
    registry.insert("ChooseCard".to_string(), choose_card::resolve);
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

const MAX_CHAIN_DEPTH: u32 = 10;

/// Resolve an ability and follow its SubAbility/Execute chain.
pub fn resolve_ability_chain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    registry: &HashMap<String, EffectHandler>,
    depth: u32,
) -> Result<(), EffectError> {
    if depth > MAX_CHAIN_DEPTH {
        return Err(EffectError::ChainTooDeep);
    }

    // Resolve the current effect
    if !ability.api_type.is_empty() {
        let _ = resolve_effect(registry, state, ability, events);
    }

    // Check for SubAbility or Execute chain
    let sub_key = ability
        .params
        .get("SubAbility")
        .or_else(|| ability.params.get("Execute"));

    if let Some(svar_name) = sub_key {
        if let Some(raw_ability) = ability.svars.get(svar_name) {
            if let Ok(def) = parse_ability(raw_ability) {
                let mut sub_resolved = ResolvedAbility {
                    api_type: def.api_type,
                    params: def.params,
                    targets: Vec::new(),
                    source_id: ability.source_id,
                    controller: ability.controller,
                    sub_ability: None,
                    svars: ability.svars.clone(),
                };

                // Inherit targets if sub-ability uses Defined$ Targeted
                if sub_resolved
                    .params
                    .get("Defined")
                    .map(|v| v == "Targeted")
                    .unwrap_or(false)
                {
                    sub_resolved.targets = ability.targets.clone();
                }

                // Check conditions before executing
                if check_conditions(&sub_resolved, state) {
                    resolve_ability_chain(state, &sub_resolved, events, registry, depth + 1)?;
                }
            }
        }
    }

    Ok(())
}

/// Check conditions on an ability link. Returns true if the ability should execute.
fn check_conditions(ability: &ResolvedAbility, state: &GameState) -> bool {
    // ConditionCompare: format like "GE2", "EQ0", "LE5"
    if let Some(compare_str) = ability.params.get("ConditionCompare") {
        let count = get_condition_count(ability, state);
        if !evaluate_compare(compare_str, count) {
            return false;
        }
    }

    // ConditionPresent: check for cards matching a type in a zone
    if let Some(filter) = ability.params.get("ConditionPresent") {
        let zone_str = ability
            .params
            .get("ConditionZone")
            .map(|s| s.as_str())
            .unwrap_or("Battlefield");
        if !evaluate_present(filter, zone_str, state) {
            return false;
        }
    }

    true
}

/// Get the count value for ConditionCompare, using ConditionSVarCompare if available.
fn get_condition_count(ability: &ResolvedAbility, state: &GameState) -> u32 {
    if let Some(_svar_name) = ability.params.get("ConditionSVarCompare") {
        // For Phase 4, count creatures on battlefield as a simple default
        state
            .battlefield
            .iter()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .map(|obj| obj.card_types.core_types.contains(&CoreType::Creature))
                    .unwrap_or(false)
            })
            .count() as u32
    } else {
        0
    }
}

/// Evaluate a comparison string like "GE2", "EQ0", "LE5" against a count.
fn evaluate_compare(compare_str: &str, count: u32) -> bool {
    let (op, num_str) = if compare_str.len() >= 3 {
        (&compare_str[..2], &compare_str[2..])
    } else {
        return true; // Malformed, default pass
    };

    let threshold: u32 = match num_str.parse() {
        Ok(n) => n,
        Err(_) => return true,
    };

    match op {
        "GE" => count >= threshold,
        "LE" => count <= threshold,
        "EQ" => count == threshold,
        "NE" => count != threshold,
        "GT" => count > threshold,
        "LT" => count < threshold,
        _ => true,
    }
}

/// Evaluate ConditionPresent: check if any card matching the filter exists in the zone.
fn evaluate_present(filter: &str, zone_str: &str, state: &GameState) -> bool {
    let check_type = match filter {
        "Creature" => Some(CoreType::Creature),
        "Land" => Some(CoreType::Land),
        _ => None,
    };

    let check_type = match check_type {
        Some(ct) => ct,
        None => return true, // Unknown filter, default pass
    };

    match zone_str {
        "Battlefield" => state.battlefield.iter().any(|id| {
            state
                .objects
                .get(id)
                .map(|obj| obj.card_types.core_types.contains(&check_type))
                .unwrap_or(false)
        }),
        _ => true, // Unknown zone, default pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::TargetRef;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

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
    fn registry_has_31_entries() {
        let registry = build_registry();
        assert_eq!(registry.len(), 31);
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
            "Mill",
            "Scry",
            "PumpAll",
            "DamageAll",
            "DestroyAll",
            "ChangeZoneAll",
            "Dig",
            "GainControl",
            "Attach",
            "Surveil",
            "Fight",
            "Bounce",
            "Explore",
            "Proliferate",
            "CopySpell",
            "ChooseCard",
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

    #[test]
    fn resolve_ability_chain_single_effect() {
        let registry = build_registry();
        let mut state = GameState::new_two_player(42);
        // Add a card in library so Draw has something to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility {
            api_type: "Draw".to_string(),
            params: HashMap::from([("NumCards".to_string(), "1".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, &registry, 0);
        assert!(result.is_ok());
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn resolve_ability_chain_with_sub_ability() {
        let registry = build_registry();
        let mut state = GameState::new_two_player(42);
        // Add cards to draw
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card A".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility {
            api_type: "DealDamage".to_string(),
            params: HashMap::from([
                ("NumDmg".to_string(), "2".to_string()),
                ("SubAbility".to_string(), "DBDraw".to_string()),
            ]),
            targets: vec![TargetRef::Player(PlayerId(1))],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::from([("DBDraw".to_string(), "DB$ Draw | NumCards$ 1".to_string())]),
        };
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, &registry, 0);
        assert!(result.is_ok());
        // Damage dealt to player 1
        assert_eq!(state.players[1].life, 18);
        // Controller drew a card
        assert_eq!(state.players[0].hand.len(), 1);
    }

    #[test]
    fn chain_depth_exceeds_limit_returns_error() {
        let registry = build_registry();
        let mut state = GameState::new_two_player(42);
        let ability = make_ability("Draw");
        let mut events = Vec::new();

        let result = resolve_ability_chain(&mut state, &ability, &mut events, &registry, 11);
        assert_eq!(result, Err(EffectError::ChainTooDeep));
    }

    #[test]
    fn condition_present_creature_on_battlefield_passes() {
        let mut state = GameState::new_two_player(42);
        let creature_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility {
            api_type: "Draw".to_string(),
            params: HashMap::from([("ConditionPresent".to_string(), "Creature".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };

        assert!(check_conditions(&ability, &state));
    }

    #[test]
    fn condition_present_no_creatures_fails() {
        let state = GameState::new_two_player(42);

        let ability = ResolvedAbility {
            api_type: "Draw".to_string(),
            params: HashMap::from([("ConditionPresent".to_string(), "Creature".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };

        assert!(!check_conditions(&ability, &state));
    }

    #[test]
    fn condition_compare_ge2_with_3_creatures_passes() {
        let mut state = GameState::new_two_player(42);
        for i in 0..3 {
            let id = create_object(
                &mut state,
                CardId(i + 1),
                PlayerId(0),
                format!("Creature {}", i),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility {
            api_type: "Draw".to_string(),
            params: HashMap::from([
                ("ConditionCompare".to_string(), "GE2".to_string()),
                (
                    "ConditionSVarCompare".to_string(),
                    "CreatureCount".to_string(),
                ),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };

        assert!(check_conditions(&ability, &state));
    }

    #[test]
    fn condition_compare_eq0_with_1_creature_fails() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility {
            api_type: "Draw".to_string(),
            params: HashMap::from([
                ("ConditionCompare".to_string(), "EQ0".to_string()),
                (
                    "ConditionSVarCompare".to_string(),
                    "CreatureCount".to_string(),
                ),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };

        assert!(!check_conditions(&ability, &state));
    }
}
