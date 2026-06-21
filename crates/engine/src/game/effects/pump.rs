use crate::game::filter;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{
    ContinuousModification, DoublePTMode, Duration, Effect, EffectError, EffectKind, PtValue,
    ResolvedAbility, TargetFilter,
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

    // CR 608.2c + 603.10a: Delegate target resolution to the unified 3-tier
    // dispatch (`resolved_targets`), matching `bounce` / `change_zone`. This
    // routes `SelfRef` through the post-#323 short-circuit so chained
    // `Pump { target: SelfRef }` sub-abilities resolve to the source object
    // rather than inheriting the parent's targets via chain propagation in
    // `effects::mod.rs::resolve_ability_chain`.
    let effective_targets =
        crate::game::targeting::resolved_targets(ability, &target_filter, state);
    let ids = crate::game::effects::effect_object_targets(&target_filter, &effective_targets);

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
    let (mode, target_filter, factor) = match &ability.effect {
        Effect::DoublePT {
            mode,
            target,
            factor,
        } => (mode, target, *factor),
        _ => return Ok(()),
    };

    let dur = ability.duration.clone().unwrap_or(Duration::UntilEndOfTurn);

    // CR 608.2c + 603.10a: Same 3-tier dispatch as `pump.resolve` — `SelfRef`
    // short-circuits to `ability.source_id` so chained
    // `DoublePT { target: SelfRef }` sub-abilities don't inherit parent targets.
    let effective_targets = crate::game::targeting::resolved_targets(ability, target_filter, state);
    let ids = crate::game::effects::effect_object_targets(target_filter, &effective_targets);

    for obj_id in ids {
        let modifications = double_modifications(state, obj_id, mode, factor)?;
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
    let (mode, target_filter, factor) = match &ability.effect {
        Effect::DoublePTAll {
            mode,
            target,
            factor,
        } => (mode, target.clone(), *factor),
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
        let modifications = double_modifications(state, obj_id, mode, factor)?;
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

/// CR 701.10b/c + CR 613.4c: Compute the layer-7c modifications that multiply a
/// creature's current P/T by `factor`. Snapshot the current power/toughness at
/// resolution time, as the CR specifies, and add `(factor - 1)` copies of that
/// snapshot: `factor == 2` ("double") adds +P/+T per CR 701.10b; `factor == 3`
/// ("triple") adds +2P/+2T; etc. CR 701.10c is handled implicitly — adding a
/// negative snapshot multiple yields the correct -X/-Y result for negative P/T.
fn double_modifications(
    state: &GameState,
    obj_id: ObjectId,
    mode: &DoublePTMode,
    factor: u32,
) -> Result<Vec<ContinuousModification>, EffectError> {
    let obj = state
        .objects
        .get(&obj_id)
        .ok_or(EffectError::ObjectNotFound(obj_id))?;
    // CR 701.10b: "double" adds 1x the snapshot; generalize to (factor - 1)x.
    let added_copies = i32::try_from(factor).unwrap_or(i32::MAX).saturating_sub(1);
    let mut mods = Vec::new();
    let add_power = matches!(mode, DoublePTMode::Power | DoublePTMode::PowerAndToughness);
    let add_toughness = matches!(
        mode,
        DoublePTMode::Toughness | DoublePTMode::PowerAndToughness
    );
    if add_power {
        if let Some(p) = obj.power {
            mods.push(ContinuousModification::AddPower {
                value: p.saturating_mul(added_copies),
            });
        }
    }
    if add_toughness {
        if let Some(t) = obj.toughness {
            mods.push(ContinuousModification::AddToughness {
                value: t.saturating_mul(added_copies),
            });
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
        PtValue::Variable(value) => {
            if let Some(resolved) = resolve_variable_pt(value, ability) {
                if resolved != 0 {
                    mods.push(ContinuousModification::AddPower { value: resolved });
                }
            }
        }
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
        PtValue::Variable(value) => {
            if let Some(resolved) = resolve_variable_pt(value, ability) {
                if resolved != 0 {
                    mods.push(ContinuousModification::AddToughness { value: resolved });
                }
            }
        }
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

fn resolve_variable_pt(value: &str, ability: &ResolvedAbility) -> Option<i32> {
    let chosen = i32::try_from(ability.chosen_x?).unwrap_or(i32::MAX);
    match value {
        "X" | "x" => Some(chosen),
        "-X" | "-x" => Some(chosen.saturating_neg()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::{PtValue, TargetFilter, TargetRef, TypedFilter};
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
    fn pump_resolves_variable_pt_against_chosen_x() {
        let mut state = GameState::new_two_player(42);
        let obj_id = make_creature(&mut state, "Bear", 5, 5, PlayerId(0));

        let mut ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Variable("-X".to_string()),
                toughness: PtValue::Variable("-X".to_string()),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        ability.chosen_x = Some(3);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&obj_id].power, Some(2));
        assert_eq!(state.objects[&obj_id].toughness, Some(2));
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

    /// CR 608.2c (issue #323 class): a chained `Pump { target: SelfRef }`
    /// sub-ability must pump the source object even when chain target
    /// propagation in `effects::mod.rs::resolve_ability_chain` injected the
    /// parent's targets into `ability.targets`. Pre-fix the resolver checked
    /// `SelfRef && ability.targets.is_empty()` locally, so a propagated parent
    /// target would route through the `ability.targets` branch and pump the
    /// wrong creature. Post-fix the resolver delegates to `resolved_targets`,
    /// which short-circuits `SelfRef` to `[source_id]` unconditionally.
    #[test]
    fn pump_selfref_overrides_propagated_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let source = make_creature(&mut state, "Source", 2, 2, PlayerId(0));
        let other = make_creature(&mut state, "Other", 3, 3, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: TargetFilter::SelfRef,
            },
            // Simulate chain target propagation from a parent that targeted
            // `other`. SelfRef must override this and pump the source instead.
            vec![TargetRef::Object(other)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&source].power,
            Some(3),
            "SelfRef pump must apply to source even with propagated parent targets"
        );
        assert_eq!(state.objects[&source].toughness, Some(3));
        assert_eq!(
            state.objects[&other].power,
            Some(3),
            "propagated parent target must NOT be pumped by SelfRef sub-ability"
        );
        assert_eq!(state.objects[&other].toughness, Some(3));
    }

    /// CR 701.10c (issue #323 class): chained `DoublePT { target: SelfRef }`
    /// sub-ability must double the source object's P/T even when chain target
    /// propagation injected the parent's targets.
    #[test]
    fn double_pt_selfref_overrides_propagated_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let source = make_creature(&mut state, "Source", 2, 2, PlayerId(0));
        let other = make_creature(&mut state, "Other", 3, 3, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::DoublePT {
                mode: DoublePTMode::PowerAndToughness,
                target: TargetFilter::SelfRef,
                factor: 2,
            },
            vec![TargetRef::Object(other)],
            source,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_double_pt(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&source].power,
            Some(4),
            "SelfRef double_pt must apply to source"
        );
        assert_eq!(state.objects[&source].toughness, Some(4));
        assert_eq!(
            state.objects[&other].power,
            Some(3),
            "propagated parent target must NOT be doubled"
        );
    }

    /// CR 701.10a + CR 613.4c: `factor: 2` ("double") adds +P/+T (Tifa's Limit
    /// Break — Meteor Strikes; The Skullspore Nexus — power-only). Discriminator:
    /// a 3/3 becomes 6/6, which cannot coincide with a no-op (3 != 6). Reverting
    /// the `factor` math (e.g. dropping the multiplier) flips the 6/6 assertion.
    #[test]
    fn double_pt_factor_two_doubles_power_and_toughness() {
        let mut state = GameState::new_two_player(7);
        let obj = make_creature(&mut state, "Cloud", 3, 3, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::DoublePT {
                mode: DoublePTMode::PowerAndToughness,
                target: TargetFilter::Any,
                factor: 2,
            },
            vec![TargetRef::Object(obj)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_double_pt(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&obj].power, Some(6), "double 3 power → 6");
        assert_eq!(
            state.objects[&obj].toughness,
            Some(6),
            "double 3 toughness → 6"
        );
    }

    /// CR 701.10a + CR 613.4c: `factor: 3` ("triple" — Tifa's Limit Break, Final
    /// Heaven) adds +2P/+2T so a 3/3 becomes 9/9. Discriminator: 9/9 is distinct
    /// from both the no-op (3/3) and the double result (6/6), so reverting the
    /// `factor` parameterization (which would fall back to doubling) flips this.
    #[test]
    fn double_pt_factor_three_triples_power_and_toughness() {
        let mut state = GameState::new_two_player(7);
        let obj = make_creature(&mut state, "Cloud", 3, 3, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::DoublePT {
                mode: DoublePTMode::PowerAndToughness,
                target: TargetFilter::Any,
                factor: 3,
            },
            vec![TargetRef::Object(obj)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_double_pt(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&obj].power, Some(9), "triple 3 power → 9");
        assert_eq!(
            state.objects[&obj].toughness,
            Some(9),
            "triple 3 toughness → 9"
        );
    }

    /// CR 701.10a + CR 613.4c: power-only multiply (The Skullspore Nexus —
    /// "Double target creature's power") with `factor: 3` leaves toughness
    /// untouched. Discriminator: power 4 → 12, toughness stays 5.
    #[test]
    fn double_pt_factor_three_power_only_leaves_toughness() {
        let mut state = GameState::new_two_player(7);
        let obj = make_creature(&mut state, "Sephiroth", 4, 5, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::DoublePT {
                mode: DoublePTMode::Power,
                target: TargetFilter::Any,
                factor: 3,
            },
            vec![TargetRef::Object(obj)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_double_pt(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(state.objects[&obj].power, Some(12), "triple 4 power → 12");
        assert_eq!(
            state.objects[&obj].toughness,
            Some(5),
            "power-only multiply must not change toughness"
        );
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
        state.layers_dirty.mark_full();
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
