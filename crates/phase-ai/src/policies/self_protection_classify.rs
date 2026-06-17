//! Shared building blocks for reactive self-protection policies.
//!
//! Classifies "save yourself / your permanents" effect signatures and assesses
//! whether an immediate threat justifies spending a cost now. Consumed by
//! `ReactiveSelfProtectionPolicy` (spells + activations) and
//! `SacrificeLandProtectionPolicy` (land-sacrifice defensive outlets such as
//! Sylvan Safekeeper — issue #771).

use engine::types::ability::{
    AbilityCost, AbilityDefinition, ContinuousModification, ControllerRef, Effect,
    StaticDefinition, TargetFilter,
};
use engine::types::game_state::GameState;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

use crate::ability_chain::collect_chain_effects;
use crate::eval::threat_level;
use crate::features::landfall::ability_searches_library_for_land;
use crate::features::mana_ramp::target_filter_references_land;

/// Threat-level threshold above which protection casts/activations are unblocked.
pub(crate) const THREAT_FLOOR: f64 = 0.45;

/// Returns true if any of four threat signals is present:
///   - Stack contains an opponent-controlled object whose targets include
///     the AI player or any AI-controlled permanent (CR 117.1a).
///   - Stack contains an opponent-controlled untargeted mass-removal effect.
///   - The AI's own life total is below 40% of starting life.
///   - On the opponent's turn, some opponent's `threat_level` is at or above
///     `THREAT_FLOOR` (board pressure that can attack this turn).
pub(crate) fn any_immediate_threat(state: &GameState, ai_player: PlayerId) -> bool {
    if any_stack_targets_ai_or_ai_permanent(state, ai_player) {
        return true;
    }
    if any_stack_has_untargeted_mass_threat(state, ai_player) {
        return true;
    }
    let starting_life = state.format_config.starting_life.max(1) as f64;
    let life_ratio = state.players[ai_player.0 as usize].life as f64 / starting_life;
    if life_ratio < 0.4 {
        return true;
    }
    if state.active_player == ai_player {
        return false;
    }
    state.players.iter().any(|p| {
        if p.id == ai_player || p.is_eliminated {
            return false;
        }
        threat_level(state, ai_player, p.id) >= THREAT_FLOOR
    })
}

/// CR 508/509/510: protective grants have a real payoff during combat steps
/// where creatures are attacking, blocking, or dealing damage.
pub(crate) fn combat_step_allows_protection(state: &GameState) -> bool {
    matches!(
        state.phase,
        Phase::DeclareAttackers | Phase::DeclareBlockers | Phase::CombatDamage
    )
}

/// Effect-signature classifier: returns true when an `Effect` represents
/// "save yourself / your permanents."
pub(crate) fn is_self_protection_effect(effect: &Effect) -> bool {
    match effect {
        Effect::PhaseOut { target } => target_filter_self_scoped(target),
        Effect::PreventDamage { .. } => true,
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => static_abilities
            .iter()
            .any(|sd| static_definition_is_self_protection(sd, target.as_ref())),
        _ => false,
    }
}

/// True when any effect in the ability chain is a self-protection grant.
pub(crate) fn ability_grants_self_protection(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(|effect| is_self_protection_effect(effect))
}

/// CR 701.21: activated ability sacrifices a land (not a fetchland) to grant
/// self-protection — Sylvan Safekeeper and the whole "sacrifice a land: target
/// creature you control gains shroud until end of turn" class (issue #771).
pub(crate) fn is_land_sacrifice_self_protection_activation(ability: &AbilityDefinition) -> bool {
    use engine::types::ability::CostCategory;

    if !ability
        .cost_categories()
        .contains(&CostCategory::SacrificesPermanent)
    {
        return false;
    }
    if !cost_sacrifices_land(ability.cost.as_ref()) {
        return false;
    }
    if ability_searches_library_for_land(ability) {
        return false;
    }
    ability_grants_self_protection(ability)
}

fn static_definition_is_self_protection(
    sd: &StaticDefinition,
    parent_target: Option<&TargetFilter>,
) -> bool {
    let affects_self = match sd.affected.as_ref() {
        Some(TargetFilter::ParentTarget) => parent_target.is_some_and(target_filter_self_scoped),
        Some(f) => target_filter_self_scoped(f),
        None => false,
    };
    if !affects_self {
        return false;
    }
    if static_mode_is_defensive(&sd.mode) {
        return true;
    }
    sd.modifications.iter().any(modification_is_defensive)
}

fn static_mode_is_defensive(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::CantBeTargeted
            | StaticMode::CantBeBlocked
            | StaticMode::CantLoseLife
            | StaticMode::Protection
            | StaticMode::Shroud
            | StaticMode::Hexproof
    )
}

fn modification_is_defensive(m: &ContinuousModification) -> bool {
    match m {
        ContinuousModification::AddKeyword { keyword } => keyword_is_defensive(keyword),
        ContinuousModification::AddStaticMode { mode } => static_mode_is_defensive(mode),
        ContinuousModification::GrantAbility { definition } => collect_chain_effects(definition)
            .iter()
            .any(|effect| is_self_protection_effect(effect)),
        _ => false,
    }
}

