use crate::types::ability::{
    AbilityDefinition, DelayedTriggerCondition, Effect, EffectError, EffectKind, ManaProduction,
    PtValue, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TargetRef,
};
#[cfg(test)]
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::TrackedSetId;
use crate::types::zones::Zone;

/// CR 603.7: Create a delayed triggered ability during resolution.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (mut condition, effect_def, uses_tracked_set) = match &ability.effect {
        Effect::CreateDelayedTrigger {
            condition,
            effect,
            uses_tracked_set,
        } => (
            condition.clone(),
            effect.as_ref().clone(),
            *uses_tracked_set,
        ),
        _ => {
            return Err(EffectError::MissingParam(
                "CreateDelayedTrigger".to_string(),
            ))
        }
    };

    bind_contextual_filter_to_condition(&mut condition, &ability.targets);

    // CR 505.1 + CR 603.7a: "your next <phase>" binds the trigger to the
    // ability's controller. The parser emits a placeholder `PlayerId(0)` in
    // `AtNextPhaseForPlayer.player` because compile-time AST has no access to
    // runtime player ids; rewrite here to the actual controller at resolve
    // time. Mirrors the `bind_contextual_filter_to_condition` pattern above.
    if let DelayedTriggerCondition::AtNextPhaseForPlayer { player, .. } = &mut condition {
        *player = ability.controller;
    }

    // CR 603.7c: Build the delayed trigger's resolved ability from the full
    // definition, preserving sub_ability chains. A bare `effect_def.effect`
    // clone dropped continuation clauses — e.g. Dalkovan Encampment's
    // "create … Warrior tokens … sacrifice them at the beginning of the next
    // end step" inner chain (Token → CreateDelayedTrigger{Sacrifice}) never
    // reached runtime when registered inside a WheneverEvent delayed trigger.
    let mut delayed_ability = crate::game::ability_utils::build_resolved_from_def(
        &effect_def,
        ability.source_id,
        ability.controller,
    );

    // CR 603.7: Bind the most recent tracked set to the effect's target filter,
    // resolving sentinel TrackedSetId(0) or TargetFilter::Any, and upgrading
    // ChangeZone → ChangeZoneAll for delayed triggers (which have empty explicit targets).
    if uses_tracked_set {
        if let Some(real_id) = crate::game::targeting::latest_tracked_set_id(state) {
            bind_tracked_set_to_condition(&mut condition, real_id);
            bind_tracked_set_to_ability_chain(&mut delayed_ability, real_id);
        }
    }

    // CR 603.7c: A delayed trigger whose inner effect targets the trigger's
    // source object via TriggeringSource or ParentTarget must snapshot that
    // object at creation time. At creation, current_trigger_event =
    // ZoneChanged { dying_creature } and TriggeringSource resolves correctly.
    //
    // Without the snapshot, at end-step firing:
    //   current_trigger_event = PhaseChanged { End }
    //   - is_pure_event_context_filter(TriggeringSource) = true → block IS entered
    //   - resolve_event_context_target returns None (PhaseChanged carries no
    //     ZoneChanged source object)
    //   - execution falls through to chosen_targets_satisfy_filter check
    //   - chosen_targets_satisfy_filter(TriggeringSource) = false
    //     (matches_target_filter always returns false for TriggeringSource)
    //   - second resolve_event_context_target attempt → None
    //   - final ability.targets.clone() fallback returns [] (empty snapshot)
    //     → the zone move silently skips (bugs #2883 Grave Betrayal,
    //       #2886 Liliana emblem)
    //
    // With the snapshot: delayed_ability.targets = [dying_creature] at
    // creation, and the final fallback correctly returns [dying_creature].
    //
    // CR 603.7c: See separate branch for LastCreated snapshots.
    let snapshot_targets = if super::ability_refs_triggering_source(&delayed_ability) {
        // CR 603.7c: TriggeringSource always reads the event context (the dying
        // creature from the ZoneChanged event), not the parent ability's chosen
        // targets. Bypasses parent_target_snapshot's ability.targets early-return,
        // which is correct for ParentTarget (Flickerwisp) but wrong here.
        crate::game::targeting::resolve_event_context_target(
            state,
            &crate::types::ability::TargetFilter::TriggeringSource,
            ability.source_id,
        )
        .map(|t| vec![t])
        .unwrap_or_default()
    } else if super::effect_refs_parent_target(&delayed_ability.effect) {
        parent_target_snapshot(state, ability)
    } else if effect_references_last_created(&delayed_ability.effect)
        && !state.last_created_token_ids.is_empty()
    {
        state
            .last_created_token_ids
            .iter()
            .map(|&id| TargetRef::Object(id))
            .collect()
    } else {
        vec![]
    };

    if super::ability_refs_triggering_source(&delayed_ability) {
        if let Some(zone) = triggering_source_destination_zone(state) {
            stamp_triggering_source_origins_in_ability_chain(&mut delayed_ability, zone);
        }
    }

    // CR 603.7 + CR 608.2h: Snapshot parent-resolution-dependent
    // quantity refs to Fixed before the delayed trigger gets stashed.
    // After this call, the delayed ability chain holds no parent context refs.
    snapshot_parent_dependent_quantities_in_ability_chain(&mut delayed_ability, state, ability);

    delayed_ability.targets = snapshot_targets;
    // CR 603.7c: A delayed triggered ability that refers to information from
    // its creation event keeps that creation-time binding for later resolution.
    delayed_ability.scoped_player = ability.scoped_player;

    // CR 603.7c: Most delayed triggers fire once and are removed.
    // WheneverEvent triggers fire each time and persist until end-of-turn cleanup.
    let one_shot = !matches!(
        condition,
        crate::types::ability::DelayedTriggerCondition::WheneverEvent { .. }
    );
    state.delayed_triggers.push(DelayedTrigger {
        condition,
        ability: delayed_ability,
        controller: ability.controller,
        source_id: ability.source_id,
        one_shot,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::CreateDelayedTrigger,
        source_id: ability.source_id,
    });

    Ok(())
}

