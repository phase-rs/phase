use crate::game::filter;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{
    ContinuousModification, DoublePTMode, Duration, Effect, EffectError, EffectKind, PtValue,
    ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;

/// CR 611.2a: Continuous effect from resolving spell — lasts until end of turn.
/// Registers transient continuous effects through the layer system so that
/// pump modifications survive layer recalculation and expire correctly.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (power, toughness, target_filter) = match &ability.effect {
        Effect::Pump {
            power,
            toughness,
            target,
        } => (power, toughness, target),
        _ => return Ok(()),
    };

    let dur = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);
    let target_filter = crate::game::effects::resolved_object_filter(ability, target_filter);

    // SelfRef with no explicit targets means pump the source object itself.
    let ids: Vec<ObjectId> =
        if matches!(target_filter, TargetFilter::SelfRef) && ability.targets.is_empty() {
            vec![ability.source_id]
        } else {
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
        };

    let modifications = pt_modifications(power, toughness, state, ability);

    for obj_id in ids {
        if !state.objects.contains_key(&obj_id) {
            return Err(EffectError::ObjectNotFound(obj_id));
        }
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            dur.clone(),
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

/// Pump all creatures matching the typed TargetFilter on the battlefield.
/// Reads power/toughness/filter from `Effect::PumpAll`.
/// CR 611.2a: Registers transient continuous effects through the layer system.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (power, toughness, target_filter) = match &ability.effect {
        Effect::PumpAll {
            power,
            toughness,
            target,
        } => (power, toughness, target.clone()),
        _ => return Ok(()),
    };

    let dur = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);

    let modifications = pt_modifications(power, toughness, state, ability);

    // Collect matching object IDs first to avoid borrow conflicts.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = filter::FilterContext::from_ability(ability);
    let matching: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter(|id| filter::matches_target_filter(state, **id, &target_filter, &ctx))
        .copied()
        .collect();

    for obj_id in matching {
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            dur.clone(),
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

/// CR 701.10a: "Doubling a creature's power and/or toughness creates a continuous effect."
/// CR 701.10b: "To double a creature's power, that creature gets +X/+0,
/// where X is that creature's power as the spell or ability resolves."
/// CR 701.10c: Negative power handling — adding current value works for both cases.
pub fn resolve_double_pt(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (mode, target_filter) = match &ability.effect {
        Effect::DoublePT { mode, target } => (mode, target),
        _ => return Ok(()),
    };

    let dur = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);

    let ids: Vec<ObjectId> =
        if matches!(target_filter, TargetFilter::SelfRef) && ability.targets.is_empty() {
            vec![ability.source_id]
        } else {
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
        };

    for obj_id in ids {
        let modifications = double_modifications(state, obj_id, mode)?;
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            dur.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            modifications,
            None,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.10a: Double power/toughness of all creatures matching a filter.
pub fn resolve_double_pt_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (mode, target_filter) = match &ability.effect {
        Effect::DoublePTAll { mode, target } => (mode, target.clone()),
        _ => return Ok(()),
    };

    let dur = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);

    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = filter::FilterContext::from_ability(ability);
    let matching: Vec<ObjectId> = state
        .battlefield
        .iter()
        .filter(|id| filter::matches_target_filter(state, **id, &target_filter, &ctx))
        .copied()
        .collect();

    for obj_id in matching {
        let modifications = double_modifications(state, obj_id, mode)?;
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            dur.clone(),
            TargetFilter::SpecificObject { id: obj_id },
            modifications,
            None,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 701.10b/c: Compute +X/+Y modifications that double a creature's current P/T.
/// Snapshot the current power/toughness at resolution time, as the CR specifies.
fn double_modifications(
    state: &GameState,
    obj_id: ObjectId,
    mode: &DoublePTMode,
) -> Result<Vec<ContinuousModification>, EffectError> {
    let obj = state
        .objects
        .get(&obj_id)
        .ok_or(EffectError::ObjectNotFound(obj_id))?;
    let mut mods = Vec::new();
    match mode {
        DoublePTMode::Power => {
            if let Some(p) = obj.power {
                mods.push(ContinuousModification::AddPower { value: p });
            }
        }
        DoublePTMode::Toughness => {
            if let Some(t) = obj.toughness {
                mods.push(ContinuousModification::AddToughness { value: t });
            }
        }
        DoublePTMode::PowerAndToughness => {
            if let Some(p) = obj.power {
                mods.push(ContinuousModification::AddPower { value: p });
            }
            if let Some(t) = obj.toughness {
                mods.push(ContinuousModification::AddToughness { value: t });
            }
        }
    }
    Ok(mods)
}

/// Build `ContinuousModification` entries for a P/T pump effect.
/// Fixed values become `AddPower`/`AddToughness`; dynamic quantities
/// become `AddDynamicPower`/`AddDynamicToughness` for layer evaluation.
fn pt_modifications(
    power: &PtValue,
    toughness: &PtValue,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Vec<ContinuousModification> {
    let mut mods = Vec::new();
    match power {
        PtValue::Fixed(n) if *n != 0 => {
            mods.push(ContinuousModification::AddPower { value: *n });
        }
        PtValue::Variable(_) => {} // X-spell: value determined at cast time (TODO)
        PtValue::Quantity(expr) => {
            let resolved = resolve_quantity_with_targets(state, expr, ability);
            if resolved != 0 {
                mods.push(ContinuousModification::AddPower { value: resolved });
            }
        }
        _ => {}
    }
    match toughness {
        PtValue::Fixed(n) if *n != 0 => {
            mods.push(ContinuousModification::AddToughness { value: *n });
        }
        PtValue::Variable(_) => {}
        PtValue::Quantity(expr) => {
            let resolved = resolve_quantity_with_targets(state, expr, ability);
            if resolved != 0 {
                mods.push(ContinuousModification::AddToughness { value: resolved });
            }
        }
        _ => {}
    }
    mods
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::{PtValue, TargetFilter, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// Helper: create a battlefield creature with base P/T set for layer evaluation.
    fn make_creature(
        state: &mut GameState,
        name: &str,
        power: i32,
        toughness: i32,
        owner: PlayerId,
    ) -> ObjectId {
        let id = create_object(state, CardId(0), owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.base_power = Some(power);
        obj.base_toughness = Some(toughness);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    #[test]
    fn pump_increases_power_and_toughness() {
        let mut state = GameState::new_two_player(42);
        let obj_id = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&obj_id].power, Some(5));
        assert_eq!(state.objects[&obj_id].toughness, Some(5));
    }

    #[test]
    fn pump_with_negative_values() {
        let mut state = GameState::new_two_player(42);
        let obj_id = make_creature(&mut state, "Bear", 3, 3, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(-2),
                toughness: PtValue::Fixed(-2),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&obj_id].power, Some(1));
        assert_eq!(state.objects[&obj_id].toughness, Some(1));
    }

    #[test]
    fn pump_all_your_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));
        let bear2 = make_creature(&mut state, "Bear 2", 1, 1, PlayerId(0));
        // Opponent's creature (should NOT be pumped)
        let opp = make_creature(&mut state, "Opp Bear", 3, 3, PlayerId(1));

        let ability = ResolvedAbility::new(
            Effect::PumpAll {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You)
                    .into(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&bear1].power, Some(3));
        assert_eq!(state.objects[&bear1].toughness, Some(3));
        assert_eq!(state.objects[&bear2].power, Some(2));
        assert_eq!(state.objects[&bear2].toughness, Some(2));
        // Opponent unchanged
        assert_eq!(state.objects[&opp].power, Some(3));
        assert_eq!(state.objects[&opp].toughness, Some(3));
    }

    /// Regression: Prowess-style abilities use `SelfRef` with an empty `targets` list.
    /// The resolver must fall back to `source_id` rather than iterating zero targets.
    #[test]
    fn pump_selfref_with_empty_targets_pumps_source() {
        let mut state = GameState::new_two_player(42);
        let swiftspear = make_creature(&mut state, "Monastery Swiftspear", 1, 2, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::SelfRef,
            },
            vec![], // empty — SelfRef must resolve via source_id
            swiftspear,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&swiftspear].power, Some(2));
        assert_eq!(state.objects[&swiftspear].toughness, Some(3));
    }

    /// Verify pump survives layer recalculation — the original bug.
    #[test]
    fn pump_survives_layer_recalculation() {
        let mut state = GameState::new_two_player(42);
        let obj_id = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // First evaluation
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&obj_id].power, Some(5));

        // Trigger another layer recalculation — pump must persist
        state.layers_dirty = true;
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&obj_id].power, Some(5));
        assert_eq!(state.objects[&obj_id].toughness, Some(5));
    }

    /// CR 613.4b: `SetPowerDynamic`/`SetToughnessDynamic` apply at layer 7b
    /// using the spell source's `cost_x_paid`. Biomass Mutation shape:
    /// creatures you control have base power and toughness X/X.
    /// Ensures +1/+1 counters (layer 7e) remain additive after the set.
    #[test]
    fn base_pt_dynamic_sets_power_from_cost_x_paid_and_counters_add() {
        use crate::types::ability::{ContinuousModification, QuantityExpr, QuantityRef};
        use crate::types::counter::CounterType;

        let mut state = GameState::new_two_player(42);
        let b22 = make_creature(&mut state, "Bear 2/2", 2, 2, PlayerId(0));
        let b44 = make_creature(&mut state, "Bear 4/4", 4, 4, PlayerId(0));
        let b11 = make_creature(&mut state, "Bear 1/1", 1, 1, PlayerId(0));
        // Add a +1/+1 counter on b22 to verify layered addition (7e after 7b).
        state
            .objects
            .get_mut(&b22)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        // Source = Biomass Mutation-like spell with X=3 paid.
        let source = create_object(
            &mut state,
            CardId(999),
            PlayerId(0),
            "Biomass Mutation".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&source).unwrap().cost_x_paid = Some(3);

        // Register the transient effect for each matching creature — this
        // mirrors what `GenericEffect` resolution does for a broadcast filter.
        for id in [b22, b44, b11] {
            state.add_transient_continuous_effect(
                source,
                PlayerId(0),
                Duration::UntilEndOfTurn,
                TargetFilter::SpecificObject { id },
                vec![
                    ContinuousModification::SetPowerDynamic {
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::CostXPaid,
                        },
                    },
                    ContinuousModification::SetToughnessDynamic {
                        value: QuantityExpr::Ref {
                            qty: QuantityRef::CostXPaid,
                        },
                    },
                ],
                None,
            );
        }
        evaluate_layers(&mut state);

        // b22 had a +1/+1 counter: base becomes 3/3, counter adds 1 → 4/4.
        assert_eq!(state.objects[&b22].power, Some(4));
        assert_eq!(state.objects[&b22].toughness, Some(4));
        // b44 and b11 become 3/3 exactly.
        assert_eq!(state.objects[&b44].power, Some(3));
        assert_eq!(state.objects[&b44].toughness, Some(3));
        assert_eq!(state.objects[&b11].power, Some(3));
        assert_eq!(state.objects[&b11].toughness, Some(3));
    }

    /// Verify pump expires at end of turn cleanup.
    #[test]
    fn pump_expires_at_end_of_turn() {
        use crate::game::layers::prune_end_of_turn_effects;

        let mut state = GameState::new_two_player(42);
        let obj_id = make_creature(&mut state, "Bear", 2, 2, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&obj_id].power, Some(5));

        // End of turn cleanup should remove the effect
        prune_end_of_turn_effects(&mut state);
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&obj_id].power, Some(2));
        assert_eq!(state.objects[&obj_id].toughness, Some(2));
    }
}
