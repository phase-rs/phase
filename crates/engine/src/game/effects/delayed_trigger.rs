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

    // CR 603.7 + CR 608.2c: Resolve the most-recent tracked set once, up front,
    // so the tracked-set CONDITION rewrite runs BEFORE the single-target
    // contextual bind below. Genuine "those cards" tracked-set forms (Ugin the
    // Ineffable, Lagrella, Mechtitan Core — WhenLeavesPlayFiltered /
    // WhenEntersBattlefield) rewrite `ParentTarget` → `TrackedSet` first; the
    // contextual bind then sees `TrackedSet` and passes it through untouched.
    // Single-target "that creature" cards (Scarblade's Malice class) register no
    // tracked set, so `latest_tracked_set_id` is `None`, the tracked-set rewrite
    // is skipped, and the contextual bind rewrites `ParentTarget` → the concrete
    // chosen object. This ordering is mandatory: running the contextual bind
    // first would pre-empt the tracked-set rewrite and break the "those cards"
    // cards.
    let tracked_set_id = if uses_tracked_set {
        crate::game::targeting::latest_tracked_set_id(state)
    } else {
        None
    };
    if let Some(real_id) = tracked_set_id {
        bind_tracked_set_to_condition(&mut condition, real_id);
    }

    bind_contextual_filter_to_condition(&mut condition, &ability.targets);

    // CR 505.1 + CR 603.7a: "your next <phase>" binds the trigger to the
    // ability's controller. The parser emits a placeholder `PlayerId(0)` in
    // `AtNextPhaseForPlayer.player` because compile-time AST has no access to
    // runtime player ids; rewrite here to the actual controller at resolve
    // time. Mirrors the `bind_contextual_filter_to_condition` pattern above.
    if let DelayedTriggerCondition::AtNextPhaseForPlayer { player, gate, .. } = &mut condition {
        *player = ability.controller;
        // CR 513.2 + CR 603.7a: the "on your next turn" floor only becomes
        // concrete at creation. Stamp the symbolic parse-time gate to the actual
        // creation turn so the matcher skips the current turn's matching phase.
        if matches!(gate, crate::types::ability::TurnGate::AfterCreationTurn) {
            *gate = crate::types::ability::TurnGate::After(state.turn_number);
        }
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

    // CR 603.7: Bind the most recent tracked set to the built ability chain's
    // effect target filter, resolving sentinel TrackedSetId(0) or
    // TargetFilter::Any, and upgrading ChangeZone → ChangeZoneAll for delayed
    // triggers (which have empty explicit targets). Reuses `tracked_set_id`
    // resolved above; the condition rewrite ran there so it precedes the
    // single-target contextual bind. This operates on the built `delayed_ability`
    // (not the condition), so it must stay after the ability chain is built.
    if let Some(real_id) = tracked_set_id {
        bind_tracked_set_to_ability_chain(&mut delayed_ability, real_id);
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
    // A delayed trigger is a continuation of this resolved ability, so preserve
    // the same exact trigger source across its later match and resolution. Spell
    // and activated-ability sources may not already carry trigger provenance;
    // capture their current incarnation at creation rather than later rebinding
    // the stored ObjectId. CR 400.7.
    let source_context = ability.trigger_source.clone().or_else(|| {
        state
            .objects
            .get(&ability.source_id)
            .map(|source| super::super::triggers::trigger_source_context_for_latch(state, source))
    });
    if let Some(source_context) = source_context {
        delayed_ability.set_trigger_source_recursive(source_context);
    }

    // CR 701.27f: A delayed triggered ability may transform its source only if
    // that permanent has not transformed or converted since the delayed
    // ability was created. Capture the generation here, not when it fires.
    let source = state
        .objects
        .get(&ability.source_id)
        .filter(|object| object.back_face.is_some());
    let source_transformation_count = source.map(|object| object.transformation_count);
    delayed_ability.set_source_transformation_count_recursive(source_transformation_count);
    // CR 400.7: bind the delayed self-transform to the source's creation-time
    // incarnation; a later re-entry must not be restamped when the trigger fires.
    delayed_ability.set_source_incarnation_recursive(source.map(|object| object.incarnation));

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
        subject: None,
    });

    Ok(())
}