/// CR 603.7c: A delayed triggered ability that refers to a particular object
/// snapshots that object at creation time. The parent ability's chosen targets
/// (e.g. Flickerwisp's exiled victim) are the snapshot; only when the parent
/// carried no targets do we fall back to the triggering source.
fn parent_target_snapshot(state: &GameState, ability: &ResolvedAbility) -> Vec<TargetRef> {
    if !ability.targets.is_empty() {
        return ability.targets.clone();
    }

    crate::game::targeting::resolve_event_context_target(
        state,
        &TargetFilter::TriggeringSource,
        ability.source_id,
    )
    .map(|target| vec![target])
    .unwrap_or_default()
}

fn triggering_source_destination_zone(state: &GameState) -> Option<Zone> {
    match state.current_trigger_event.as_ref()? {
        GameEvent::ZoneChanged { to, .. } => Some(*to),
        _ => None,
    }
}

/// CR 603.7c + CR 400.7: A delayed trigger that snapshots a zone-change event's
/// `TriggeringSource` may affect that object only if it remains in the event's
/// destination zone. Stamp unset `origin` guards so the zone-move resolver can
/// enforce that creation-event binding at delayed-trigger resolution.
fn stamp_triggering_source_origins_in_ability_chain(ability: &mut ResolvedAbility, expected: Zone) {
    stamp_triggering_source_origins(&mut ability.effect, expected);
    if let Some(sub_ability) = ability.sub_ability.as_deref_mut() {
        stamp_triggering_source_origins_in_ability_chain(sub_ability, expected);
    }
    if let Some(else_ability) = ability.else_ability.as_deref_mut() {
        stamp_triggering_source_origins_in_ability_chain(else_ability, expected);
    }
}

fn stamp_triggering_source_origins_in_definition_chain(
    ability: &mut AbilityDefinition,
    expected: Zone,
) {
    stamp_triggering_source_origins(&mut ability.effect, expected);
    if let Some(sub_ability) = ability.sub_ability.as_deref_mut() {
        stamp_triggering_source_origins_in_definition_chain(sub_ability, expected);
    }
    if let Some(else_ability) = ability.else_ability.as_deref_mut() {
        stamp_triggering_source_origins_in_definition_chain(else_ability, expected);
    }
}

fn stamp_triggering_source_origins(effect: &mut Effect, expected: Zone) {
    match effect {
        Effect::ChangeZone { origin, target, .. }
        | Effect::ChangeZoneAll { origin, target, .. }
            if origin.is_none() && super::filter_refs_triggering_source(target) =>
        {
            *origin = Some(expected);
        }
        Effect::CreateDelayedTrigger { effect, .. } => {
            stamp_triggering_source_origins_in_definition_chain(effect, expected);
        }
        _ => {}
    }
}

/// CR 603.7c: Walk an effect (and any nested sub-ability
/// definitions) looking for `TargetFilter::LastCreated` in a target position.
/// Used by `resolve` to decide whether to snapshot `last_created_token_ids`
/// into the delayed ability's `targets` at creation time.
fn effect_references_last_created(effect: &Effect) -> bool {
    matches!(effect.target_filter(), Some(TargetFilter::LastCreated))
}

fn bind_contextual_filter_to_condition(
    condition: &mut DelayedTriggerCondition,
    parent_targets: &[TargetRef],
) {
    match condition {
        DelayedTriggerCondition::WhenDiesOrExiled { filter } => {
            bind_parent_target_filter(filter, parent_targets);
        }
        DelayedTriggerCondition::WheneverEvent { trigger }
        | DelayedTriggerCondition::WhenNextEvent { trigger } => {
            for filter in [
                &mut trigger.valid_card,
                &mut trigger.valid_source,
                &mut trigger.valid_target,
            ]
            .into_iter()
            .flatten()
            {
                bind_parent_target_filter(filter, parent_targets);
            }
        }
        _ => {}
    }
}

fn bind_parent_target_filter(filter: &mut TargetFilter, parent_targets: &[TargetRef]) {
    *filter = concrete_parent_target_filter(filter, parent_targets);
}

fn concrete_parent_target_filter(
    filter: &TargetFilter,
    parent_targets: &[TargetRef],
) -> TargetFilter {
    let filter = crate::game::filter::normalize_contextual_filter(filter, parent_targets);
    match filter {
        TargetFilter::ParentTarget => parent_targets_filter(parent_targets),
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(concrete_parent_target_filter(&filter, parent_targets)),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters
                .iter()
                .map(|filter| concrete_parent_target_filter(filter, parent_targets))
                .collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters
                .iter()
                .map(|filter| concrete_parent_target_filter(filter, parent_targets))
                .collect(),
        },
        other => other,
    }
}

fn parent_targets_filter(parent_targets: &[TargetRef]) -> TargetFilter {
    let targets: Vec<_> = parent_targets
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => TargetFilter::SpecificObject { id: *id },
            TargetRef::Player(id) => TargetFilter::SpecificPlayer { id: *id },
        })
        .collect();

    match targets.as_slice() {
        [] => TargetFilter::Any,
        [target] => target.clone(),
        _ => TargetFilter::Or { filters: targets },
    }
}

