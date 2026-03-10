use std::collections::HashMap;

use crate::game::filter::{object_matches_filter, player_matches_filter};
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
    RuleModification { mode: String },
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

    // Promoted static ability handlers
    registry.insert("CantBeBlocked".to_string(), handle_cant_be_blocked);
    registry.insert("Ward".to_string(), handle_ward);
    registry.insert("Protection".to_string(), handle_protection);

    // Promoted static ability handlers -- Standard-relevant mechanics
    registry.insert("Indestructible".to_string(), handle_indestructible);
    registry.insert("CantBeCountered".to_string(), handle_cant_be_countered);
    registry.insert("CantBeDestroyed".to_string(), handle_cant_be_destroyed);
    registry.insert("FlashBack".to_string(), handle_flashback);
    registry.insert("Shroud".to_string(), handle_shroud);
    registry.insert("Vigilance".to_string(), handle_static_vigilance);
    registry.insert("Menace".to_string(), handle_static_menace);
    registry.insert("Reach".to_string(), handle_static_reach);
    registry.insert("Flying".to_string(), handle_static_flying);
    registry.insert("Trample".to_string(), handle_static_trample);
    registry.insert("Deathtouch".to_string(), handle_static_deathtouch);
    registry.insert("Lifelink".to_string(), handle_static_lifelink);
    registry.insert("CantTap".to_string(), handle_rule_mod);
    registry.insert("CantUntap".to_string(), handle_rule_mod);
    registry.insert("MustBeBlocked".to_string(), handle_rule_mod);
    registry.insert("CantAttackAlone".to_string(), handle_rule_mod);
    registry.insert("CantBlockAlone".to_string(), handle_rule_mod);
    registry.insert("MayLookAtTopOfLibrary".to_string(), handle_rule_mod);

    // Stub modes -- recognized but no-op until needed
    let stubs = [
        "CantBeSacrificed",
        "CantBeEnchanted",
        "CantTransform",
        "CantBeEquipped",
        "CantRegenerate",
        "CantPlaneswalkerRedirect",
        "Devoid",
        "Forecast",
        "ReduceCostEach",
        "SetCost",
        "AlternateCost",
        "CantPlayLand",
        "CantShuffle",
        "ETBReplacement",
        "CantDealDamage",
        "CantBeDealtDamage",
        "DamageReduction",
        "PreventDamage",
        "DealtDamageInsteadExile",
        "AssignNoCombatDamage",
        "AttackRestriction",
        "BlockRestriction",
        "MinBlockers",
        "MaxBlockers",
        "CantBeAttached",
        "CantExistWithout",
        "LeavesPlay",
        "ChangesZoneAll",
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

/// Handler for CantBeBlocked -- creature cannot be blocked.
pub fn handle_cant_be_blocked(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "CantBeBlocked".to_string(),
    }]
}

/// Handler for Ward -- opponent must pay additional cost or spell/ability is countered.
/// Cost enforcement is deferred to mana payment UI; this marks the static as active.
pub fn handle_ward(
    _state: &GameState,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    let cost = params.get("Cost").cloned().unwrap_or_default();
    vec![StaticEffect::RuleModification {
        mode: format!("Ward:{}", cost),
    }]
}

/// Handler for Protection -- prevents damage, blocking, targeting, and enchanting
/// by sources with the specified quality.
pub fn handle_protection(
    _state: &GameState,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    let target = params.get("Target").cloned().unwrap_or_default();
    vec![StaticEffect::RuleModification {
        mode: format!("Protection:{}", target),
    }]
}

/// Handler for Indestructible -- prevents destruction by lethal damage and destroy effects.
/// SBA integration: sba.rs already checks has_keyword(Indestructible). This static handler
/// enables static-granted Indestructible (e.g., "Creatures you control are indestructible").
fn handle_indestructible(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Indestructible".to_string(),
    }]
}

/// Handler for CantBeCountered -- spell cannot be countered.
fn handle_cant_be_countered(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "CantBeCountered".to_string(),
    }]
}

/// Handler for CantBeDestroyed -- permanent cannot be destroyed.
fn handle_cant_be_destroyed(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "CantBeDestroyed".to_string(),
    }]
}

/// Handler for FlashBack -- allows casting from graveyard, exiled after resolution.
fn handle_flashback(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "FlashBack".to_string(),
    }]
}

/// Handler for Shroud -- permanent cannot be the target of spells or abilities.
fn handle_shroud(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Shroud".to_string(),
    }]
}

/// Handler for static-granted Vigilance (e.g., "All creatures you control have vigilance").
fn handle_static_vigilance(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Vigilance".to_string(),
    }]
}

/// Handler for static-granted Menace (requires 2+ blockers).
fn handle_static_menace(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Menace".to_string(),
    }]
}

/// Handler for static-granted Reach (can block flying).
fn handle_static_reach(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Reach".to_string(),
    }]
}

/// Handler for static-granted Flying.
fn handle_static_flying(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Flying".to_string(),
    }]
}

/// Handler for static-granted Trample.
fn handle_static_trample(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Trample".to_string(),
    }]
}

/// Handler for static-granted Deathtouch.
fn handle_static_deathtouch(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Deathtouch".to_string(),
    }]
}