fn keyword_is_defensive(keyword: &Keyword) -> bool {
    matches!(
        keyword,
        Keyword::Indestructible
            | Keyword::Hexproof
            | Keyword::HexproofFrom(_)
            | Keyword::Shroud
            | Keyword::Protection(_)
    )
}

pub(crate) fn target_filter_self_scoped(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Controller | TargetFilter::SelfRef => true,
        TargetFilter::Typed(tf) => matches!(tf.controller, Some(ControllerRef::You)),
        _ => false,
    }
}

fn cost_sacrifices_land(cost: Option<&AbilityCost>) -> bool {
    match cost {
        None => false,
        Some(AbilityCost::Sacrifice(sacrifice)) => target_filter_references_land(&sacrifice.target),
        Some(AbilityCost::Composite { costs }) => {
            costs.iter().any(|c| cost_sacrifices_land(Some(c)))
        }
        _ => false,
    }
}

fn any_stack_has_untargeted_mass_threat(state: &GameState, ai_player: PlayerId) -> bool {
    use engine::types::zones::Zone;
    state.stack.iter().any(|entry| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        matches!(
            &ability.effect,
            Effect::DestroyAll { .. }
                | Effect::DamageAll { .. }
                | Effect::BounceAll { .. }
                | Effect::ChangeZoneAll {
                    destination: Zone::Exile | Zone::Graveyard | Zone::Hand,
                    ..
                }
        )
    })
}

fn any_stack_targets_ai_or_ai_permanent(state: &GameState, ai_player: PlayerId) -> bool {
    use engine::types::ability::TargetRef;
    state.stack.iter().any(|entry| {
        if entry.controller == ai_player {
            return false;
        }
        let Some(ability) = entry.ability() else {
            return false;
        };
        ability.targets.iter().any(|t| match t {
            TargetRef::Player(pid) => *pid == ai_player,
            TargetRef::Object(obj_id) => state
                .objects
                .get(obj_id)
                .is_some_and(|obj| obj.controller == ai_player),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{AbilityKind, ControllerRef, TypedFilter};
    use engine::types::keywords::ProtectionTarget;

    fn grant_effect(
        affected: Option<TargetFilter>,
        target: Option<TargetFilter>,
        keyword: Keyword,
    ) -> Effect {
        use engine::types::ability::StaticDefinition;
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(affected.unwrap_or(TargetFilter::ParentTarget))
                .modifications(vec![ContinuousModification::AddKeyword { keyword }])],
            target,
            duration: None,
        }
    }

    #[test]
    fn classifier_recognises_parent_target_shroud_grant() {
        assert!(is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            )),
            Keyword::Shroud,
        )));
    }

    #[test]
    fn classifier_recognises_static_mode_shroud() {
        use engine::types::ability::StaticDefinition;
        let effect = Effect::GenericEffect {
            static_abilities: vec![
                StaticDefinition::new(StaticMode::Shroud).affected(TargetFilter::ParentTarget)
            ],
            target: Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
            duration: None,
        };
        assert!(is_self_protection_effect(&effect));
    }

    #[test]
    fn classifier_recognises_grant_ability_wrapped_shroud() {
        use engine::types::ability::{AbilityDefinition, StaticDefinition};
        let inner = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::GenericEffect {
                static_abilities: vec![StaticDefinition::continuous().modifications(vec![
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Shroud,
                    },
                ])],
                target: None,
                duration: None,
            },
        );
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::continuous()
                .affected(TargetFilter::ParentTarget)
                .modifications(vec![ContinuousModification::GrantAbility {
                    definition: inner,
                }])],
            target: Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
            duration: None,
        };
        assert!(is_self_protection_effect(&effect));
    }

    #[test]
    fn land_sacrifice_classifier_matches_safekeeper_shape() {
        use engine::types::ability::{SacrificeCost, SacrificeRequirement};
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            grant_effect(
                Some(TargetFilter::ParentTarget),
                Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::You),
                )),
                Keyword::Shroud,
            ),
        );
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost {
            target: TargetFilter::Typed(TypedFilter::new(engine::types::ability::TypeFilter::Land)),
            requirement: SacrificeRequirement::count(1),
        }));
        assert!(is_land_sacrifice_self_protection_activation(&ability));
    }

    #[test]
    fn land_sacrifice_classifier_rejects_fetchland() {
        use engine::types::ability::{
            ControllerRef, QuantityExpr, SacrificeCost, SearchSelectionConstraint,
        };
        use engine::types::zones::Zone;
        let search = Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::land()),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::None,
            split: None,
            source_zones: vec![Zone::Library],
        };
        let put_in_play = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped: engine::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, search);
        ability.cost = Some(AbilityCost::Sacrifice(SacrificeCost::count(
            TargetFilter::SelfRef,
            1,
        )));
        ability.sub_ability = Some(Box::new(put_in_play));
        assert!(!is_land_sacrifice_self_protection_activation(&ability));
    }

    #[test]
    fn protection_keyword_grant_is_self_scoped() {
        assert!(is_self_protection_effect(&grant_effect(
            Some(TargetFilter::ParentTarget),
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You)
            )),
            Keyword::Protection(ProtectionTarget::ChosenColor),
        )));
    }
}