fn bind_tracked_set_to_condition(condition: &mut DelayedTriggerCondition, real_id: TrackedSetId) {
    let filter = match condition {
        DelayedTriggerCondition::WhenDies { filter }
        | DelayedTriggerCondition::WhenLeavesPlayFiltered { filter }
        | DelayedTriggerCondition::WhenEntersBattlefield { filter }
        | DelayedTriggerCondition::WhenDiesOrExiled { filter } => filter,
        _ => return,
    };

    if matches!(
        filter,
        TargetFilter::ParentTarget
            | TargetFilter::Any
            | TargetFilter::TrackedSet {
                id: TrackedSetId(0)
            }
    ) {
        *filter = TargetFilter::TrackedSet { id: real_id };
    }
}

/// CR 603.7 + CR 202.3 + CR 608.2h: Snapshot QuantityRef leaves in the
/// delayed trigger's inner effect that depend on parent-resolution
/// context (the countered spell on the stack, the cast-time mana
/// snapshot, etc.). After this walker runs, the delayed trigger holds
/// no references to parent context — it fires self-contained at
/// `AtNextPhaseForPlayer` time with `Fixed` values everywhere.
///
/// Handles two scopes that the parser emits for "that spell" anaphors:
/// - `ObjectManaValue { CostPaidObject }` from "that spell's mana value"
/// - `ObjectManaValue { Target }` (treated identically)
///
/// Both resolve via the parent ability's `targets[0]` rather than the
/// standard resolver chain (which keys off `cost_paid_object` /
/// `current_trigger_event`, neither of which is set during a spell-card
/// resolution like Mana Drain or Mana Sculpt).
fn snapshot_parent_dependent_quantities(
    effect: &mut Effect,
    state: &GameState,
    ability: &ResolvedAbility,
) {
    match effect {
        Effect::Mana {
            produced:
                ManaProduction::Colorless { count }
                | ManaProduction::AnyOneColor { count, .. }
                | ManaProduction::AnyCombination { count, .. }
                | ManaProduction::ChosenColor { count, .. },
            ..
        } => {
            snapshot_quantity_expr(count, state, ability);
        }
        Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. } => {
            snapshot_quantity_expr(amount, state, ability);
        }
        Effect::Draw { count: amount, .. }
        | Effect::Mill { count: amount, .. }
        | Effect::PutCounter { count: amount, .. } => {
            snapshot_quantity_expr(amount, state, ability);
        }
        Effect::Pump {
            power, toughness, ..
        }
        | Effect::PumpAll {
            power, toughness, ..
        } => {
            snapshot_pt_value(power, state, ability);
            snapshot_pt_value(toughness, state, ability);
        }
        // CR 603.7c + CR 122.2: Snapshot counter-relative quantities inside
        // ChangeZone.enter_with_counters so the LKI-based counter count is
        // frozen at delayed trigger creation time (before step transition
        // clears the LKI cache).
        Effect::ChangeZone {
            enter_with_counters,
            ..
        } => {
            for (_, qty) in enter_with_counters.iter_mut() {
                snapshot_quantity_expr(qty, state, ability);
            }
        }
        _ => {}
    }
}

fn snapshot_parent_dependent_quantities_in_ability_chain(
    ability: &mut ResolvedAbility,
    state: &GameState,
    parent: &ResolvedAbility,
) {
    snapshot_parent_dependent_quantities(&mut ability.effect, state, parent);
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        snapshot_parent_dependent_quantities_in_ability_chain(sub_ability, state, parent);
    }
    if let Some(else_ability) = ability.else_ability.as_mut() {
        snapshot_parent_dependent_quantities_in_ability_chain(else_ability, state, parent);
    }
}

fn snapshot_pt_value(value: &mut PtValue, state: &GameState, ability: &ResolvedAbility) {
    if let PtValue::Quantity(expr) = value {
        snapshot_quantity_expr(expr, state, ability);
    }
}

/// Recursively walks a QuantityExpr tree, snapshotting any snapshottable
/// leaf to `Fixed { value }`. Non-snapshottable leaves pass through.
fn snapshot_quantity_expr(expr: &mut QuantityExpr, state: &GameState, ability: &ResolvedAbility) {
    match expr {
        QuantityExpr::Ref { qty } => {
            if let Some(value) = snapshot_quantity_ref(qty, state, ability) {
                *expr = QuantityExpr::Fixed { value };
            }
        }
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. } => {
            snapshot_quantity_expr(inner, state, ability);
        }
        QuantityExpr::Sum { exprs } => {
            for e in exprs.iter_mut() {
                snapshot_quantity_expr(e, state, ability);
            }
        }
        QuantityExpr::Difference { left, right } => {
            snapshot_quantity_expr(left, state, ability);
            snapshot_quantity_expr(right, state, ability);
        }
        QuantityExpr::UpTo { max } => {
            snapshot_quantity_expr(max, state, ability);
        }
        QuantityExpr::Power { exponent, .. } => {
            snapshot_quantity_expr(exponent, state, ability);
        }
        QuantityExpr::Fixed { .. } => {}
    }
}

