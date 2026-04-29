use crate::types::ability::{
    CombatDamageScope, DamageTargetFilter, DamageTargetPlayerScope, Effect, EffectError,
    EffectKind, FilterProp, PreventionScope, ReplacementDefinition, ResolvedAbility, TargetFilter,
    TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::replacements::ReplacementEvent;
use crate::types::zones::Zone;

/// CR 614.1a: Resolve a damage source filter, replacing dynamic references
/// (e.g., `IsChosenColor`) with concrete values from the source object's state.
fn resolve_source_filter(
    filter: &TargetFilter,
    state: &GameState,
    source_id: ObjectId,
) -> TargetFilter {
    match filter {
        TargetFilter::ChosenDamageSource => state
            .last_chosen_damage_source
            .as_ref()
            .map(|choice| TargetFilter::And {
                filters: vec![
                    TargetFilter::SpecificObject {
                        id: choice.source_id,
                    },
                    resolve_source_filter(&choice.source_filter, state, source_id),
                ],
            })
            .unwrap_or(TargetFilter::None),
        TargetFilter::Not { filter: inner } => TargetFilter::Not {
            filter: Box::new(resolve_source_filter(inner, state, source_id)),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .iter()
                .map(|inner| resolve_source_filter(inner, state, source_id))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .iter()
                .map(|inner| resolve_source_filter(inner, state, source_id))
                .collect(),
        },
        TargetFilter::Typed(tf) => {
            let has_chosen_ref = tf
                .properties
                .iter()
                .any(|p| matches!(p, FilterProp::IsChosenColor));
            if !has_chosen_ref {
                return filter.clone();
            }
            // Resolve IsChosenColor → concrete HasColor using source's chosen attributes.
            let chosen_color = state
                .objects
                .get(&source_id)
                .and_then(|obj| obj.chosen_color());
            let mut resolved = tf.clone();
            resolved
                .properties
                .retain(|p| !matches!(p, FilterProp::IsChosenColor));
            if let Some(color) = chosen_color {
                resolved.properties.push(FilterProp::HasColor { color });
            }
            TargetFilter::Typed(resolved)
        }
        _ => filter.clone(),
    }
}

/// CR 615: Prevent damage — creates a prevention shield on the source object.
///
/// The shield is stored as a `ReplacementDefinition` with `ShieldKind::Prevention`
/// on the source object's `replacement_definitions`. The `damage_done_applier`
/// in `replacement.rs` consumes these shields when matching `ProposedEvent::Damage`.
///
/// Follows the same lifecycle as regeneration shields:
/// 1. Created here → 2. Matched/applied in replacement pipeline → 3. Cleaned up at end of turn
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, scope, effect_source_filter) = match &ability.effect {
        Effect::PreventDamage {
            amount,
            scope,
            damage_source_filter,
            ..
        } => (*amount, *scope, damage_source_filter.clone()),
        _ => {
            return Err(EffectError::InvalidParam(
                "expected PreventDamage effect".to_string(),
            ))
        }
    };

    // Build the prevention shield replacement definition.
    // Note: valid_card is NOT set here — targeted shields scope via placement on the target
    // object, and global shields (pending_damage_replacements) must match any damage event.
    let mut shield = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .prevention_shield(amount)
        .description("Prevent damage".to_string());

    // CR 615 + CR 614.1a: Resolve damage source filter from effect definition.
    // Filters using IsChosenColor need the chosen color resolved from the source object
    // and converted to a concrete HasColor filter for the shield.
    if let Some(src_filter) = effect_source_filter {
        let resolved_filter = resolve_source_filter(&src_filter, state, ability.source_id);
        shield = shield.damage_source_filter(resolved_filter);
    }

    // CR 615: Scope restriction — combat damage only vs all damage
    if scope == PreventionScope::CombatDamage {
        shield = shield.combat_scope(CombatDamageScope::CombatOnly);
    }

    // CR 615: For targeted prevention ("prevent the next N damage to target creature"),
    // the shield lives on the TARGET object — same pattern as regeneration shields.
    // This ensures the shield is found by find_applicable_replacements() which only
    // scans Battlefield/Command zones (instants move to graveyard after resolving).
    //
    // For untargeted effects (Fog: "prevent all combat damage"), the shield lives on
    // the source permanent. If the source is an instant/sorcery, the shield won't persist
    // after resolution — untargeted instant prevention requires a global mechanism (future work).
    if !ability.targets.is_empty() {
        for target in &ability.targets {
            match target {
                TargetRef::Object(obj_id) => {
                    if let Some(obj) = state.objects.get_mut(obj_id) {
                        obj.replacement_definitions.push(shield.clone());
                    }
                }
                TargetRef::Player(_) => {
                    // Player-targeted prevention: attach to source (permanent abilities)
                    // and scope with damage_target_filter.
                    let player_shield =
                        shield
                            .clone()
                            .damage_target_filter(DamageTargetFilter::Player {
                                player: DamageTargetPlayerScope::Any,
                            });
                    if let Some(obj) = state.objects.get_mut(&ability.source_id) {
                        obj.replacement_definitions.push(player_shield);
                    }
                }
            }
        }
    } else {
        // CR 615.3: Untargeted prevention — attach to source if it's a permanent on the
        // battlefield. Instants/sorceries on the Stack will be moved to graveyard/exile
        // after resolution, so their shields must go to the global registry instead.
        // find_applicable_replacements only scans Battlefield/Command zones for
        // object-attached shields.
        let is_permanent_on_battlefield = state
            .objects
            .get(&ability.source_id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield);
        if is_permanent_on_battlefield {
            if let Some(obj) = state.objects.get_mut(&ability.source_id) {
                obj.replacement_definitions.push(shield);
            }
        } else {
            // Source is on the Stack (instant/sorcery mid-resolution) or already left —
            // store in game-state-level registry so it persists until end of turn.
            state.pending_damage_replacements.push(shield);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PreventDamage,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{PreventionAmount, ShieldKind, TypedFilter};
    use crate::types::game_state::ChosenDamageSource;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaColor;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_prevent_ability(
        source: ObjectId,
        amount: PreventionAmount,
        scope: PreventionScope,
        targets: Vec<TargetRef>,
    ) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::PreventDamage {
                amount,
                target: TargetFilter::Any,
                scope,
                damage_source_filter: None,
            },
            targets,
            source,
            PlayerId(0),
        )
    }

    #[test]
    fn prevent_all_creates_shield_on_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fog".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::All,
            PreventionScope::AllDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert_eq!(obj.replacement_definitions.len(), 1);
        assert!(matches!(
            obj.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::All
            }
        ));
        assert_eq!(
            obj.replacement_definitions[0].event,
            ReplacementEvent::DamageDone
        );
        assert!(!obj.replacement_definitions[0].is_consumed);
    }

    #[test]
    fn chosen_damage_source_resolves_to_specific_source_and_rechecked_filter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Prevention Spell".to_string(),
            Zone::Stack,
        );
        let chosen = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Red Source".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&chosen).unwrap().color = vec![ManaColor::Red];
        let source_filter =
            TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::HasColor {
                    color: ManaColor::Red,
                }]),
            );
        state.last_chosen_damage_source = Some(ChosenDamageSource {
            source_id: chosen,
            source_filter: source_filter.clone(),
        });

        let ability = ResolvedAbility::new(
            Effect::PreventDamage {
                amount: PreventionAmount::All,
                target: TargetFilter::Any,
                scope: PreventionScope::AllDamage,
                damage_source_filter: Some(TargetFilter::ChosenDamageSource),
            },
            vec![],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.pending_damage_replacements.len(), 1);
        assert_eq!(
            state.pending_damage_replacements[0].damage_source_filter,
            Some(TargetFilter::And {
                filters: vec![TargetFilter::SpecificObject { id: chosen }, source_filter],
            })
        );
    }

    #[test]
    fn prevent_next_n_creates_shield_with_amount() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shield".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::Next(3),
            PreventionScope::AllDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert!(matches!(
            obj.replacement_definitions[0].shield_kind,
            ShieldKind::Prevention {
                amount: PreventionAmount::Next(3)
            }
        ));
    }

    #[test]
    fn combat_damage_scope_sets_combat_only() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fog".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::All,
            PreventionScope::CombatDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&source).unwrap();
        assert_eq!(
            obj.replacement_definitions[0].combat_scope,
            Some(CombatDamageScope::CombatOnly)
        );
    }

    #[test]
    fn emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Fog".to_string(),
            Zone::Battlefield,
        );

        let ability = make_prevent_ability(
            source,
            PreventionAmount::All,
            PreventionScope::AllDamage,
            vec![],
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::PreventDamage,
                ..
            }
        )));
    }
}
