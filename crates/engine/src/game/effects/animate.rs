use std::str::FromStr;

use crate::types::ability::{
    ContinuousModification, Duration, Effect, EffectError, EffectKind, PtValue, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 613.1: Animation — apply type/subtype and P/T changes via the layer system.
/// Uses `TransientContinuousEffect` so the layer system handles ordering (CR 613.1d,
/// CR 613.1f, CR 613.4) and automatic cleanup at end-of-turn or when source leaves play.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (power, toughness, types_list, remove_types_list, kw_list) = match &ability.effect {
        Effect::Animate {
            power,
            toughness,
            types,
            remove_types,
            keywords,
            ..
        } => (
            power.clone(),
            toughness.clone(),
            types.as_slice(),
            remove_types.as_slice(),
            keywords.as_slice(),
        ),
        _ => (None, None, [].as_slice(), [].as_slice(), [].as_slice()),
    };

    let targets = resolve_animate_targets(ability);

    let duration = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);

    // CR 613.1: Build layer-appropriate modifications instead of direct mutation.
    let mut modifications = Vec::new();

    // CR 613.4 / Layer 7b: Set base P/T (overrides printed values).
    // PtValue::Fixed(n) emits a static SetPower; PtValue::Quantity(q) emits
    // SetPowerDynamic so the layer system re-evaluates q each tick (CR 613.1).
    //
    // CR 608.2h exception: a quantity that reads a resolution-only object scope
    // (the triggering/cost object — "that creature's power" → `Power {
    // CostPaidObject }`) is snapshotted to a fixed `SetPower`/`SetToughness` at
    // resolution. Such referents are gone by the time the layer system
    // re-evaluates, so a dynamic modification would read 0; CR 608.2h also
    // requires the value be determined once when the effect applies. Source- and
    // recipient-scoped quantities (CDA "becomes X/X where X is its power") stay
    // dynamic — they remain layer-resolvable.
    match power {
        Some(PtValue::Fixed(n)) => {
            modifications.push(ContinuousModification::SetPower { value: n })
        }
        Some(PtValue::Quantity(q)) => {
            modifications.push(set_pt_modification(state, ability, q, false))
        }
        Some(PtValue::Variable(_)) | None => {}
    }
    match toughness {
        Some(PtValue::Fixed(n)) => {
            modifications.push(ContinuousModification::SetToughness { value: n })
        }
        Some(PtValue::Quantity(q)) => {
            modifications.push(set_pt_modification(state, ability, q, true))
        }
        Some(PtValue::Variable(_)) | None => {}
    }

    // CR 613.1d / Layer 4: Add types and subtypes.
    for t in types_list {
        let t = t.trim();
        if let Ok(core) = CoreType::from_str(t) {
            modifications.push(ContinuousModification::AddType { core_type: core });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: t.to_string(),
            });
        }
    }

    // CR 205.1a / Layer 4: Remove core types (e.g., Glimmer cycle "it's not a creature").
    for t in remove_types_list {
        if let Ok(core) = CoreType::from_str(t.trim()) {
            modifications.push(ContinuousModification::RemoveType { core_type: core });
        }
    }

    // CR 613.1f / Layer 6: Add keywords.
    for kw in kw_list {
        modifications.push(ContinuousModification::AddKeyword {
            keyword: kw.clone(),
        });
    }

    // Register a TransientContinuousEffect per target so the layer system handles
    // ordering and cleanup automatically.
    for obj_id in targets {
        if !state.objects.contains_key(&obj_id) {
            return Err(EffectError::ObjectNotFound(obj_id));
        }
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            duration.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            modifications.clone(),
            None,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 613.4b + CR 608.2h: Build the layer-7b base-P/T set modification for a
/// dynamic `PtValue::Quantity`. Resolution-only-scoped quantities (the
/// triggering/cost object referent) are snapshotted to a fixed value here;
/// everything else stays a `SetPowerDynamic`/`SetToughnessDynamic` the layer
/// system re-evaluates. `is_toughness` selects power vs. toughness.
fn set_pt_modification(
    state: &GameState,
    ability: &ResolvedAbility,
    value: crate::types::ability::QuantityExpr,
    is_toughness: bool,
) -> ContinuousModification {
    if crate::game::quantity::quantity_expr_uses_resolution_only_object_scope(&value) {
        let resolved = crate::game::quantity::resolve_quantity_with_targets(state, &value, ability);
        if is_toughness {
            ContinuousModification::SetToughness { value: resolved }
        } else {
            ContinuousModification::SetPower { value: resolved }
        }
    } else if is_toughness {
        ContinuousModification::SetToughnessDynamic { value }
    } else {
        ContinuousModification::SetPowerDynamic { value }
    }
}

fn resolve_animate_targets(ability: &ResolvedAbility) -> Vec<crate::types::identifiers::ObjectId> {
    if let Effect::Animate { target, .. } = &ability.effect {
        if matches!(target, TargetFilter::None) {
            return vec![ability.source_id];
        }
    }
    ability
        .targets
        .iter()
        .filter_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{QuantityExpr, QuantityRef};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn animate_creates_transient_continuous_effect() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Fixed(7)),
                toughness: Some(PtValue::Fixed(7)),
                types: vec!["Creature".to_string(), "Beast".to_string()],
                remove_types: vec![],
                keywords: vec![],
                target: TargetFilter::None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should create a TransientContinuousEffect instead of mutating directly
        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: obj_id });
        assert!(tce
            .modifications
            .contains(&ContinuousModification::SetPower { value: 7 }));
        assert!(tce
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 7 }));
        assert!(tce
            .modifications
            .contains(&ContinuousModification::AddType {
                core_type: CoreType::Creature
            }));
        assert!(tce
            .modifications
            .contains(&ContinuousModification::AddSubtype {
                subtype: "Beast".to_string()
            }));
    }

    #[test]
    fn animate_uses_until_end_of_turn_by_default() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Fixed(3)),
                toughness: Some(PtValue::Fixed(3)),
                types: vec!["Creature".to_string()],
                remove_types: vec![],
                keywords: vec![],
                target: TargetFilter::None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.transient_continuous_effects[0].duration,
            Duration::UntilEndOfTurn
        );
    }

    #[test]
    fn animate_respects_explicit_duration() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );

        let mut ability = ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Fixed(5)),
                toughness: Some(PtValue::Fixed(5)),
                types: vec!["Creature".to_string()],
                remove_types: vec![],
                keywords: vec![],
                target: TargetFilter::None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        ability.duration = Some(Duration::UntilHostLeavesPlay);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.transient_continuous_effects[0].duration,
            Duration::UntilHostLeavesPlay
        );
    }

    /// CR 613.4 + CR 613.1: PtValue::Quantity routes to SetPowerDynamic /
    /// SetToughnessDynamic so the layer system re-evaluates the quantity each
    /// tick (e.g. "becomes an X/X creature" where X = CostXPaid).
    #[test]
    fn animate_dynamic_pt_emits_set_power_dynamic() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Land".to_string(),
            Zone::Battlefield,
        );

        let cost_x = QuantityExpr::Ref {
            qty: QuantityRef::CostXPaid,
        };
        let ability = ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Quantity(cost_x.clone())),
                toughness: Some(PtValue::Quantity(cost_x.clone())),
                types: vec!["Creature".to_string()],
                remove_types: vec![],
                keywords: vec![],
                target: TargetFilter::None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let tce = &state.transient_continuous_effects[0];
        assert!(
            tce.modifications
                .contains(&ContinuousModification::SetPowerDynamic {
                    value: cost_x.clone()
                }),
            "must emit SetPowerDynamic(CostXPaid)"
        );
        assert!(
            tce.modifications
                .contains(&ContinuousModification::SetToughnessDynamic { value: cost_x }),
            "must emit SetToughnessDynamic(CostXPaid)"
        );
        assert!(
            !tce.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::SetPower { .. })),
            "must not emit static SetPower when PtValue::Quantity"
        );
    }

    /// CR 608.2h + CR 613.4b: a base-power set whose value reads a
    /// resolution-only object scope ("that creature's power" →
    /// `Power { CostPaidObject }`, the triggering object) is snapshotted to a
    /// fixed `SetPower` at resolution — the layer system can't re-resolve that
    /// referent later. A `Source`-scoped value, by contrast, stays a dynamic
    /// `SetPowerDynamic` (layer-resolvable per object).
    #[test]
    fn animate_snapshots_event_scoped_power_to_fixed_set_power() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Entering".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&entering).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(5);
            obj.toughness = Some(5);
            obj.base_power = Some(5);
            obj.base_toughness = Some(5);
        }
        // The trigger-event source is the entering creature, so
        // `Power { CostPaidObject }` resolves (slot-2 fallback) to its power.
        state.current_trigger_event = Some(crate::types::events::GameEvent::ZoneChanged {
            object_id: entering,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(crate::types::game_state::ZoneChangeRecord::test_minimal(
                entering,
                Some(Zone::Hand),
                Zone::Battlefield,
            )),
        });

        let event_power = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::CostPaidObject,
            },
        };
        let ability = ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Quantity(event_power)),
                toughness: None,
                types: vec![],
                remove_types: vec![],
                keywords: vec![],
                target: TargetFilter::None,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let tce = &state.transient_continuous_effects[0];
        assert!(
            tce.modifications
                .contains(&ContinuousModification::SetPower { value: 5 }),
            "event-scoped base power must be snapshotted to a fixed SetPower(5), got {:?}",
            tce.modifications
        );
        assert!(
            !tce.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::SetPowerDynamic { .. })),
            "must not leave a dynamic SetPowerDynamic for a resolution-only scope"
        );
    }

    /// Guard: a `Source`-scoped dynamic base power must stay `SetPowerDynamic`
    /// so CDA-style "becomes an X/X where X is its power" set effects remain
    /// layer-resolvable (no snapshot).
    #[test]
    fn animate_keeps_source_scoped_power_dynamic() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let source_power = QuantityExpr::Ref {
            qty: QuantityRef::Power {
                scope: crate::types::ability::ObjectScope::Source,
            },
        };
        let ability = ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Quantity(source_power.clone())),
                toughness: None,
                types: vec![],
                remove_types: vec![],
                keywords: vec![],
                target: TargetFilter::None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let tce = &state.transient_continuous_effects[0];
        assert!(
            tce.modifications
                .contains(&ContinuousModification::SetPowerDynamic {
                    value: source_power
                }),
            "source-scoped base power must stay dynamic, got {:?}",
            tce.modifications
        );
    }
}