/// Resolve a single snapshottable QuantityRef leaf to a concrete value,
/// or return None if the ref is not snapshottable (caller leaves it
/// unchanged). Reads the parent ability's `targets[0]` for the spell
/// reference.
fn snapshot_quantity_ref(
    qty: &QuantityRef,
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<i32> {
    use crate::types::ability::ObjectScope;
    // CR 603.7c + CR 400.7: CountersOn { Source } uses ability.source_id,
    // not targets — handle it before the target_object_id extraction which
    // early-returns None when targets is empty (common for dies triggers).
    if let QuantityRef::CountersOn {
        scope: ObjectScope::Source,
        counter_type,
    } = qty
    {
        let source_id = ability.source_id;
        // Mirrors resolve_counters_on_scope (quantity.rs:2778): live first,
        // LKI fallback.
        let live = state.objects.get(&source_id);
        let on_battlefield =
            live.is_some_and(|obj| obj.zone == crate::types::zones::Zone::Battlefield);
        if !on_battlefield {
            if let Some(lki) = state.lki_cache.get(&source_id) {
                return Some(crate::game::quantity::counter_count_from_map(
                    &lki.counters,
                    counter_type.as_ref(),
                ));
            }
        }
        return live.map(|obj| {
            crate::game::quantity::counter_count_from_map(&obj.counters, counter_type.as_ref())
        });
    }
    let target_object_id = ability.targets.iter().find_map(|t| match t {
        TargetRef::Object(id) => Some(*id),
        _ => None,
    })?;
    match qty {
        // CR 608.2c + CR 608.2k: All four target-bound object-scope variants
        // (`CostPaidObject` cost/trigger referent, `Target` first-target slot,
        // `Anaphoric` pronoun and `Demonstrative` noun-phrase
        // instruction-order referents) bake to the parent's first object target
        // at snapshot time. `Demonstrative` carries the bare-anaphoric
        // possessives ("that spell's mana value", Mana Drain class) that
        // `classify_possessive_referent` routes off `CostPaidObject`; snapshot
        // baking must preserve the prior behavior — read the parent target's
        // mana value now and freeze it as `Fixed` — or the delayed trigger
        // fires later with an empty context and produces 0.
        QuantityRef::ObjectManaValue {
            scope: ObjectScope::CostPaidObject,
        }
        | QuantityRef::ObjectManaValue {
            scope: ObjectScope::Target,
        }
        | QuantityRef::ObjectManaValue {
            scope: ObjectScope::Anaphoric,
        }
        | QuantityRef::ObjectManaValue {
            scope: ObjectScope::Demonstrative,
        } => {
            // Read live state first, LKI as fallback, 0 if neither.
            // CR 202.3e: include cost_x_paid for on-stack spells.
            let value = state
                .objects
                .get(&target_object_id)
                .map(|obj| obj.mana_cost.mana_value_with_x(obj.zone, obj.cost_x_paid) as i32)
                .or_else(|| {
                    state
                        .lki_cache
                        .get(&target_object_id)
                        .map(|lki| lki.mana_value as i32)
                })
                .unwrap_or(0);
            Some(value)
        }
        QuantityRef::ManaSpentToCast {
            scope: crate::types::ability::CastManaObjectScope::TriggeringSpell,
            metric,
        } => {
            let filter_ctx =
                crate::game::filter::FilterContext::from_source(state, ability.source_id);
            crate::game::quantity::resolve_mana_spent_to_cast_metric(
                state,
                target_object_id,
                metric,
                &filter_ctx,
            )
            .or(Some(0))
        }
        _ => None,
    }
}

/// Bind a tracked set to an effect's target filter, resolve origin zone,
/// and upgrade ChangeZone → ChangeZoneAll if needed.
///
/// Three responsibilities:
/// 1. Resolve TrackedSetId(0) sentinel → TrackedSetId(real_id)
/// 2. Bind TargetFilter::Any → TrackedSet(real_id) for implicit pronouns
/// 3. Set origin zone to Exile (tracked sets are always from exile)
fn bind_tracked_set_to_effect(effect: &mut Effect, real_id: TrackedSetId) {
    match effect {
        Effect::ChangeZoneAll { origin, target, .. } => {
            // Resolve target filter
            match target {
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                }
                | TargetFilter::Any => {
                    *target = TargetFilter::TrackedSet { id: real_id };
                }
                _ => {}
            }
            // CR 400.7: Tracked objects are in exile; set origin for zone scan
            if origin.is_none() {
                *origin = Some(Zone::Exile);
            }
        }
        // Upgrade ChangeZone → ChangeZoneAll: ChangeZone uses ability.targets (empty for
        // delayed triggers), so it would move nothing. ChangeZoneAll scans by filter.
        Effect::ChangeZone { destination, .. } => {
            *effect = Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: *destination,
                target: TargetFilter::TrackedSet { id: real_id },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            };
        }
        _ => {}
    }
}

