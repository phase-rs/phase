use std::collections::HashMap;

use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Describes what a static ability does (returned by handlers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticEffect {
    /// Continuous effect -- evaluated through layers.rs, details in params.
    Continuous {
        layer_params: HashMap<String, String>,
    },
    /// Rule modification -- checked at specific game points.
    RuleModification {
        mode: String,
    },
}

/// Context for checking if a static ability applies to a given scenario.
#[derive(Debug, Clone, Default)]
pub struct StaticCheckContext {
    pub source_id: Option<ObjectId>,
    pub target_id: Option<ObjectId>,
    pub player_id: Option<PlayerId>,
    pub card_name: Option<String>,
}

/// Handler function type for static ability modes.
pub type StaticAbilityHandler = fn(
    state: &GameState,
    params: &HashMap<String, String>,
    source_id: ObjectId,
) -> Vec<StaticEffect>;

/// Build the static ability handler registry.
/// Maps mode strings to handler functions.
pub fn build_static_registry() -> HashMap<String, StaticAbilityHandler> {
    let mut registry: HashMap<String, StaticAbilityHandler> = HashMap::new();

    // Core continuous mode (evaluated through layers)
    registry.insert("Continuous".to_string(), handle_continuous);

    // Core rule-modification handlers with real logic
    registry.insert("CantAttack".to_string(), handle_rule_mod);
    registry.insert("CantBlock".to_string(), handle_rule_mod);
    registry.insert("CantBeTargeted".to_string(), handle_rule_mod);
    registry.insert("CantBeCast".to_string(), handle_rule_mod);
    registry.insert("CantBeActivated".to_string(), handle_rule_mod);
    registry.insert("CastWithFlash".to_string(), handle_rule_mod);
    registry.insert("ReduceCost".to_string(), handle_rule_mod);
    registry.insert("RaiseCost".to_string(), handle_rule_mod);
    registry.insert("CantGainLife".to_string(), handle_rule_mod);
    registry.insert("CantLoseLife".to_string(), handle_rule_mod);
    registry.insert("MustAttack".to_string(), handle_rule_mod);
    registry.insert("MustBlock".to_string(), handle_rule_mod);
    registry.insert("CantDraw".to_string(), handle_rule_mod);
    registry.insert("Panharmonicon".to_string(), handle_rule_mod);
    registry.insert("IgnoreHexproof".to_string(), handle_rule_mod);

    // Stub modes -- recognized but no-op until needed
    let stubs = [
        "CantBeCountered", "CantBeDestroyed", "CantBeSacrificed", "CantBeEnchanted",
        "CantTransform", "CantBeEquipped", "CantBeBlocked", "CantRegenerate",
        "CantPlaneswalkerRedirect", "Devoid", "FlashBack", "Forecast",
        "ReduceCostEach", "SetCost", "AlternateCost", "CantPlayLand",
        "CantShuffle", "CantTap", "CantUntap", "ETBReplacement",
        "Indestructible", "Shroud", "Ward", "Protection",
        "CantDealDamage", "CantBeDealtDamage", "DamageReduction", "PreventDamage",
        "DealtDamageInsteadExile", "AssignNoCombatDamage", "CantAttackAlone",
        "CantBlockAlone", "MustBeBlocked", "AttackRestriction", "BlockRestriction",
        "MinBlockers", "MaxBlockers", "CantBeAttached", "CantExistWithout",
        "LeavesPlay", "ChangesZoneAll", "Vigilance", "Menace", "Reach",
        "Flying", "Trample", "Deathtouch", "Lifelink",
    ];
    for mode in &stubs {
        registry.insert(mode.to_string(), handle_stub);
    }

    registry
}

/// Handler for the Continuous mode -- returns parsed layer effects from params.
fn handle_continuous(
    _state: &GameState,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::Continuous {
        layer_params: params.clone(),
    }]
}

/// Handler for rule-modification modes -- returns the mode as a RuleModification effect.
fn handle_rule_mod(
    _state: &GameState,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    // The mode is embedded in params or inferred from the registry key.
    // For rule mods, we return the effect so callers can check applicability.
    let mode = params.get("Mode").cloned().unwrap_or_default();
    vec![StaticEffect::RuleModification { mode }]
}

/// Stub handler for recognized but unimplemented modes.
fn handle_stub(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    Vec::new()
}