/// CR 603.7c + CR 608.2c: A delayed triggered ability that refers to a
/// particular object snapshots that object at creation time. The snapshot is
/// seeded from the FLATTENED ROOT chain (`parent_chain_targets_from_root`), not
/// the current node's per-clause `targets`: for a multi-clause parent chain the
/// tail clause carries only its own local slot, so an inner delayed
/// `ParentTargetSlot { index }` anaphor pointing at an earlier slot would index
/// out of range and degrade to `Any`. Flattening the root chain exposes every
/// declared slot in order so the indexed anaphor resolves.
///
/// CR 608.2c (phase#4767): When the root chain exposes NO concrete slot — because
/// the parent target was injected at runtime by a `forward_result` zone-change
/// rather than declared as an explicit chain slot (Animate Dead / Dance of the
/// Dead: the reanimated creature is the moved object, bound into the sub-chain's
/// `targets` by `effects/mod.rs`'s forward_result block, never a declared slot) —
/// the node's OWN propagated `targets` are the resolved parent target. Prefer them
/// over the triggering-source fallback, which would otherwise snapshot the
/// triggering object (the Aura) instead of "that creature". Only when BOTH the
/// root chain and the node's own targets are empty do we fall back to the
/// triggering source (unchanged).
fn parent_target_snapshot(state: &GameState, ability: &ResolvedAbility) -> Vec<TargetRef> {
    let root_chain = crate::game::targeting::parent_chain_targets_from_root(state, ability);
    if !root_chain.is_empty() {
        return root_chain;
    }

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
        // CR 603.7c + CR 608.2k: A delayed triggered ability that refers to
        // "that creature/permanent" binds the single chosen object into the
        // condition filter. Runs AFTER the tracked-set condition rewrite, so
        // genuine "those cards" tracked-set forms (already `TrackedSet`) pass
        // through untouched; only an unbound `ParentTarget` (single-target
        // class, no tracked set) binds to the concrete object. Covers the whole
        // zone-change condition family so "that creature dies / leaves play /
        // enters" back-references all resolve identically.
        DelayedTriggerCondition::WhenDies { filter }
        | DelayedTriggerCondition::WhenLeavesPlayFiltered { filter }
        | DelayedTriggerCondition::WhenEntersBattlefield { filter }
        | DelayedTriggerCondition::WhenDiesOrExiled { filter } => {
            bind_parent_target_filter(filter, parent_targets);
        }
        DelayedTriggerCondition::WheneverEvent { trigger } => {
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
        DelayedTriggerCondition::WhenNextEvent {
            trigger,
            or_trigger,
            ..
        } => {
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
            if let Some(alt) = or_trigger {
                for filter in [
                    &mut alt.valid_card,
                    &mut alt.valid_source,
                    &mut alt.valid_target,
                ]
                .into_iter()
                .flatten()
                {
                    bind_parent_target_filter(filter, parent_targets);
                }
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
        // CR 603.7c + CR 608.2c: bind a `ParentTargetSlot { index }` delayed
        // condition filter to the concrete parent object at that declared slot
        // (single-slot analogue of the `ParentTarget` arm). Out-of-range/empty
        // slots fall back to `Any`, matching `parent_targets_filter`'s empty case.
        TargetFilter::ParentTargetSlot { index } => parent_targets
            .get(index)
            .map(|target| match target {
                TargetRef::Object(id) => TargetFilter::SpecificObject { id: *id },
                TargetRef::Player(id) => TargetFilter::SpecificPlayer { id: *id },
            })
            .unwrap_or(TargetFilter::Any),
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
                | ManaProduction::AnyCombinationOfObjectColors { count, .. }
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
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
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
    // CR 603.7c + CR 603.12 + CR 202.3e: A reflexive/delayed trigger that
    // references "that spell's mana value" (`ObjectManaValue` with the
    // demonstrative/anaphoric referent — Breeches, the Blastmaker's
    // "deals damage equal to that spell's mana value") carries no parent object
    // target: the spell lives in the creation-time trigger event (a `SpellCast`
    // whose source is the cast spell). Snapshot it from that event context now,
    // before the `ability.targets[0]` extraction below (which would early-return
    // `None` and leave the ref to evaluate to 0 at fire time, where
    // `current_trigger_event` is the later `CoinFlipped`). Falls through to the
    // target-based path when targets are present.
    if matches!(
        qty,
        QuantityRef::ObjectManaValue {
            scope: ObjectScope::Demonstrative | ObjectScope::Anaphoric,
        }
    ) && ability.targets.is_empty()
    {
        if let Some(spell_id) = state
            .current_trigger_event
            .as_ref()
            .and_then(crate::game::targeting::extract_source_from_event)
        {
            // CR 202.3d + CR 202.3e + CR 702.102b: snapshot "that spell's mana value"
            // through the split-aware authority — a FUSED split spell freezes its
            // COMBINED mana value (both halves), and every other spell freezes its own
            // cost with the chosen X (`spell_mana_value`'s non-fused arm is the same
            // `mana_value_with_x(zone, cost_x_paid)` read).
            return state
                .objects
                .get(&spell_id)
                .map(|obj| obj.spell_mana_value() as i32);
        }
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
                // CR 202.3d + CR 709.4b: the target object may be in a non-stack
                // zone (a targeted card in a graveyard), where a split card's mana
                // value is its combined halves; CR 202.3e: chosen X on the stack.
                .map(|obj| obj.effective_mana_value() as i32)
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
/// 3. Preserve the parsed `origin` (Battlefield for token cleanup, Exile for
///    cross-clause exiled-card references). When unset, `change_zone::resolve_all`
///    derives scan zones from tracked-set members at firing time.
fn bind_tracked_set_to_effect(effect: &mut Effect, real_id: TrackedSetId) {
    match effect {
        Effect::ChangeZoneAll {
            origin: _, target, ..
        } => {
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
        }
        // CR 603.7c + CR 608.2c: Pin the tracked-set sentinel `TrackedSetId(0)` to
        // the concrete `real_id` inside the mass-destroy target filter at
        // delayed-trigger CREATION, so end-step resolution reads THIS ability's
        // frozen population and never falls back to `matches_target_filter`'s live
        // `max_by_key` scan (which would pick a later, unrelated tracked set — the
        // Maddening Imp cross-resolution collision). Reuses the existing
        // `TargetFilter::rebind_tracked_set_sentinel` (types/ability.rs) — the
        // single authority for rewriting `TrackedSet{0}`/`TrackedSetFiltered{0}` →
        // concrete inside a filter (recursing And/Or/Not) — rather than open-coding
        // the two-variant rewrite the `ChangeZoneAll` arm above does inline.
        Effect::DestroyAll { target, .. } => target.rebind_tracked_set_sentinel(real_id),
        // Upgrade ChangeZone → ChangeZoneAll: ChangeZone uses ability.targets (empty for
        // delayed triggers), so it would move nothing. ChangeZoneAll scans by filter.
        Effect::ChangeZone {
            destination,
            origin,
            target,
            enters_under,
            enter_tapped,
            enter_with_counters,
            face_down_profile,
            ..
        } => {
            let bound_target = match target {
                TargetFilter::TrackedSet {
                    id: TrackedSetId(0),
                }
                | TargetFilter::Any => TargetFilter::TrackedSet { id: real_id },
                TargetFilter::TrackedSet { id } => TargetFilter::TrackedSet { id: *id },
                _ => TargetFilter::TrackedSet { id: real_id },
            };
            *effect = Effect::ChangeZoneAll {
                origin: *origin,
                destination: *destination,
                target: bound_target,
                enters_under: enters_under.clone(),
                enter_tapped: *enter_tapped,
                enter_with_counters: enter_with_counters.clone(),
                face_down_profile: face_down_profile.clone(),
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
    use crate::types::triggers::{PlaneswalkRole, TriggerMode};

    /// T5 (s25 site 1) — CR 603.7c + CR 608.2c: `concrete_parent_target_filter`
    /// binds a `ParentTargetSlot { index }` delayed-condition filter to the
    /// concrete parent object at that one declared slot (not the first). Pre-fix
    /// the `other => other` fall-through returned the abstract `ParentTargetSlot`
    /// unchanged (index dropped), so binding never happened — reverting the arm
    /// flips these assertions from `SpecificObject` back to `ParentTargetSlot`.
    #[test]
    fn concrete_parent_target_filter_binds_parent_target_slot_to_that_slot() {
        let parents = [
            TargetRef::Object(ObjectId(7)),
            TargetRef::Object(ObjectId(8)),
        ];
        assert_eq!(
            concrete_parent_target_filter(&TargetFilter::ParentTargetSlot { index: 1 }, &parents),
            TargetFilter::SpecificObject { id: ObjectId(8) },
        );
        assert_eq!(
            concrete_parent_target_filter(&TargetFilter::ParentTargetSlot { index: 0 }, &parents),
            TargetFilter::SpecificObject { id: ObjectId(7) },
        );
        // Out-of-range slot falls back to `Any`, matching the empty-slice case.
        assert_eq!(
            concrete_parent_target_filter(&TargetFilter::ParentTargetSlot { index: 5 }, &parents),
            TargetFilter::Any,
        );
    }

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

    /// CR 603.7c + CR 608.2c: the `parent_target_snapshot` path freezes a
    /// MULTI-target parent selection into the delayed ability at creation, exactly
    /// as it does for The Pandorica's single target. This is the building-block
    /// proof that The Doctor's Childhood Barn's per-opponent "choose up to one
    /// target nonland permanent that opponent controls … those permanents phase
    /// in" delayed trigger captures every chosen permanent (not just the first).
    /// The intervening player ref is harmlessly carried and later filtered out by
    /// `collect_phase_in_targets` at fire time.
    #[test]
    fn parent_target_snapshot_freezes_all_multi_targets_for_delayed_phase_in() {
        let mut state = GameState::new_two_player(42);
        let obj_a = ObjectId(10);
        let obj_b = ObjectId(11);

        let inner = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::PhaseIn {
                target: TargetFilter::ParentTarget,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(TriggerDefinition::new(TriggerMode::Planeswalked {
                        role: PlaneswalkRole::Any,
                    })),
                    or_trigger: None,
                    lifetime: crate::types::ability::DelayedTriggerLifetime::Persistent,
                },
                effect: Box::new(inner),
                uses_tracked_set: false,
            },
            vec![
                TargetRef::Object(obj_a),
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(obj_b),
            ],
            ObjectId(5),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.delayed_triggers.len(), 1);
        let snapshot = &state.delayed_triggers[0].ability.targets;
        assert!(
            snapshot.contains(&TargetRef::Object(obj_a)),
            "first chosen permanent must be snapshotted, got {snapshot:?}"
        );
        assert!(
            snapshot.contains(&TargetRef::Object(obj_b)),
            "second chosen permanent must ALSO be snapshotted (multi-target), got {snapshot:?}"
        );
        // Persistent lifetime survives across turns until the planeswalk fires.
        assert!(state.delayed_triggers[0].one_shot);
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
                assert_eq!(*origin, None);
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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

        // Should be upgraded to ChangeZoneAll with resolved TrackedSetId; origin
        // stays unset so runtime derives member zones when firing.
        match &state.delayed_triggers[0].ability.effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                ..
            } => {
                assert_eq!(*origin, None);
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

    /// CR 603.7c + CR 608.2k (issue #762): a single-target "when that creature
    /// dies" delayed trigger — no tracked set registered — must bind its
    /// `WhenDies { ParentTarget }` condition filter to the parent's chosen
    /// object. This is the unit-level proof of the Scarblade's Malice fix: with
    /// `uses_tracked_set: true` but no tracked set present, the tracked-set
    /// rewrite is skipped and the contextual bind rewrites
    /// `ParentTarget` → `SpecificObject { victim }`.
    #[test]
    fn when_dies_parent_target_binds_to_specific_victim_without_tracked_set() {
        let mut state = GameState::new_two_player(42);
        let victim = ObjectId(10);

        // Mirror the real card: uses_tracked_set is true, but NO tracked set is
        // registered, so latest_tracked_set_id is None.
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenDies {
                    filter: TargetFilter::ParentTarget,
                },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![TargetRef::Object(victim)],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::WhenDies {
                filter: TargetFilter::SpecificObject { id: victim },
            },
            "single-target WhenDies must bind ParentTarget to the chosen victim, \
             not leave it unbound (0 tokens on Scarblade's Malice)"
        );
    }

    /// CR 603.7 + CR 608.2c: reorder non-regression — a genuine "those cards"
    /// tracked-set `WhenLeavesPlayFiltered { ParentTarget }` (Ugin the Ineffable
    /// / Lagrella class) must rewrite to `TrackedSet` FIRST, then pass through
    /// the single-target contextual bind untouched. If the reorder were wrong,
    /// the contextual bind would pre-empt it and bind to `SpecificObject`,
    /// breaking those cards.
    #[test]
    fn tracked_set_leaves_play_condition_survives_contextual_bind() {
        let mut state = GameState::new_two_player(42);
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10)]);
        state.next_tracked_set_id = 2;

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        // Non-empty parent targets — if the contextual bind ran first it would
        // rewrite ParentTarget to SpecificObject(99) and clobber the tracked set.
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenLeavesPlayFiltered {
                    filter: TargetFilter::ParentTarget,
                },
                effect: Box::new(effect_def),
                uses_tracked_set: true,
            },
            vec![TargetRef::Object(ObjectId(99))],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::WhenLeavesPlayFiltered {
                filter: TargetFilter::TrackedSet {
                    id: TrackedSetId(1)
                },
            },
            "tracked-set condition rewrite must run BEFORE the single-target \
             contextual bind, so ParentTarget → TrackedSet passes through untouched"
        );
    }

    /// CR 603.7c: a `WhenLeavesPlayFiltered { SelfRef }` (animate-dead class)
    /// must resolve with its filter UNCHANGED — `SelfRef` is neither
    /// `ParentTarget` nor a tracked set, so it flows through
    /// `concrete_parent_target_filter`'s `other => other` arm untouched.
    #[test]
    fn self_ref_leaves_play_condition_passes_through_unchanged() {
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
                condition: DelayedTriggerCondition::WhenLeavesPlayFiltered {
                    filter: TargetFilter::SelfRef,
                },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(ObjectId(7))],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::WhenLeavesPlayFiltered {
                filter: TargetFilter::SelfRef,
            },
            "SelfRef condition filters must pass through the contextual bind unchanged"
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
                    gate: crate::types::ability::TurnGate::None,
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
                gate: crate::types::ability::TurnGate::None,
            },
            "placeholder player must be rewritten to ability.controller"
        );
    }

    /// CR 513.2 + CR 603.7a: the parser's symbolic `TurnGate::AfterCreationTurn`
    /// (Kav Landseeker "the end step on your next turn") must be stamped to
    /// `TurnGate::After(creation_turn)` at resolve time, so the runtime matcher
    /// skips the current turn's end step. Revert-to-red: drop the stamp in
    /// `resolve()` and the stored gate stays `AfterCreationTurn` (which the
    /// matcher `debug_assert!`s against — a wrong-timing bug).
    #[test]
    fn after_creation_turn_gate_stamped_to_concrete_floor() {
        use crate::types::ability::TurnGate;
        let mut state = GameState::new_two_player(42);
        state.turn_number = 5;
        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: PlayerId(0),
                    gate: TurnGate::AfterCreationTurn,
                },
                effect: Box::new(effect_def),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].condition,
            DelayedTriggerCondition::AtNextPhaseForPlayer {
                phase: Phase::End,
                player: PlayerId(0),
                gate: TurnGate::After(5),
            },
            "AfterCreationTurn must be stamped to After(state.turn_number)"
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
                    gate: crate::types::ability::TurnGate::None,
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

    /// CR 603.7c + CR 608.2c: For a MULTI-CLAUSE parent chain, the snapshot must
    /// seed from the flattened ROOT chain, not the tail clause's local `targets`.
    /// The tail clause here carries only slot 0 (`slot0`); slot 1 (`slot1`) lives
    /// on the parent's `sub_ability`. The inner delayed effect references
    /// `ParentTargetSlot { index: 1 }`, which is only reachable via the root
    /// flatten. `flatten_targets_in_chain` walks `sub_ability`, producing
    /// `[slot0, slot1]`.
    ///
    /// Non-vacuity / discrimination: with the old `ability.targets` early-return
    /// the snapshot is `[slot0]` and this assertion FAILS (slot1 absent, the
    /// index-1 anaphor would index out of range). Reverting the fn to that form
    /// makes this test panic — proven by the driver's revert run.
    #[test]
    fn delayed_parent_slot_snapshots_full_root_chain() {
        let mut state = GameState::new_two_player(42);
        let slot0 = ObjectId(10);
        let slot1 = ObjectId(11);

        // Inner delayed effect points at the SECOND declared slot — only present
        // in the flattened root chain, never in the tail clause's local targets.
        let inner_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::ParentTargetSlot { index: 1 },
                destination: None,
                selection: BounceSelection::Targeted,
            },
        );

        // Tail clause (the CreateDelayedTrigger node) carries only slot0 locally.
        let mut ability = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhaseForPlayer {
                    phase: Phase::End,
                    player: PlayerId(0),
                    gate: crate::types::ability::TurnGate::None,
                },
                effect: Box::new(inner_def),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(slot0)],
            ObjectId(5),
            PlayerId(0),
        );
        // Earlier chain clause holding slot1; flatten_targets_in_chain walks it.
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
                selection: BounceSelection::Targeted,
            },
            vec![TargetRef::Object(slot1)],
            ObjectId(5),
            PlayerId(0),
        )));

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");
        assert_eq!(state.delayed_triggers.len(), 1);
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![TargetRef::Object(slot0), TargetRef::Object(slot1)],
            "delayed ParentTargetSlot snapshot must carry the FULL flattened root \
             chain so index-1 anaphors resolve, not just the tail clause's slot"
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
                    gate: crate::types::ability::TurnGate::None,
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

    /// CR 202.3d + CR 702.102b: a delayed/reflexive "that spell's mana value"
    /// (`ObjectManaValue { Demonstrative }`, no parent target) snapshots from the
    /// `SpellCast` trigger-event context. For a FUSED split spell the frozen value
    /// must be the COMBINED mana value of both halves (Breaking // Entering: front
    /// {U}{B} = 2, back {4}{B}{R} = 6 → 8), not the front half. Reverting the
    /// snapshot to `mana_cost.mana_value_with_x(...)` freezes 2 and this flips.
    #[test]
    fn snapshot_that_spells_mana_value_uses_combined_for_fused_split_spell() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::game::scenario_db::GameScenarioDbExt;

        let db = crate::test_support::shared_card_db();
        let mut sc = GameScenario::new();
        let spell = sc.add_real_card(P0, "Breaking", Zone::Stack, db);
        sc.state.objects.get_mut(&spell).unwrap().fused_split_spell = true;
        let card_id = sc.state.objects[&spell].card_id;
        let mut state = sc.state;
        // "that spell's mana value" resolves from the SpellCast event context.
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id,
            controller: PlayerId(0),
            object_id: spell,
        });

        // Demonstrative "that spell" ref with NO parent target -> event-context path.
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );
        let value = snapshot_quantity_ref(
            &QuantityRef::ObjectManaValue {
                scope: ObjectScope::Demonstrative,
            },
            &state,
            &ability,
        );
        assert_eq!(
            value,
            Some(8),
            "'that spell's mana value' for a fused Breaking // Entering freezes the \
             COMBINED MV 8, not the front half (2)"
        );
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
                    gate: crate::types::ability::TurnGate::None,
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
                    gate: crate::types::ability::TurnGate::None,
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
                    gate: crate::types::ability::TurnGate::None,
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
                    gate: crate::types::ability::TurnGate::None,
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
                    gate: crate::types::ability::TurnGate::None,
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
                    gate: crate::types::ability::TurnGate::None,
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
                    gate: crate::types::ability::TurnGate::None,
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
                    gate: crate::types::ability::TurnGate::None,
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
                token_image_ref: None,
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
                tapped: false,
                is_suspected: false,
                attachments: Vec::new(),
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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

    /// Cluster J3 (delayed-trigger provenance lock-in): Saheeli's "Sacrifice it
    /// at the beginning of the next end step" must bind the specific token
    /// created THIS resolution, not "whatever token was created most recently"
    /// at firing time. The token id is SNAPSHOTTED from `last_created_token_ids`
    /// into `delayed_triggers[0].ability.targets` at `CreateDelayedTrigger`
    /// resolution — before any later token exists.
    ///
    /// CR 603.7c: A delayed triggered ability that refers to information from
    /// its creation event keeps that creation-time binding for later resolution.
    ///
    /// Hostile multi-authority fixture: after the snapshot, a SECOND unrelated
    /// token is created (mutating `last_created_token_ids`). The discriminating
    /// assertion is that the snapshot equals the FIRST token's id — a live
    /// re-read at firing would instead point at the second token. Firing the
    /// stored ability then sacrifices the FIRST token and leaves the second
    /// untouched, confirming the snapshot is what production consumes.
    #[test]
    fn delayed_sacrifice_it_snapshots_first_token_not_later_token() {
        let mut state = GameState::new_two_player(42);

        // The token created by this resolution (Saheeli's 5/5 copy).
        let first_token = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Saheeli Token".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&first_token)
            .unwrap()
            .card_types
            .core_types = vec![crate::types::card_type::CoreType::Creature];
        // CopyTokenOf records the created token id here; the snapshot reads it.
        state.last_created_token_ids = vec![first_token];

        // "Sacrifice it at the beginning of the next end step" — the anaphoric
        // "it" parses to `TargetFilter::LastCreated`.
        let inner = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Sacrifice {
                target: crate::types::ability::TargetFilter::LastCreated,
                count: QuantityExpr::Fixed { value: 1 },
                min_count: 0,
            },
        );
        let create = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
                effect: Box::new(inner),
                uses_tracked_set: false,
            },
            vec![],
            ObjectId(100), // Saheeli's source id
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &create, &mut events).expect("CreateDelayedTrigger resolves");

        // Discriminating assertion: the snapshot captured the FIRST token at
        // creation. A live re-read at firing would instead read the second.
        assert_eq!(
            state.delayed_triggers[0].ability.targets,
            vec![crate::types::ability::TargetRef::Object(first_token)],
            "CR 603.7c: the delayed 'sacrifice it' must snapshot the just-created \
             token's id at creation time"
        );

        // A SECOND, unrelated token is created before the end step fires,
        // mutating `last_created_token_ids`.
        let second_token = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Later Token".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&second_token)
            .unwrap()
            .card_types
            .core_types = vec![crate::types::card_type::CoreType::Creature];
        state.last_created_token_ids = vec![second_token];

        // Fire the stored delayed ability through the effect dispatcher.
        let fired = state.delayed_triggers[0].ability.clone();
        let mut fire_events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &fired, &mut fire_events, 0)
            .expect("delayed sacrifice resolves");

        assert!(
            state.players[0].graveyard.contains(&first_token),
            "the FIRST (snapshotted) token is sacrificed at the end step"
        );
        assert!(
            state.battlefield.contains(&second_token),
            "the later, unrelated token must survive — the snapshot did not drift to it"
        );
    }

    /// CR 603.7c + CR 608.2c (issue #5972): plural "those tokens" delayed exile
    /// binds the tracked set with origin `Battlefield`. A token that already
    /// left the battlefield is skipped; the remaining member is exiled.
    #[test]
    fn tracked_set_battlefield_cleanup_skips_departed_token() {
        let mut state = GameState::new_two_player(42);
        let first_token = crate::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Twinflame Token A".to_string(),
            Zone::Battlefield,
        );
        let second_token = crate::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Twinflame Token B".to_string(),
            Zone::Battlefield,
        );
        for token in [first_token, second_token] {
            state.objects.get_mut(&token).unwrap().card_types.core_types =
                vec![crate::types::card_type::CoreType::Creature];
        }

        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![first_token, second_token]);
        state.next_tracked_set_id = 2;
        state.chain_tracked_set_id = Some(TrackedSetId(1));

        // One token leaves the battlefield before end-step cleanup fires.
        crate::game::zones::move_to_zone(&mut state, first_token, Zone::Graveyard, &mut Vec::new());

        let delayed = ResolvedAbility::new(
            Effect::ChangeZoneAll {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::TrackedSet {
                    id: TrackedSetId(1),
                },
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: None,
                random_order: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &delayed, &mut events, 0)
            .expect("tracked-set battlefield cleanup resolves");

        assert_eq!(
            state.objects[&first_token].zone,
            Zone::Graveyard,
            "a token that already left the battlefield must not be exiled by cleanup"
        );
        assert_eq!(
            state.objects[&second_token].zone,
            Zone::Exile,
            "the remaining tracked-set token on the battlefield must be exiled"
        );
    }

    /// CR 603.7c (issue #5972): binding preserves an explicit Battlefield
    /// origin when upgrading `ChangeZone { TrackedSet }` → `ChangeZoneAll`.
    #[test]
    fn bind_tracked_set_preserves_battlefield_origin_on_change_zone_upgrade() {
        let mut state = GameState::new_two_player(42);
        state
            .tracked_object_sets
            .insert(TrackedSetId(1), vec![ObjectId(10)]);
        state.next_tracked_set_id = 2;

        let effect_def = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
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
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
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
        resolve(&mut state, &ability, &mut events).expect("resolve must succeed");

        match &state.delayed_triggers[0].ability.effect {
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
                ..
            } => {
                assert_eq!(*origin, Some(Zone::Battlefield));
                assert_eq!(*destination, Zone::Exile);
                assert_eq!(
                    *target,
                    TargetFilter::TrackedSet {
                        id: TrackedSetId(1)
                    }
                );
            }
            other => panic!("expected ChangeZoneAll, got {other:?}"),
        }
    }
}