fn bind_tracked_set_to_ability_chain(ability: &mut ResolvedAbility, real_id: TrackedSetId) {
    bind_tracked_set_to_effect(&mut ability.effect, real_id);
    if let Some(sub_ability) = ability.sub_ability.as_mut() {
        bind_tracked_set_to_ability_chain(sub_ability, real_id);
    }
    if let Some(else_ability) = ability.else_ability.as_mut() {
        bind_tracked_set_to_ability_chain(else_ability, real_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::GameObject;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, BounceSelection, DamageKindFilter, DelayedTriggerCondition,
        Effect, ManaProduction, ObjectScope, PtValue, QuantityExpr, QuantityRef, TriggerDefinition,
    };
    use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
    use crate::types::mana::ManaCost;
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;

    /// Construct a synthetic GameObject with a known mana value and insert
    /// it into state.objects under the given ObjectId. Used by walker tests
    /// that need a stand-in for a countered spell.
    fn inject_spell_with_mana_value(state: &mut GameState, spell_id: ObjectId, mana_value: u32) {
        let mut obj = GameObject::new(
            spell_id,
            CardId(0),
            PlayerId(1),
            "Test Spell".to_string(),
            crate::types::zones::Zone::Graveyard,
        );
        obj.mana_cost = ManaCost::generic(mana_value);
        state.objects.insert(spell_id, obj);
    }

    /// Build an `Effect::Mana { Colorless { count } }` with all fields
    /// of the Mana variant populated. Used by walker tests to construct the
    /// inner effect of a delayed trigger.
    fn mana_colorless_effect(count: QuantityExpr) -> Effect {
        Effect::Mana {
            produced: ManaProduction::Colorless { count },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }

    fn mana_any_one_color_effect(count: QuantityExpr) -> Effect {
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count,
                color_options: crate::types::mana::ManaColor::ALL.to_vec(),
                contribution: Default::default(),
            },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }

    #[test]
    fn creates_delayed_trigger_on_state() {
        let mut state = GameState::new_two_player(42);
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert_eq!(state.delayed_triggers.len(), 1);
        assert!(state.delayed_triggers[0].one_shot);
        assert_eq!(state.delayed_triggers[0].controller, PlayerId(0));
        assert_eq!(state.delayed_triggers[0].source_id, ObjectId(5));
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhase { phase: Phase::End }
        );
    }

    #[test]
    fn parent_target_snapshots_triggering_zone_change_object() {
        let mut state = GameState::new_two_player(42);
        let dead_creature = ObjectId(10);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: dead_creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord::test_minimal(
                dead_creature,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        });

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(dead_creature)]
        );
    }

    /// CR 603.7c: A delayed trigger whose inner effect targets the dying
    /// creature via TriggeringSource (the "it" anaphor — e.g. Grave Betrayal
    /// "return it to the battlefield") must snapshot the ZoneChanged source
    /// object into delayed_ability.targets at creation time.
    ///
    /// Without the fix, delayed_ability.targets = [] and at end-step firing
    /// the zone move silently skips (bugs #2883, #2886).
    #[test]
    fn triggering_source_snapshots_zone_change_object() {
        let mut state = GameState::new_two_player(42);
        let dying_creature = ObjectId(10);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: dying_creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord::test_minimal(
                dying_creature,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        });

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(dying_creature)],
            "TriggeringSource delayed trigger must snapshot the dying creature; \
             if this fails, effect_refs_triggering_source gate is missing"
        );
    }

    /// CR 603.7c: TriggeringSource snapshot must read from the trigger event
    /// even when the parent ability has non-empty targets. This distinguishes
    /// TriggeringSource from ParentTarget, where ability.targets IS the snapshot.
    #[test]
    fn triggering_source_snapshot_ignores_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let dying_creature = ObjectId(10);
        let other_target = ObjectId(20);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: dying_creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord::test_minimal(
                dying_creature,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        });

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(other_target)], // non-empty parent targets
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(dying_creature)],
            "TriggeringSource snapshot must read from ZoneChanged event, not parent's chosen targets"
        );
    }

    /// CR 603.7c: The snapshot gate must inspect the whole delayed ability chain,
    /// not only the first effect, because sub-abilities inherit parent targets at
    /// delayed-trigger resolution.
    #[test]
    fn triggering_source_snapshot_detects_sub_ability_reference() {
        let mut state = GameState::new_two_player(42);
        let dying_creature = ObjectId(10);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: dying_creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord::test_minimal(
                dying_creature,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        });

        let mut effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        effect_def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        )));
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(dying_creature)]
        );
    }

    /// CR 603.7c + CR 400.7: when the delayed trigger snapshots a zone-change
    /// `TriggeringSource`, the stored zone move must also remember the event's
    /// destination as its expected origin. Otherwise an object that leaves that
    /// zone before the delayed trigger fires can be moved anyway.
    #[test]
    fn triggering_source_snapshot_stamps_event_destination_origin() {
        let mut state = GameState::new_two_player(42);
        let dying_creature = ObjectId(10);
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: dying_creature,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord::test_minimal(
                dying_creature,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        });

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TriggeringSource,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.delayed_triggers[0].ability.effect {
            Effect::ChangeZone { origin, .. } => assert_eq!(*origin, Some(Zone::Graveyard)),
            other => panic!("expected ChangeZone, got {other:?}"),
        }
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(dying_creature)]
        );
    }

    #[test]
    fn whenever_event_parent_target_binds_to_specific_source() {
        let mut state = GameState::new_two_player(42);
        let target = ObjectId(10);

        let mut trigger = TriggerDefinition::new(TriggerMode::DamageDone);
        trigger.damage_kind = DamageKindFilter::CombatOnly;
        trigger.valid_source = Some(TargetFilter::ParentTarget);
        trigger.valid_target = Some(TargetFilter::Player);

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: crate::types::ability::QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WheneverEvent {
                    trigger: Box::new(trigger),
                },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(target)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let DelayedTriggerCondition::WheneverEvent { trigger } =
            &state.delayed_triggers[0].condition
        else {
            panic!(
                "expected WheneverEvent, got {:?}",
                state.delayed_triggers[0].condition
            );
        };
        assert_eq!(
            trigger.valid_source,
            Some(TargetFilter::SpecificObject { id: target })
        );
    }

    #[test]
    fn uses_tracked_set_binds_to_change_zone_all() {
        let mut state = GameState::new_two_player(42);
        // Register a tracked set
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10), ObjectId(11)]);
        state.next_tracked_set_id = 2;

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Exile),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
        assert_eq!(state.delayed_triggers.len(), 1);

        // The delayed trigger's effect should reference the tracked set
        match &state.delayed_triggers[0].ability.effect {
            Effect::ChangeZoneAll { target, .. } => {
                assert_eq!(
                    *target,
                    TargetFilter::TrackedSet {
                        id: TrackedSetId(1)
                    }
                );
            }
            other => panic!("Expected ChangeZoneAll, got {:?}", other),
        }
    }

    #[test]
    fn uses_tracked_set_binds_sub_ability_effects() {
        let mut state = GameState::new_two_player(42);
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10)]);
        state.next_tracked_set_id = 2;

        let mut effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        effect_def.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        )));
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let sub = state.delayed_triggers[0]
            .ability
            .sub_ability
            .as_deref()
            .expect("sub-ability chain must be preserved");
        match &sub.effect {
            Effect::ChangeZoneAll { origin, target, .. } => {
                assert_eq!(*origin, Some(Zone::Exile));
                assert_eq!(
                    *target,
                    TargetFilter::TrackedSet {
                        id: TrackedSetId(1)
                    }
                );
            }
            other => panic!("Expected sub ChangeZoneAll, got {:?}", other),
        }
    }

    #[test]
    fn uses_tracked_set_resolves_sentinel() {
        let mut state = GameState::new_two_player(42);
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10)]);
        state.next_tracked_set_id = 2;

        // Parser emits ChangeZone with TrackedSetId(0) sentinel
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: None,
                destination: Zone::Battlefield,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                },
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());

        // Should be upgraded to ChangeZoneAll with resolved TrackedSetId and Exile origin
        match &state.delayed_triggers[0].ability.effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                ..
            } => {
                assert_eq!(*origin, Some(Zone::Exile));
                assert_eq!(*destination, Zone::Battlefield);
                assert_eq!(
                    *target,
                    TargetFilter::TrackedSet {
                        id: TrackedSetId(1)
                    }
                );
            }
            other => panic!("Expected ChangeZoneAll, got {:?}", other),
        }
    }

    #[test]
    fn uses_tracked_set_binds_zone_change_condition_filter() {
        let mut state = GameState::new_two_player(42);
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10)]);
        state.next_tracked_set_id = 2;

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::TriggeringSource,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenEntersBattlefield {
                    filter: TargetFilter::ParentTarget,
                },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![],
            "no current_trigger_event means TriggeringSource snapshot is empty"
        );
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::WhenEntersBattlefield {
                filter: TargetFilter::TrackedSet {
                    id: TrackedSetId(1)
                },
            },
            "tracked-set delayed trigger conditions must match only the captured objects"
        );
    }

    /// CR 505.1 + CR 603.7a: `AtNextPhaseForPlayer` player field is emitted
    /// by the parser as a `PlayerId(0)` placeholder (compile-time AST has no
    /// access to runtime player ids). `resolve()` rewrites it to
    /// `ability.controller` so the delayed trigger fires on the correct
    /// player's turn. Used by Mana Sculpt.
    #[test]
    fn at_next_phase_for_player_rebinds_placeholder_to_controller() {
        let mut state = GameState::new_two_player(42);
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        // Cast by PlayerId(1), with the placeholder PlayerId(0) in the
        // condition. Resolver must rewrite to PlayerId(1).
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::PreCombatMain,
                player: PlayerId(1),
            },
            "placeholder player must be rewritten to ability.controller"
        );
    }

    #[test]
    fn delayed_parent_target_snapshots_parent_targets() {
        let mut state = GameState::new_two_player(42);
        let vehicle_id = ObjectId(10);
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
                selection: BounceSelection::Targeted,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: PlayerId(0),
                },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(vehicle_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(vehicle_id)],
            "delayed ParentTarget effects must remember the object from the parent resolution"
        );
    }

    /// CR 603.7 + CR 106.3 + CR 608.2h: A delayed trigger whose inner
    /// effect references `ManaSpentToCast{TriggeringSpell, Total}` (the
    /// parser-emitted anaphor for "the amount of mana spent to cast that
    /// spell" — used by Mana Sculpt) must have that leaf snapshotted to a
    /// `Fixed` value at creation time. The snapshot reads
    /// `state.objects[parent.targets[0]].mana_spent_to_cast_amount` via
    /// `resolve_mana_spent_to_cast_metric`, bypassing the standard
    /// TriggeringSpell resolver chain (which keys off
    /// state.current_trigger_event — wrong context at firing time, and
    /// also unset during Mana Sculpt's spell-card resolution).
    #[test]
    fn snapshot_mana_spent_to_cast_triggering_spell_baked_to_fixed() {
        use crate::types::ability::{CastManaObjectScope, CastManaSpentMetric};

        let mut state = GameState::new_two_player(42);
        let spell_id = ObjectId(42);
        // Reuse the fixture from Task 4 to create a spell GameObject, then
        // override mana_spent_to_cast_amount specifically (mana_cost can be
        // anything since this test exercises the ManaSpentToCast path, not
        // ObjectManaValue).
        inject_spell_with_mana_value(&mut state, spell_id, 0);
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .mana_spent_to_cast_amount = 5;

        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            mana_colorless_effect(QuantityExpr::Ref {
                qty: QuantityRef::ManaSpentToCast {
                    scope: CastManaObjectScope::TriggeringSpell,
                    metric: CastManaSpentMetric::Total,
                },
            }),
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 5 },
                    "ManaSpentToCast{{TriggeringSpell}} must snapshot to Fixed{{5}}"
                );
            }
            other => panic!("expected Mana{{Colorless}}, got {other:?}"),
        }
    }

    #[test]
    fn sub_ability_parent_dependent_quantity_baked_to_fixed() {
        let mut state = GameState::new_two_player(42);
        let spell_id = ObjectId(42);
        inject_spell_with_mana_value(&mut state, spell_id, 6);

        let mut delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        delayed_inner.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            mana_colorless_effect(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Target,
                },
            }),
        )));
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let sub = state.delayed_triggers[0]
            .ability
            .sub_ability
            .as_deref()
            .expect("sub-ability chain must be preserved");
        let Effect::Mana {
            produced: ManaProduction::Colorless { count },
            ..
        } = &sub.effect
        else {
            panic!("Expected sub Mana effect, got {:?}", sub.effect);
        };
        assert_eq!(
            *count,
            QuantityExpr::Fixed { value: 6 },
            "parent-dependent sub-chain quantities must be snapshotted before the delayed trigger fires"
        );
    }

    /// CR 603.7 + CR 202.3: A delayed trigger whose inner effect references
    /// `ObjectManaValue { CostPaidObject }` (the parser-emitted anaphor for
    /// "that spell's mana value") must have that leaf snapshotted to a
    /// `Fixed` value at creation time. The snapshot reads the parent
    /// ability's targets[0] mana value directly, bypassing the standard
    /// CostPaidObject resolver chain (which is wrong for spell-card
    /// contexts where `cost_paid_object` is unset).
    #[test]
    fn snapshot_object_mana_value_cost_paid_object_baked_to_fixed() {
        let mut state = GameState::new_two_player(42);
        let spell_id = ObjectId(42);
        inject_spell_with_mana_value(&mut state, spell_id, 3);

        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            mana_colorless_effect(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        // After resolve, the delayed trigger's effect must have its
        // ObjectManaValue{CostPaidObject} leaf rewritten to Fixed{3}.
        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 3 },
                    "delayed trigger's mana count must be snapshotted to Fixed{{3}}"
                );
            }
            other => panic!("expected Mana{{Colorless}}, got {other:?}"),
        }
    }

    /// CR 603.7 + CR 608.2h: The snapshot walker must cover every
    /// quantity-bearing mana-production sibling, including "one color" mana.
    #[test]
    fn snapshot_any_one_color_count_baked_to_fixed() {
        let mut state = GameState::new_two_player(42);
        let spell_id = ObjectId(42);
        inject_spell_with_mana_value(&mut state, spell_id, 4);

        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            mana_any_one_color_effect(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Mana {
                produced: ManaProduction::AnyOneColor { count, .. },
                ..
            } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 4 },
                    "AnyOneColor count must be snapshotted to Fixed{{4}}"
                );
            }
            other => panic!("expected Mana{{AnyOneColor}}, got {other:?}"),
        }
    }

    /// CR 603.7 + CR 608.2h: Pump effects carry dynamic quantities inside
    /// `PtValue::Quantity`, not directly as `QuantityExpr`, so they need their
    /// own walker branch.
    #[test]
    fn snapshot_pump_pt_quantity_baked_to_fixed() {
        let mut state = GameState::new_two_player(42);
        let spell_id = ObjectId(42);
        inject_spell_with_mana_value(&mut state, spell_id, 6);

        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject,
                    },
                }),
                toughness: PtValue::Quantity(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::Target,
                    },
                }),
                target: TargetFilter::SelfRef,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Pump {
                power, toughness, ..
            } => {
                assert_eq!(*power, PtValue::Quantity(QuantityExpr::Fixed { value: 6 }));
                assert_eq!(
                    *toughness,
                    PtValue::Quantity(QuantityExpr::Fixed { value: 6 })
                );
            }
            other => panic!("expected Pump, got {other:?}"),
        }
    }

    /// CR 603.7 (defensive): If the parent ability has no Object targets,
    /// the walker leaves the QuantityRef unmodified. At fire time the ref
    /// evaluates against empty targets and returns 0 — same fail-closed
    /// behavior as before the walker existed.
    #[test]
    fn snapshot_no_parent_targets_leaves_ref_intact() {
        let mut state = GameState::new_two_player(42);
        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            mana_colorless_effect(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![], // empty targets
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectManaValue { .. }
                        }
                    ),
                    "empty parent targets must leave the ref unmodified, got {count:?}"
                );
            }
            other => panic!("expected Mana{{Colorless}}, got {other:?}"),
        }
    }

    /// CR 603.7 (defensive): If the target ObjectId exists in parent.targets
    /// but `state.objects` does NOT contain that id (the spell already left
    /// the game through a weirder replacement), snapshot to Fixed{0} via
    /// the LKI-or-zero fallback chain.
    #[test]
    fn snapshot_target_missing_from_objects_baked_to_zero() {
        let mut state = GameState::new_two_player(42);
        // Do NOT insert an object for spell_id — simulate a missing target.
        let spell_id = ObjectId(999);

        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            mana_colorless_effect(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::CostPaidObject,
                },
            }),
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Fixed { value: 0 },
                    "missing object must snapshot to Fixed{{0}}"
                );
            }
            other => panic!("expected Mana{{Colorless}}, got {other:?}"),
        }
    }

    /// CR 603.7: Non-snapshottable QuantityRef leaves (Source-scoped,
    /// Controller, Variable, aggregate refs, etc.) pass through the walker
    /// unmodified. They evaluate against live game state at fire time,
    /// which is the correct semantic.
    #[test]
    fn snapshot_non_snapshottable_ref_passes_through() {
        let mut state = GameState::new_two_player(42);
        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            // Source-scoped — refers to the ability source, which persists
            // at fire time. Walker must NOT snapshot.
            mana_colorless_effect(QuantityExpr::Ref {
                qty: QuantityRef::ObjectManaValue {
                    scope: ObjectScope::Source,
                },
            }),
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(ObjectId(42))],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => {
                assert!(
                    matches!(
                        count,
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectManaValue {
                                scope: ObjectScope::Source
                            }
                        }
                    ),
                    "Source-scoped ref must pass through unmodified, got {count:?}"
                );
            }
            other => panic!("expected Mana{{Colorless}}, got {other:?}"),
        }
    }

    /// CR 603.7: Compound QuantityExpr variants (Offset, Multiply, Sum,
    /// etc.) must recurse — the walker snapshots any snapshottable leaves
    /// nested inside. Verifies an Offset(ObjectManaValue{CostPaidObject},
    /// +1) rewrites to Offset(Fixed{N}, +1), not full collapse.
    #[test]
    fn snapshot_compound_expr_recurses() {
        let mut state = GameState::new_two_player(42);
        let spell_id = ObjectId(42);
        inject_spell_with_mana_value(&mut state, spell_id, 2);

        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            mana_colorless_effect(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectManaValue {
                        scope: ObjectScope::CostPaidObject,
                    },
                }),
                offset: 1,
            }),
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::PreCombatMain,
                    player: PlayerId(0),
                },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(spell_id)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::Mana {
                produced: ManaProduction::Colorless { count },
                ..
            } => {
                assert_eq!(
                    *count,
                    QuantityExpr::Offset {
                        inner: Box::new(QuantityExpr::Fixed { value: 2 }),
                        offset: 1,
                    },
                    "compound Offset must recurse: inner snapshotted to Fixed{{2}}, outer Offset{{+1}} preserved"
                );
            }
            other => panic!("expected Mana{{Colorless}}, got {other:?}"),
        }
    }

    /// Issue #528: Nine-Lives Familiar — snapshot_parent_dependent_quantities must
    /// freeze CountersOn { Source } inside ChangeZone.enter_with_counters to a Fixed
    /// value from LKI at delayed trigger creation time (before step transition clears
    /// the LKI cache).
    #[test]
    fn snapshot_counters_on_source_in_change_zone_enter_with_counters() {
        use crate::types::game_state::LKISnapshot;
        use std::collections::HashMap;

        let mut state = GameState::new_two_player(42);
        let source_id = ObjectId(7); // Nine-Lives Familiar that just died

        // Populate LKI cache as if the source died with 5 revival counters
        let mut lki_counters = HashMap::new();
        lki_counters.insert(CounterType::Generic("revival".to_string()), 5);
        state.lki_cache.insert(
            source_id,
            LKISnapshot {
                name: "Nine-Lives Familiar".to_string(),
                power: Some(3),
                toughness: Some(3),
                base_power: Some(3),
                base_toughness: Some(3),
                mana_value: 4,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![],
                subtypes: vec![],
                supertypes: vec![],
                keywords: vec![],
                colors: vec![],
                chosen_attributes: Vec::new(),
                counters: lki_counters,
            },
        );

        // Set up the trigger event (dies = zone change to graveyard)
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: source_id,
            from: Some(Zone::Battlefield),
            to: Zone::Graveyard,
            record: Box::new(crate::types::game_state::ZoneChangeRecord::test_minimal(
                source_id,
                Some(Zone::Battlefield),
                Zone::Graveyard,
            )),
        });

        // Build the delayed trigger inner effect: ChangeZone with enter_with_counters
        // containing ClampMin { Offset { CountersOn { Source, revival }, -1 }, 0 }
        let revival_type = CounterType::Generic("revival".to_string());
        let counter_qty = QuantityExpr::ClampMin {
            inner: Box::new(QuantityExpr::Offset {
                inner: Box::new(QuantityExpr::Ref {
                    qty: QuantityRef::CountersOn {
                        scope: ObjectScope::Source,
                        counter_type: Some(revival_type.clone()),
                    },
                }),
                offset: -1,
            }),
            minimum: 0,
        };

        let delayed_inner = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Graveyard),
                destination: Zone::Battlefield,
                target: TargetFilter::ParentTarget,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![(revival_type.clone(), counter_qty)],
                face_down_profile: None,
            },
        );

        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(delayed_inner),
                uses_tracked_set: false,
            },
            vec![],
            source_id, // source_id = the dying creature
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        // Verify the delayed trigger's enter_with_counters was snapshotted:
        // CountersOn(Source) resolved to Fixed(5) from LKI. The outer Offset and
        // ClampMin wrappers are preserved (snapshot only freezes Ref leaves).
        let delayed = &state.delayed_triggers[0];
        match &delayed.ability.effect {
            Effect::ChangeZone {
                enter_with_counters,
                ..
            } => {
                assert_eq!(enter_with_counters.len(), 1);
                let (ct, qty) = &enter_with_counters[0];
                assert_eq!(*ct, revival_type);
                assert_eq!(
                    *qty,
                    QuantityExpr::ClampMin {
                        inner: Box::new(QuantityExpr::Offset {
                            inner: Box::new(QuantityExpr::Fixed { value: 5 }),
                            offset: -1,
                        }),
                        minimum: 0,
                    },
                    "CountersOn(Source) with 5 revival counters in LKI must snapshot to \
                     ClampMin {{ Offset {{ Fixed(5), -1 }}, 0 }}"
                );
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }
}