/// Check if any active static ability of the given mode applies to the context.
///
/// Scans battlefield objects for static_definitions matching the mode,
/// then checks if the static's condition applies.
pub fn check_static_ability(
    state: &GameState,
    mode: &str,
    context: &StaticCheckContext,
) -> bool {
    for &id in &state.battlefield {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };

        for def in &obj.static_definitions {
            if def.mode != mode {
                continue;
            }

            // Check affected filter if present
            if let Some(affected) = def.params.get("Affected") {
                if !static_filter_matches(state, context, affected, id) {
                    continue;
                }
            }

            return true;
        }
    }

    false
}

/// Check if a static ability's affected filter matches the check context.
fn static_filter_matches(
    state: &GameState,
    context: &StaticCheckContext,
    filter: &str,
    source_id: ObjectId,
) -> bool {
    // If we have a target_id, check if it matches the filter
    if let Some(target_id) = context.target_id {
        return object_matches_static_filter(state, target_id, filter, source_id);
    }

    // If we have a player_id, check player-based filters
    if let Some(player_id) = context.player_id {
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        for part in filter.split('.') {
            match part {
                "You" => {
                    if source_controller != Some(player_id) {
                        return false;
                    }
                }
                "Opp" => {
                    if source_controller == Some(player_id) {
                        return false;
                    }
                }
                _ => {}
            }
        }
        return true;
    }

    // No specific target -- matches by default
    true
}

/// Check if an object matches a static ability filter (similar to layer filter).
fn object_matches_static_filter(
    state: &GameState,
    object_id: ObjectId,
    filter: &str,
    source_id: ObjectId,
) -> bool {
    use crate::types::card_type::CoreType;

    let obj = match state.objects.get(&object_id) {
        Some(o) => o,
        None => return false,
    };

    for part in filter.split('.') {
        match part {
            "Creature" => {
                if !obj.card_types.core_types.contains(&CoreType::Creature) {
                    return false;
                }
            }
            "Land" => {
                if !obj.card_types.core_types.contains(&CoreType::Land) {
                    return false;
                }
            }
            "Artifact" => {
                if !obj.card_types.core_types.contains(&CoreType::Artifact) {
                    return false;
                }
            }
            "Enchantment" => {
                if !obj.card_types.core_types.contains(&CoreType::Enchantment) {
                    return false;
                }
            }
            "Card" | "Permanent" | "Any" => {}
            "YouCtrl" => {
                let source_controller = state.objects.get(&source_id).map(|o| o.controller);
                if source_controller != Some(obj.controller) {
                    return false;
                }
            }
            "OppCtrl" => {
                let source_controller = state.objects.get(&source_id).map(|o| o.controller);
                if source_controller == Some(obj.controller) {
                    return false;
                }
            }
            "Self" => {
                if object_id != source_id {
                    return false;
                }
            }
            _ => {}
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::StaticDefinition;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    #[test]
    fn test_registry_has_all_modes() {
        let registry = build_static_registry();
        // 1 Continuous + 15 core rule-mod + 47 stubs = 63
        assert!(
            registry.len() >= 61,
            "Expected 61+ modes, got {}",
            registry.len()
        );
    }

    #[test]
    fn test_check_cant_attack() {
        let mut state = setup();
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pacifism Source".to_string(),
            Zone::Battlefield,
        );
        let target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Add CantAttack static targeting the creature
        let mut params = HashMap::new();
        params.insert("Affected".to_string(), "Creature.OppCtrl".to_string());
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(StaticDefinition {
                mode: "CantAttack".to_string(),
                params,
            });

        let ctx = StaticCheckContext {
            target_id: Some(target),
            ..Default::default()
        };
        assert!(check_static_ability(&state, "CantAttack", &ctx));
    }

    #[test]
    fn test_check_no_matching_static() {
        let state = setup();
        let ctx = StaticCheckContext {
            target_id: Some(ObjectId(99)),
            ..Default::default()
        };
        assert!(!check_static_ability(&state, "CantAttack", &ctx));
    }

    #[test]
    fn test_continuous_mode_returns_effects() {
        let state = setup();
        let mut params = HashMap::new();
        params.insert("Affected".to_string(), "Creature.YouCtrl".to_string());
        params.insert("AddPower".to_string(), "1".to_string());
        params.insert("AddToughness".to_string(), "1".to_string());

        let effects = handle_continuous(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            StaticEffect::Continuous { layer_params } => {
                assert_eq!(layer_params.get("AddPower").unwrap(), "1");
                assert_eq!(layer_params.get("AddToughness").unwrap(), "1");
            }
            _ => panic!("Expected Continuous effect"),
        }
    }
}