/// Handler for static-granted Lifelink.
fn handle_static_lifelink(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::RuleModification {
        mode: "Lifelink".to_string(),
    }]
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
pub fn check_static_ability(state: &GameState, mode: &str, context: &StaticCheckContext) -> bool {
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
    if let Some(target_id) = context.target_id {
        return object_matches_filter(state, target_id, filter, source_id);
    }

    if let Some(player_id) = context.player_id {
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        return player_matches_filter(player_id, filter, source_controller);
    }

    // No specific target -- matches by default
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
    fn test_cant_be_blocked_returns_rule_modification() {
        let state = setup();
        let params = HashMap::new();
        let effects = handle_cant_be_blocked(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            StaticEffect::RuleModification { mode } => {
                assert_eq!(mode, "CantBeBlocked");
            }
            _ => panic!("Expected RuleModification effect"),
        }
    }

    #[test]
    fn test_ward_returns_rule_modification_with_cost() {
        let state = setup();
        let mut params = HashMap::new();
        params.insert("Cost".to_string(), "2".to_string());
        let effects = handle_ward(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            StaticEffect::RuleModification { mode } => {
                assert!(mode.starts_with("Ward:"));
            }
            _ => panic!("Expected RuleModification effect"),
        }
    }

    #[test]
    fn test_protection_returns_rule_modification_with_target() {
        let state = setup();
        let mut params = HashMap::new();
        params.insert("Target".to_string(), "Red".to_string());
        let effects = handle_protection(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        match &effects[0] {
            StaticEffect::RuleModification { mode } => {
                assert!(mode.starts_with("Protection:"));
            }
            _ => panic!("Expected RuleModification effect"),
        }
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

    #[test]
    fn test_indestructible_returns_rule_modification() {
        let state = setup();
        let params = HashMap::new();
        let effects = handle_indestructible(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "Indestructible".to_string()
            }
        );
    }

    #[test]
    fn test_cant_be_countered_returns_rule_modification() {
        let state = setup();
        let params = HashMap::new();
        let effects = handle_cant_be_countered(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "CantBeCountered".to_string()
            }
        );
    }

    #[test]
    fn test_flashback_returns_rule_modification() {
        let state = setup();
        let params = HashMap::new();
        let effects = handle_flashback(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "FlashBack".to_string()
            }
        );
    }

    #[test]
    fn test_cant_be_destroyed_returns_rule_modification() {
        let state = setup();
        let params = HashMap::new();
        let effects = handle_cant_be_destroyed(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(
            effects[0],
            StaticEffect::RuleModification {
                mode: "CantBeDestroyed".to_string()
            }
        );
    }

    #[test]
    fn test_static_keyword_handlers_return_correct_modes() {
        let state = setup();
        let params = HashMap::new();

        let vigilance = handle_static_vigilance(&state, &params, ObjectId(1));
        assert_eq!(
            vigilance[0],
            StaticEffect::RuleModification {
                mode: "Vigilance".to_string()
            }
        );

        let menace = handle_static_menace(&state, &params, ObjectId(1));
        assert_eq!(
            menace[0],
            StaticEffect::RuleModification {
                mode: "Menace".to_string()
            }
        );

        let reach = handle_static_reach(&state, &params, ObjectId(1));
        assert_eq!(
            reach[0],
            StaticEffect::RuleModification {
                mode: "Reach".to_string()
            }
        );

        let flying = handle_static_flying(&state, &params, ObjectId(1));
        assert_eq!(
            flying[0],
            StaticEffect::RuleModification {
                mode: "Flying".to_string()
            }
        );

        let trample = handle_static_trample(&state, &params, ObjectId(1));
        assert_eq!(
            trample[0],
            StaticEffect::RuleModification {
                mode: "Trample".to_string()
            }
        );

        let deathtouch = handle_static_deathtouch(&state, &params, ObjectId(1));
        assert_eq!(
            deathtouch[0],
            StaticEffect::RuleModification {
                mode: "Deathtouch".to_string()
            }
        );

        let lifelink = handle_static_lifelink(&state, &params, ObjectId(1));
        assert_eq!(
            lifelink[0],
            StaticEffect::RuleModification {
                mode: "Lifelink".to_string()
            }
        );

        let shroud = handle_shroud(&state, &params, ObjectId(1));
        assert_eq!(
            shroud[0],
            StaticEffect::RuleModification {
                mode: "Shroud".to_string()
            }
        );
    }

    #[test]
    fn test_promoted_statics_no_longer_stubs() {
        let registry = build_static_registry();
        // Promoted statics should NOT return empty Vec (which stub does)
        let state = setup();
        let params = HashMap::new();

        for mode in &[
            "Indestructible",
            "CantBeCountered",
            "CantBeDestroyed",
            "FlashBack",
            "Vigilance",
            "Menace",
            "Reach",
            "Flying",
            "Trample",
            "Deathtouch",
            "Lifelink",
            "Shroud",
        ] {
            let handler = registry
                .get(*mode)
                .unwrap_or_else(|| panic!("{} should be in registry", mode));
            let effects = handler(&state, &params, ObjectId(1));
            assert!(
                !effects.is_empty(),
                "{} should return non-empty effects (no longer a stub)",
                mode
            );
        }
    }
}
