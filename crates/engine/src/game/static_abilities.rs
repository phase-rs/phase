use std::collections::HashMap;

use crate::game::filter::matches_target_filter;
use crate::types::ability::{TargetFilter, TypedFilter};
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::statics::StaticMode;

/// Describes what a static ability does (returned by handlers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaticEffect {
    /// Continuous effect -- evaluated through layers.rs, details in typed modifications.
    Continuous,
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
/// Maps StaticMode keys to handler functions.
pub fn build_static_registry() -> HashMap<StaticMode, StaticAbilityHandler> {
    let mut registry: HashMap<StaticMode, StaticAbilityHandler> = HashMap::new();

    // Core continuous mode (evaluated through layers)
    registry.insert(StaticMode::Continuous, handle_continuous);

    // Core rule-modification handlers with real logic
    registry.insert(StaticMode::CantAttack, handle_rule_mod);
    registry.insert(StaticMode::CantBlock, handle_rule_mod);
    registry.insert(StaticMode::CantBeTargeted, handle_rule_mod);
    registry.insert(StaticMode::CantBeCast, handle_rule_mod);
    registry.insert(StaticMode::CantBeActivated, handle_rule_mod);
    registry.insert(StaticMode::CastWithFlash, handle_rule_mod);
    registry.insert(StaticMode::ReduceCost, handle_rule_mod);
    registry.insert(StaticMode::RaiseCost, handle_rule_mod);
    registry.insert(StaticMode::CantGainLife, handle_rule_mod);
    registry.insert(StaticMode::CantLoseLife, handle_rule_mod);
    registry.insert(StaticMode::MustAttack, handle_rule_mod);
    registry.insert(StaticMode::MustBlock, handle_rule_mod);
    registry.insert(StaticMode::CantDraw, handle_rule_mod);
    registry.insert(StaticMode::Panharmonicon, handle_rule_mod);
    registry.insert(StaticMode::IgnoreHexproof, handle_rule_mod);

    // Promoted static ability handlers
    registry.insert(
        StaticMode::Other("CantBeBlocked".into()),
        handle_cant_be_blocked,
    );
    registry.insert(StaticMode::Other("Ward".into()), handle_ward);
    registry.insert(StaticMode::Other("Protection".into()), handle_protection);

    // Promoted static ability handlers -- Standard-relevant mechanics
    registry.insert(
        StaticMode::Other("Indestructible".into()),
        handle_indestructible,
    );
    registry.insert(
        StaticMode::Other("CantBeCountered".into()),
        handle_cant_be_countered,
    );
    registry.insert(
        StaticMode::Other("CantBeDestroyed".into()),
        handle_cant_be_destroyed,
    );
    registry.insert(StaticMode::Other("FlashBack".into()), handle_flashback);
    registry.insert(StaticMode::Other("Shroud".into()), handle_shroud);
    registry.insert(
        StaticMode::Other("Vigilance".into()),
        handle_static_vigilance,
    );
    registry.insert(StaticMode::Other("Menace".into()), handle_static_menace);
    registry.insert(StaticMode::Other("Reach".into()), handle_static_reach);
    registry.insert(StaticMode::Other("Flying".into()), handle_static_flying);
    registry.insert(StaticMode::Other("Trample".into()), handle_static_trample);
    registry.insert(
        StaticMode::Other("Deathtouch".into()),
        handle_static_deathtouch,
    );
    registry.insert(StaticMode::Other("Lifelink".into()), handle_static_lifelink);
    registry.insert(StaticMode::Other("CantTap".into()), handle_rule_mod);
    registry.insert(StaticMode::Other("CantUntap".into()), handle_rule_mod);
    registry.insert(StaticMode::Other("MustBeBlocked".into()), handle_rule_mod);
    registry.insert(StaticMode::Other("CantAttackAlone".into()), handle_rule_mod);
    registry.insert(StaticMode::Other("CantBlockAlone".into()), handle_rule_mod);
    registry.insert(
        StaticMode::Other("MayLookAtTopOfLibrary".into()),
        handle_rule_mod,
    );

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
        registry.insert(StaticMode::Other((*mode).into()), handle_stub);
    }

    registry
}

/// Handler for the Continuous mode -- layers.rs handles the actual evaluation.
fn handle_continuous(
    _state: &GameState,
    _params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
    vec![StaticEffect::Continuous]
}

/// Handler for rule-modification modes -- returns the mode as a RuleModification effect.
fn handle_rule_mod(
    _state: &GameState,
    params: &HashMap<String, String>,
    _source_id: ObjectId,
) -> Vec<StaticEffect> {
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
    let target_mode: StaticMode = mode.parse().unwrap();
    for &id in &state.battlefield {
        let obj = match state.objects.get(&id) {
            Some(o) => o,
            None => continue,
        };

        for def in &obj.static_definitions {
            if def.mode != target_mode {
                continue;
            }

            // Check affected filter if present (typed TargetFilter)
            if let Some(ref affected) = def.affected {
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
    filter: &TargetFilter,
    source_id: ObjectId,
) -> bool {
    if let Some(target_id) = context.target_id {
        return matches_target_filter(state, target_id, filter, source_id);
    }

    if let Some(player_id) = context.player_id {
        // For player-targeted checks, we still use the string-based player filter.
        // TargetFilter::Player variant just returns false for object matching,
        // so we need to check if this is a player-affecting filter.
        let source_controller = state.objects.get(&source_id).map(|o| o.controller);
        match filter {
            TargetFilter::Any => return true,
            TargetFilter::Player => {
                // All players match
                return true;
            }
            TargetFilter::Typed(TypedFilter { controller, .. }) => {
                if let Some(ctrl) = controller {
                    return match ctrl {
                        crate::types::ability::ControllerRef::You => {
                            source_controller == Some(player_id)
                        }
                        crate::types::ability::ControllerRef::Opponent => {
                            source_controller.is_some() && source_controller != Some(player_id)
                        }
                    };
                }
                return true;
            }
            _ => return true,
        }
    }

    // No specific target -- matches by default
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;
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

        // Add CantAttack static targeting opponent's creatures
        let affected =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::Opponent));
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantAttack).affected(affected));

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
        let params = HashMap::new();
        let effects = handle_continuous(&state, &params, ObjectId(1));
        assert_eq!(effects.len(), 1);
        assert_eq!(effects[0], StaticEffect::Continuous);
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

        for mode_str in &[
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
            let mode_key = StaticMode::Other((*mode_str).into());
            let handler = registry
                .get(&mode_key)
                .unwrap_or_else(|| panic!("{} should be in registry", mode_str));
            let effects = handler(&state, &params, ObjectId(1));
            assert!(
                !effects.is_empty(),
                "{} should return non-empty effects (no longer a stub)",
                mode_str
            );
        }
    }
}
