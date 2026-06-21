use crate::game::filter;
use crate::game::layers::evaluate_condition;
use crate::game::quantity::{quantity_expr_uses_recipient, resolve_quantity_with_targets};
use crate::types::ability::{
    ContinuousModification, ControllerRef, Duration, Effect, EffectError, EffectKind, QuantityExpr,
    QuantityRef, ResolvedAbility, StaticCondition, StaticDefinition, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Effect handler: creates transient continuous effects from a GenericEffect.
///
/// Resolved GenericEffect definitions are registered as state-level transient
/// continuous effects with explicit durations, rather than being pushed onto
/// individual game objects. This ensures proper layer evaluation and cleanup.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    if let Effect::GenericEffect {
        static_abilities,
        duration,
        target,
    } = &ability.effect
    {
        // CR 611.2b: Default UntilEndOfTurn applies to non-"becomes" GenericEffects
        // (pump spells, etc.). "Becomes" effects inject Duration::Permanent at parse time.
        let dur = ability
            .duration
            .clone()
            .or(duration.clone())
            .unwrap_or(Duration::UntilEndOfTurn);

        for static_def in static_abilities {
            // CR 611.2d: A continuous effect's variable (X) is determined once,
            // on resolution. Snapshot resolution-context quantity refs (e.g.
            // "where X is the result" of the preceding die roll) to constants
            // before registration, so the layer system never re-resolves them
            // against game state that no longer carries the resolution context.
            let mut static_def = static_def.clone();
            for modification in &mut static_def.modifications {
                match modification {
                    ContinuousModification::AddDynamicPower { value }
                    | ContinuousModification::AddDynamicToughness { value }
                    | ContinuousModification::AddDynamicKeyword { value, .. } => {
                        *value = snapshot_resolution_context_quantity(value, events);
                    }
                    _ => {}
                }
            }
            let static_def = &static_def;
            // CR 603.4 + CR 608.2h + CR 611.2d: An in-effect "if <condition>"
            // carried by a `StaticDefinition` (Odric, Lunarch Marshal:
            // "creatures you control gain first strike ... if a creature you
            // control has first strike") is NOT an intervening-if (CR 603.4 —
            // that rule applies only to an "if" immediately after the trigger
            // condition). It is information the resolving ability requires
            // from the game, so per CR 608.2h / CR 611.2d its truth is
            // determined exactly once, here, when the effect is applied. We
            // register a transient continuous effect for the satisfied subset
            // only and pass `condition: None` to `register_transient_effect`
            // so `layers.rs` never re-evaluates it — the resulting grant
            // persists for `dur` (CR 611.2c) regardless of later state.
            if let Some(condition) = &static_def.condition {
                if !evaluate_static_condition_for_ability(state, condition, ability) {
                    continue;
                }
                let mut snapshotted = static_def.clone();
                snapshotted.condition = None;
                register_transient_effect(state, ability, &snapshotted, target.as_ref(), &dur);
            } else {
                register_transient_effect(state, ability, static_def, target.as_ref(), &dur);
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn evaluate_static_condition_for_ability(
    state: &GameState,
    condition: &StaticCondition,
    ability: &ResolvedAbility,
) -> bool {
    match condition {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => comparator.evaluate(
            resolve_quantity_with_targets(state, lhs, ability),
            resolve_quantity_with_targets(state, rhs, ability),
        ),
        StaticCondition::And { conditions } => conditions
            .iter()
            .all(|condition| evaluate_static_condition_for_ability(state, condition, ability)),
        StaticCondition::Or { conditions } => conditions
            .iter()
            .any(|condition| evaluate_static_condition_for_ability(state, condition, ability)),
        StaticCondition::Not { condition } => {
            !evaluate_static_condition_for_ability(state, condition, ability)
        }
        _ => evaluate_condition(state, condition, ability.controller, ability.source_id),
    }
}

fn register_transient_effect(
    state: &mut GameState,
    ability: &ResolvedAbility,
    static_def: &StaticDefinition,
    target_filter: Option<&TargetFilter>,
    duration: &Duration,
) {
    let modifications = snapshot_transient_modifications(state, ability, &static_def.modifications);

    // CR 608.2c (issue #323 class): SelfRef is the printed-name anaphor and
    // always refers to the source object regardless of `ability.targets`.
    // Short-circuit BEFORE the chosen-targets branch so chained Effect
    // sub-abilities with `target: SelfRef` don't inherit the parent's targets
    // via chain propagation in `effects::mod.rs::resolve_ability_chain`.
    if matches!(
        target_filter.or(static_def.affected.as_ref()),
        Some(TargetFilter::SelfRef)
    ) {
        state.add_transient_continuous_effect(
            ability.source_id,
            ability.controller,
            duration.clone(),
            TargetFilter::SpecificObject {
                id: ability.source_id,
            },
            modifications,
            static_def.condition.clone(),
        );
        return;
    }
    // CR 603.7 + CR 611.2c: Token followup grants ("It has trample, haste, and …")
    // target `LastCreated` — bind directly to the just-created token(s) instead
    // of broadcasting across the battlefield (issue #3297: Rite of the Raging
    // Storm was granting haste/trample/sacrifice to the enchantment source).
    if matches!(
        target_filter.or(static_def.affected.as_ref()),
        Some(TargetFilter::LastCreated)
    ) {
        for obj_id in state.last_created_token_ids.clone() {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificObject { id: obj_id },
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
        return;
    }
    // CR 603.2 + CR 608.2c: Judith modal sub-abilities set `target:
    // TriggeringSource` (the GenericEffect `target` parameter); a SequentialSibling
    // continuous grant on a non-targeted trigger ("put a +1/+1 counter on it. It
    // gains haste until end of turn" — Surrak and Goreclaw, issue #2378) instead
    // carries `affected: TriggeringSource` with `target: None`. Both name the
    // triggering object directly, so when there is no chosen target to inherit
    // (`ability.targets.is_empty()`), resolve via `resolve_event_context_target`
    // here. We must NOT short-circuit when targets exist: `affected:
    // TriggeringSource` with chosen targets is the inherited-target form that the
    // branch below resolves against `ability.targets` (Earthbender Ascension).
    let affected_is_triggering_source = static_def
        .affected
        .as_ref()
        .is_some_and(|filter| matches!(filter, TargetFilter::TriggeringSource));
    if matches!(target_filter, Some(TargetFilter::TriggeringSource))
        || (target_filter.is_none() && affected_is_triggering_source && ability.targets.is_empty())
    {
        if let Some(TargetRef::Object(obj_id)) =
            crate::game::targeting::resolve_event_context_target(
                state,
                &TargetFilter::TriggeringSource,
                ability.source_id,
            )
        {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificObject { id: obj_id },
                modifications,
                static_def.condition.clone(),
            );
        }
        return;
    }
    // CR 608.2c + CR 611.2c: `GenericEffect.target` is the player-chosen
    // targeting slot (e.g. `Typed(Creature)` for "target creature"), while
    // `static_def.affected` is the runtime binding filter (often
    // `ParentTarget` for "apply to the chosen object"). Registration must
    // follow `affected` when it carries an inherited-target reference —
    // otherwise `target_filter.or(affected)` prefers the broadcast
    // targeting descriptor and fans the grant to every matching permanent
    // (issue #2922: Mu Yanling +2).
    let application_filter =
        generic_effect_application_filter(target_filter, static_def.affected.as_ref());
    let static_affected_references_target_player = target_filter.is_none()
        && static_def
            .affected
            .as_ref()
            .is_some_and(crate::game::ability_utils::filter_references_target_player);
    let inherited_object_target = static_def
        .affected
        .as_ref()
        .is_some_and(generic_effect_affected_uses_inherited_targets)
        && !static_affected_references_target_player
        && ability
            .targets
            .iter()
            .any(|target| matches!(target, TargetRef::Object(_)));
    let direct_binding_uses_targets = target_filter.is_some()
        || application_filter.is_some_and(generic_effect_affected_uses_inherited_targets)
        || inherited_object_target;

    // CR 611.1 + CR 611.2c + CR 115.1: Targeted effects — register one transient
    // continuous effect per target. `TargetRef::Object` binds to
    // `SpecificObject { id }`; `TargetRef::Player` binds to
    // `SpecificPlayer { id }`. The player branch mirrors the object branch and
    // makes the player-scoped restriction family (CR 305.1 land-play, CR 119.7
    // life-gain, CR 119.8 life-loss, CR 104.2b/3b game-loss/win) reachable from
    // one-shot targeted abilities — e.g. Pardic Miner's "Target player can't
    // play lands this turn". The resulting TCE is read by player-scoped runtime
    // queries (`player_has_static_other`, `player_has_cant_gain_life`, etc.)
    // that scan `state.transient_continuous_effects` directly.
    // A `ControllerRef::TargetPlayer` affected filter is different: its player
    // target parameterizes a broadcast object filter and is resolved below.
    if !ability.targets.is_empty()
        && direct_binding_uses_targets
        && !static_affected_references_target_player
    {
        let skip_companion_player_target = target_filter
            .is_some_and(crate::game::ability_utils::filter_references_target_player)
            && matches!(ability.targets.first(), Some(TargetRef::Player(_)));
        for bound_filter in transient_bound_filters(
            ability,
            application_filter,
            skip_companion_player_target,
            inherited_object_target,
        ) {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                bound_filter,
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
        return;
    }

    // Shared registration for player-scope fan-out arms: bind one
    // `SpecificPlayer` TCE per player id. Takes `state` as a parameter so the
    // sibling object/single-player arms below retain exclusive access to it.
    let register_for_players = |state: &mut GameState, ids: Vec<PlayerId>| {
        for player_id in ids {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificPlayer { id: player_id },
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
    };

    // Non-targeted: resolve the affected filter (SelfRef handled above).
    match application_filter {
        // CR 113.10 + CR 702.16j: Player-scoped affected filter — register the
        // transient effect bound to the ability's controller (a player) via
        // SpecificPlayer. Queried by player_has_protection_from_everything
        // and friends in static_abilities.rs.
        Some(TargetFilter::Controller) => {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificPlayer {
                    id: ability.controller,
                },
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
        // Pass-through: the caller already pinned a specific player.
        Some(TargetFilter::SpecificPlayer { id }) => {
            state.add_transient_continuous_effect(
                ability.source_id,
                ability.controller,
                duration.clone(),
                TargetFilter::SpecificPlayer { id: *id },
                modifications.clone(),
                static_def.condition.clone(),
            );
        }
        // CR 104.3: "There are several ways to lose the game." + CR 119.7: "If an
        // effect says that a player can't gain life, that player can't make their
        // life total increase." + CR 119.8: "If an effect says that a player can't
        // lose life, that player can't make their life total decrease."
        // Bare-player scope ("Players can't ...") fans out to one transient effect
        // per non-eliminated player so player-scoped runtime queries
        // (`player_has_cant_lose`, `player_has_cant_gain_life`, etc.) see a
        // `SpecificPlayer`-bound TCE for each player. Without this branch,
        // spell-applied player-scoped statics like Everybody Lives! never reach
        // those queries.
        Some(TargetFilter::Player) => {
            let player_ids: Vec<PlayerId> = state
                .players
                .iter()
                .filter(|p| !p.is_eliminated)
                .map(|p| p.id)
                .collect();
            register_for_players(state, player_ids);
        }
        // CR 119.7 + CR 119.8: Bare player-scope `Typed` affected
        // filter. A `TargetFilter::Typed` with no `type_filters` and no
        // `properties`, carrying a `You`/`Opponent`/unscoped `controller`, is the
        // engine's canonical *player* filter (see `targeting.rs`: "Typed filter
        // with no type_filters targets players, not permanents"). The "[possessor]
        // life total can't change" parser (Teferi's Protection) and the
        // "[possessor] can't gain/lose life" restriction parser emit player-scoped
        // statics with this shape. Resolve the `controller` ref to concrete
        // player(s) via the shared `collect_player_targets` authority — which
        // matches every `ControllerRef` variant exhaustively with per-variant CR
        // annotations (no `_ => true` wildcard over the closed enum) — and bind
        // each as `SpecificPlayer` so player-scoped runtime queries
        // (`player_has_cant_gain_life`, etc.) can find them. Without this arm the
        // grant falls through to the object-broadcast branch below, binds to
        // (zero, under Teferi's mass phase-out) battlefield objects as
        // `SpecificObject`, and the life-lock silently never applies. The guard
        // keeps the arm scoped to `You`/`Opponent`/unscoped controllers; filters
        // carrying `properties` or a context-relative controller are genuine
        // object filters and stay on the broadcast path.
        Some(player_filter @ TargetFilter::Typed(tf))
            if tf.type_filters.is_empty()
                && tf.properties.is_empty()
                && matches!(
                    tf.controller,
                    None | Some(ControllerRef::You) | Some(ControllerRef::Opponent)
                ) =>
        {
            // CR 104.2: eliminated players hold no game-state restrictions.
            let eliminated: std::collections::HashSet<PlayerId> = state
                .players
                .iter()
                .filter(|p| p.is_eliminated)
                .map(|p| p.id)
                .collect();
            let player_ids: Vec<PlayerId> =
                crate::game::ability_utils::collect_player_targets(state, ability, player_filter)
                    .into_iter()
                    .filter(|id| !eliminated.contains(id))
                    .collect();
            register_for_players(state, player_ids);
        }
        Some(TargetFilter::None) | None => {}
        // CR 608.2k: A grant whose affected object is the ability's cost-paid
        // object (Jhoira of the Ghitu's suspend grant — "If it doesn't have
        // suspend, it gains suspend", the "it" anaphor parsed as `ParentTarget`
        // on the suspend-grant sub_ability). The exiled card is in the exile
        // zone, so the battlefield-scan branch below cannot reach it; resolve
        // directly from the recursively-stamped `cost_paid_object`. A bare
        // `ParentTarget` with no chosen targets but a stamped cost-paid object
        // is treated as the cost-paid reference (the parent acted on it).
        Some(TargetFilter::CostPaidObject) | Some(TargetFilter::ParentTarget)
            if ability.targets.is_empty() && ability.cost_paid_object.is_some() =>
        {
            if let Some(snap) = &ability.cost_paid_object {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id: snap.object_id },
                    modifications.clone(),
                    static_def.condition.clone(),
                );
            }
        }
        // TriggeringSource is handled via early short-circuit to avoid target propagation bugs.
        Some(TargetFilter::ParentTarget) if ability.targets.is_empty() => {
            let tracked = state
                .chain_tracked_set_id
                .and_then(|id| state.tracked_object_sets.get(&id).cloned())
                .unwrap_or_default();
            for obj_id in tracked {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id: obj_id },
                    modifications.clone(),
                    static_def.condition.clone(),
                );
            }
        }
        Some(filter) => {
            if generic_effect_affected_uses_inherited_targets(filter) {
                return;
            }
            let filter = crate::game::effects::resolved_object_filter(ability, filter);
            let filter = crate::game::targeting::resolve_tracked_set_sentinel(state, filter);
            // Broadcast filter: find matching objects at resolution time and bind each.
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            let ctx = filter::FilterContext::from_ability(ability);
            let matching: Vec<ObjectId> = state
                .battlefield
                .iter()
                .filter(|obj_id| filter::matches_target_filter(state, **obj_id, &filter, &ctx))
                .copied()
                .collect();
            for obj_id in matching {
                state.add_transient_continuous_effect(
                    ability.source_id,
                    ability.controller,
                    duration.clone(),
                    TargetFilter::SpecificObject { id: obj_id },
                    modifications.clone(),
                    static_def.condition.clone(),
                );
            }
        }
    }
}

fn transient_bound_filters(
    ability: &ResolvedAbility,
    resolved_filter: Option<&TargetFilter>,
    skip_companion_player_target: bool,
    inherited_object_target: bool,
) -> Vec<TargetFilter> {
    if inherited_object_target {
        let Some(filter) = resolved_filter else {
            return Vec::new();
        };
        return crate::game::effects::effect_object_targets(filter, &ability.targets)
            .into_iter()
            .map(|id| TargetFilter::SpecificObject { id })
            .collect();
    }

    ability
        .targets
        .iter()
        .skip(usize::from(skip_companion_player_target))
        .map(|target| match target {
            TargetRef::Object(obj_id) => TargetFilter::SpecificObject { id: *obj_id },
            TargetRef::Player(player_id) => TargetFilter::SpecificPlayer { id: *player_id },
        })
        .collect()
}

fn generic_effect_affected_uses_inherited_targets(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::TriggeringSource | TargetFilter::ParentTarget | TargetFilter::CostPaidObject
    )
}

/// CR 608.2c: Choose the filter that governs *where modifications land* at
/// resolution. The outer `GenericEffect.target` slot names what the player
/// chose; `static_def.affected` names how that choice is bound onto objects.
/// Inherited-reference `affected` values (`ParentTarget`, …) win over the
/// targeting descriptor so a `target: Typed(Creature)` + `affected:
/// ParentTarget` pair binds to the chosen creature, not every creature.
fn generic_effect_application_filter<'a>(
    target_filter: Option<&'a TargetFilter>,
    static_affected: Option<&'a TargetFilter>,
) -> Option<&'a TargetFilter> {
    if static_affected.is_some_and(generic_effect_affected_uses_inherited_targets) {
        static_affected
    } else {
        target_filter.or(static_affected)
    }
}

fn snapshot_transient_modifications(
    state: &GameState,
    ability: &ResolvedAbility,
    modifications: &[ContinuousModification],
) -> Vec<ContinuousModification> {
    modifications
        .iter()
        .map(|modification| match modification {
            ContinuousModification::AddDynamicPower { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::AddPower {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            ContinuousModification::AddDynamicToughness { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::AddToughness {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            ContinuousModification::SetPowerDynamic { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::SetPower {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            ContinuousModification::SetToughnessDynamic { value }
                if !quantity_expr_uses_recipient(value) =>
            {
                ContinuousModification::SetToughness {
                    value: resolve_quantity_with_targets(state, value, ability),
                }
            }
            _ => modification.clone(),
        })
        .collect()
}

/// CR 611.2d: A resolving spell/ability that creates a continuous effect with a
/// variable X determines that variable's value only once, on resolution.
///
/// Walks a `QuantityExpr` tree and replaces every resolution-context leaf —
/// currently only `QuantityRef::EventContextAmount`, which "where X is the
/// result" of a preceding die roll (CR 706.2) compiles to — with a constant
/// `Fixed`. The amount is read from the most recent amount-yielding event in
/// this resolution's `events` slice (the `RollDie` sub-ability resolved one
/// step earlier, so its `GameEvent::DieRolled` is present).
///
/// Persistent game-state refs (`ObjectCount`, `LifeTotal`, `Power`, …) are left
/// UNTOUCHED so CDA-style "+1/+1 for each X" continuous mods keep their dynamic
/// behavior — only resolution-context refs, which read transient context that
/// is gone by the next layer recompute, are snapshotted.
fn snapshot_resolution_context_quantity(expr: &QuantityExpr, events: &[GameEvent]) -> QuantityExpr {
    match expr {
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        } => {
            let amount = events
                .iter()
                .rev()
                .find_map(crate::game::targeting::extract_amount_from_event);
            // GAP-3 silent-failure guard: an `EventContextAmount` leaf with no
            // source event in this resolution would snapshot to 0 and silently
            // produce a +0/+0 pump. On a silent-failure-remediation branch that
            // must trip in debug builds rather than ship a no-op.
            debug_assert!(
                amount.is_some(),
                "snapshot_resolution_context_quantity: EventContextAmount leaf found no \
                 source event in the resolution events slice",
            );
            QuantityExpr::Fixed {
                value: amount.unwrap_or(0),
            }
        }
        QuantityExpr::Ref { .. } | QuantityExpr::Fixed { .. } => expr.clone(),
        QuantityExpr::DivideRounded {
            inner,
            divisor,
            rounding,
        } => QuantityExpr::DivideRounded {
            inner: Box::new(snapshot_resolution_context_quantity(inner, events)),
            divisor: *divisor,
            rounding: *rounding,
        },
        QuantityExpr::Offset { inner, offset } => QuantityExpr::Offset {
            inner: Box::new(snapshot_resolution_context_quantity(inner, events)),
            offset: *offset,
        },
        QuantityExpr::ClampMin { inner, minimum } => QuantityExpr::ClampMin {
            inner: Box::new(snapshot_resolution_context_quantity(inner, events)),
            minimum: *minimum,
        },
        QuantityExpr::Multiply { factor, inner } => QuantityExpr::Multiply {
            factor: *factor,
            inner: Box::new(snapshot_resolution_context_quantity(inner, events)),
        },
        QuantityExpr::Sum { exprs } => QuantityExpr::Sum {
            exprs: exprs
                .iter()
                .map(|e| snapshot_resolution_context_quantity(e, events))
                .collect(),
        },
        QuantityExpr::Max { exprs } => QuantityExpr::Max {
            exprs: exprs
                .iter()
                .map(|e| snapshot_resolution_context_quantity(e, events))
                .collect(),
        },
        QuantityExpr::UpTo { max } => QuantityExpr::UpTo {
            max: Box::new(snapshot_resolution_context_quantity(max, events)),
        },
        QuantityExpr::Power { base, exponent } => QuantityExpr::Power {
            base: *base,
            exponent: Box::new(snapshot_resolution_context_quantity(exponent, events)),
        },
        QuantityExpr::Difference { left, right } => QuantityExpr::Difference {
            left: Box::new(snapshot_resolution_context_quantity(left, events)),
            right: Box::new(snapshot_resolution_context_quantity(right, events)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ContinuousModification, ControllerRef, Duration, QuantityExpr, QuantityRef,
        StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::events::GameEvent;
    use crate::types::identifiers::{CardId, TrackedSetId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn generic_effect_registers_transient_effect_for_self_ref() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.source_id, source);
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: source });
        assert_eq!(tce.duration, Duration::UntilEndOfTurn);
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]
        );
    }

    #[test]
    fn generic_effect_registers_transient_effect_for_matching_filter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let your_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ally".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&your_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opp_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Enemy".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opp_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should create transient effect for your_creature only
        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject { id: your_creature }
        );
    }

    #[test]
    fn generic_effect_target_player_affected_filter_binds_matching_objects_not_player() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sudden Spoiling".to_string(),
            Zone::Stack,
        );
        let your_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Your Creature".to_string(),
            Zone::Battlefield,
        );
        let target_players_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Target Player's Creature".to_string(),
            Zone::Battlefield,
        );
        for object_id in [your_creature, target_players_creature] {
            state
                .objects
                .get_mut(&object_id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::TargetPlayer),
            ))
            .modifications(vec![
                ContinuousModification::RemoveAllAbilities,
                ContinuousModification::SetPower { value: 0 },
                ContinuousModification::SetToughness { value: 2 },
            ]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificObject {
                id: target_players_creature
            },
            "TargetPlayer affected filter must bind to the chosen player's creature"
        );
        assert!(tce
            .modifications
            .contains(&ContinuousModification::RemoveAllAbilities));
        assert!(tce
            .modifications
            .contains(&ContinuousModification::SetPower { value: 0 }));
        assert!(tce
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 2 }));
    }

    #[test]
    fn generic_effect_target_player_sibling_static_resolves_own_affected_filter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Continuous Source".to_string(),
            Zone::Stack,
        );
        let your_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Your Creature".to_string(),
            Zone::Battlefield,
        );
        let target_players_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Target Player's Creature".to_string(),
            Zone::Battlefield,
        );
        for object_id in [your_creature, target_players_creature] {
            state
                .objects
                .get_mut(&object_id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let target_player_static = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::TargetPlayer),
            ))
            .modifications(vec![ContinuousModification::SetPower { value: 0 }]);
        let controller_static = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::SetToughness { value: 2 }]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![target_player_static, controller_static],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 2);
        assert!(state.transient_continuous_effects.iter().any(|tce| {
            tce.affected
                == (TargetFilter::SpecificObject {
                    id: target_players_creature,
                })
                && tce
                    .modifications
                    .contains(&ContinuousModification::SetPower { value: 0 })
        }));
        assert!(state.transient_continuous_effects.iter().any(|tce| {
            tce.affected == (TargetFilter::SpecificObject { id: your_creature })
                && tce
                    .modifications
                    .contains(&ContinuousModification::SetToughness { value: 2 })
        }));
        assert!(!state.transient_continuous_effects.iter().any(|tce| {
            matches!(
                tce.affected,
                TargetFilter::SpecificPlayer { id: PlayerId(1) }
            )
        }));
    }

    #[test]
    fn generic_effect_without_explicit_target_binds_inherited_object_target() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Earthbender Ascension".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::TriggeringSource)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![TargetRef::Object(target_creature)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject {
                id: target_creature
            }
        );
    }

    /// Issue #2013: Judith's modal mode binds `target: TriggeringSource` with no
    /// chosen targets; the grant must reach the cast instant/sorcery on the stack.
    #[test]
    fn triggering_source_grant_binds_cast_spell_without_targets() {
        let mut state = GameState::new_two_player(42);
        let judith = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Judith, Carnage Connoisseur".to_string(),
            Zone::Battlefield,
        );
        let cast_spell = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&cast_spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(2),
            controller: PlayerId(0),
            object_id: cast_spell,
        });

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Deathtouch,
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Lifelink,
                },
            ]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::TriggeringSource),
            },
            vec![],
            judith,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificObject { id: cast_spell }
        );
        assert!(tce
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Deathtouch,
            }));
        assert!(tce
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }));
    }

    /// Issue #2378 class: a non-targeted trigger whose SequentialSibling continuous
    /// grant carries `affected: TriggeringSource` with `target: None` and no chosen
    /// targets ("put a +1/+1 counter on it. It gains haste until end of turn" —
    /// Surrak and Goreclaw) must bind the grant to the triggering object via the
    /// event-context resolver (CR 611.2c). Pre-fix this fell through to the
    /// broadcast arm, which `return`ed early on the inherited-reference filter and
    /// dropped the grant entirely. Distinct from the Earthbender test above, which
    /// has the same affected/target shape *with* a chosen target to inherit.
    #[test]
    fn triggering_source_affected_without_target_or_chosen_targets_binds_trigger_source() {
        use crate::types::game_state::ZoneChangeRecord;

        let mut state = GameState::new_two_player(42);
        let surrak = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Surrak and Goreclaw".to_string(),
            Zone::Battlefield,
        );
        let entering = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Battlefield,
        );
        // The trigger fired from the creature's enter-the-battlefield event;
        // `TriggeringSource` resolves to `entering` through this record.
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: entering,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord::test_minimal(
                entering,
                None,
                Zone::Battlefield,
            )),
        });

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::TriggeringSource)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }]);
        // No chosen targets: a non-targeted "it gains haste" sibling clause.
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            surrak,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.transient_continuous_effects.len(),
            1,
            "the haste grant must bind to exactly the triggering creature"
        );
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: entering });
        assert!(tce
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Haste,
            }));
    }

    /// Issue #323 class: propagated `ability.targets` must not override TriggeringSource.
    #[test]
    fn triggering_source_short_circuits_when_targets_propagated() {
        let mut state = GameState::new_two_player(42);
        let judith = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Judith, Carnage Connoisseur".to_string(),
            Zone::Battlefield,
        );
        let cast_spell = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Stack,
        );
        let wrong_target = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Wrong".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&cast_spell)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(11),
            controller: PlayerId(0),
            object_id: cast_spell,
        });

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Lifelink,
            }]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::TriggeringSource),
            },
            vec![TargetRef::Object(wrong_target)],
            judith,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject { id: cast_spell },
            "TriggeringSource must bind to the cast spell, not propagated targets"
        );
    }

    #[test]
    fn generic_effect_inherited_object_binding_ignores_sibling_player_target() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Inherited Context Source".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::TriggeringSource)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![
                TargetRef::Player(PlayerId(1)),
                TargetRef::Object(target_creature),
            ],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject {
                id: target_creature
            }
        );
    }

    #[test]
    fn generic_effect_without_explicit_target_ignores_inherited_object_for_broadcast_filter() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Broadcast Source".to_string(),
            Zone::Battlefield,
        );
        let inherited_target = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Inherited Target".to_string(),
            Zone::Battlefield,
        );
        let your_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Your Creature".to_string(),
            Zone::Battlefield,
        );
        for object_id in [inherited_target, your_creature] {
            state
                .objects
                .get_mut(&object_id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![TargetRef::Object(inherited_target)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject { id: your_creature },
            "non-context affected filters must broadcast through their own filter"
        );
    }

    #[test]
    fn generic_effect_binds_targeted_object_to_specific_object() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let other_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&other_creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Typed(TypedFilter::creature()))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
            },
            vec![TargetRef::Object(target_creature)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject {
                id: target_creature
            }
        );
    }

    /// Issue #2922: `target: Typed(Creature)` names the targeting slot while
    /// `affected: ParentTarget` binds the grant to the chosen creature. The
    /// application filter must follow `ParentTarget`, not broadcast through the
    /// creature targeting descriptor.
    #[test]
    fn parent_target_affected_with_creature_target_slot_binds_chosen_creature_only() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mu Yanling, Sky Dancer".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Chosen Target".to_string(),
            Zone::Battlefield,
        );
        let other_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Other Creature".to_string(),
            Zone::Battlefield,
        );
        for id in [target_creature, other_creature] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(3);
            obj.base_toughness = Some(3);
            obj.power = Some(3);
            obj.toughness = Some(3);
            obj.keywords.push(Keyword::Flying);
        }

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![
                ContinuousModification::AddPower { value: -2 },
                ContinuousModification::AddToughness { value: 0 },
                ContinuousModification::RemoveKeyword {
                    keyword: Keyword::Flying,
                },
            ]);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilNextTurnOf {
                    player: crate::types::ability::PlayerScope::Controller,
                }),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
            },
            vec![TargetRef::Object(target_creature)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        });

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.transient_continuous_effects.len(),
            1,
            "exactly one TCE for the chosen target"
        );
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject {
                id: target_creature
            }
        );

        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&target_creature).unwrap().power,
            Some(1),
            "chosen creature gets -2/-0"
        );
        assert!(
            !state
                .objects
                .get(&target_creature)
                .unwrap()
                .keywords
                .contains(&Keyword::Flying),
            "chosen creature loses flying"
        );
        assert_eq!(
            state.objects.get(&other_creature).unwrap().power,
            Some(3),
            "non-target creature must not be debuffed"
        );
        assert!(
            state
                .objects
                .get(&other_creature)
                .unwrap()
                .keywords
                .contains(&Keyword::Flying),
            "non-target creature must keep flying"
        );
    }

    /// Issue #2922 guard: when no target was chosen ("up to one" → zero),
    /// `ParentTarget` must not fall through to a creature-broadcast arm.
    #[test]
    fn parent_target_affected_with_creature_target_slot_empty_targets_does_not_broadcast() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mu Yanling, Sky Dancer".to_string(),
            Zone::Battlefield,
        );
        let creature_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        let creature_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Creature B".to_string(),
            Zone::Battlefield,
        );
        for id in [creature_a, creature_b] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.power = Some(2);
        }

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![ContinuousModification::AddPower { value: -2 }]);
        let mut ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilNextTurnOf {
                    player: crate::types::ability::PlayerScope::Controller,
                }),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
            },
            vec![],
            source,
            PlayerId(0),
        );
        ability.optional_targeting = true;

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.is_empty(),
            "zero targets must not register battlefield-wide debuffs"
        );
        crate::game::layers::evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&creature_a).unwrap().power, Some(2));
        assert_eq!(state.objects.get(&creature_b).unwrap().power, Some(2));
    }

    /// Issue #2922 end-to-end: parser output for Mu Yanling's +2 resolves onto
    /// the single chosen creature only.
    #[test]
    fn mu_yanling_plus_two_pump_and_lose_flying_parses_and_resolves_to_target_only() {
        use crate::game::layers::evaluate_layers;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let mut state = GameState::new_two_player(42);
        let yanling = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mu Yanling, Sky Dancer".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let bystander = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Bystander".to_string(),
            Zone::Battlefield,
        );
        for id in [target_creature, bystander] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(4);
            obj.base_toughness = Some(4);
            obj.power = Some(4);
            obj.toughness = Some(4);
            obj.keywords.push(Keyword::Flying);
        }

        let parsed = parse_effect_chain(
            "up to one target creature gets -2/-0 and loses flying",
            AbilityKind::Activated,
        );
        let ability = ResolvedAbility::new(
            (*parsed.effect).clone(),
            vec![TargetRef::Object(target_creature)],
            yanling,
            PlayerId(0),
        )
        .duration(parsed.duration.clone().unwrap_or(Duration::UntilEndOfTurn));

        let Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } = &ability.effect
        else {
            panic!("expected single GenericEffect, got {:?}", ability.effect);
        };
        assert!(
            target.is_some(),
            "targeting slot must be present on the parsed GenericEffect"
        );
        assert_eq!(
            static_abilities[0].affected,
            Some(TargetFilter::ParentTarget),
            "per-static affected must bind to ParentTarget"
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        assert_eq!(
            state.transient_continuous_effects[0].affected,
            TargetFilter::SpecificObject {
                id: target_creature
            }
        );

        evaluate_layers(&mut state);
        assert_eq!(state.objects.get(&target_creature).unwrap().power, Some(2));
        assert!(!state
            .objects
            .get(&target_creature)
            .unwrap()
            .keywords
            .contains(&Keyword::Flying));
        assert_eq!(state.objects.get(&bystander).unwrap().power, Some(4));
        assert!(state
            .objects
            .get(&bystander)
            .unwrap()
            .keywords
            .contains(&Keyword::Flying));
    }

    /// CR 608.2c + CR 611.2a + CR 702.7: Gallant Fowlknight ETB end-to-end —
    /// "creatures you control get +1/+0 until end of turn. Kithkin creatures you
    /// control also gain first strike until end of turn." After resolving the
    /// full parsed chain (PumpAll + the subtype-filtered first-strike grant)
    /// through the production effect resolver and layer evaluation, BOTH
    /// controlled creatures gain +1/+0, but ONLY the Kithkin gains first strike.
    /// Reverting `strip_trailing_additive_adverb` drops the second sentence to
    /// `Effect::Unimplemented`, leaving the non-Kithkin and the Kithkin alike
    /// without first strike — the Kithkin first-strike assertion then fails.
    #[test]
    fn gallant_fowlknight_first_strike_only_on_kithkin_after_resolution() {
        use crate::game::layers::evaluate_layers;
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gallant Fowlknight".to_string(),
            Zone::Battlefield,
        );
        let kithkin = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Kithkin Ally".to_string(),
            Zone::Battlefield,
        );
        let non_kithkin = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Plain Bear".to_string(),
            Zone::Battlefield,
        );
        for id in [kithkin, non_kithkin] {
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        // Only the first creature is a Kithkin.
        state
            .objects
            .get_mut(&kithkin)
            .unwrap()
            .card_types
            .subtypes
            .push("Kithkin".to_string());

        // Parse the real ETB effect body (the two chained sentences).
        let parsed = parse_effect_chain(
            "creatures you control get +1/+0 until end of turn. Kithkin creatures \
             you control also gain first strike until end of turn.",
            AbilityKind::Spell,
        );

        // Resolve every clause in the chain through the production resolver.
        let mut node: Option<&crate::types::ability::AbilityDefinition> = Some(&parsed);
        let mut resolved_any_unimplemented = false;
        while let Some(def) = node {
            if matches!(*def.effect, Effect::Unimplemented { .. }) {
                resolved_any_unimplemented = true;
            }
            let ability = ResolvedAbility::new((*def.effect).clone(), vec![], source, PlayerId(0))
                .duration(def.duration.clone().unwrap_or(Duration::UntilEndOfTurn));
            let mut events = Vec::new();
            // Drive through the top-level effect dispatcher so `PumpAll` routes to
            // `pump::resolve_all` and the `GenericEffect` first-strike grant
            // routes to `effect::resolve` — the same dispatch the stack uses.
            crate::game::effects::resolve_effect(&mut state, &ability, &mut events).unwrap();
            node = def.sub_ability.as_deref();
        }
        assert!(
            !resolved_any_unimplemented,
            "the parsed chain must not contain Unimplemented clauses"
        );

        evaluate_layers(&mut state);

        // Both controlled creatures gain +1/+0 from the PumpAll clause.
        assert_eq!(
            state.objects.get(&kithkin).unwrap().power,
            Some(3),
            "Kithkin must get +1/+0"
        );
        assert_eq!(
            state.objects.get(&non_kithkin).unwrap().power,
            Some(3),
            "non-Kithkin must also get +1/+0"
        );

        // Only the Kithkin gains first strike from the subtype-filtered grant.
        assert!(
            state
                .objects
                .get(&kithkin)
                .unwrap()
                .has_keyword(&Keyword::FirstStrike),
            "Kithkin must gain first strike"
        );
        assert!(
            !state
                .objects
                .get(&non_kithkin)
                .unwrap()
                .has_keyword(&Keyword::FirstStrike),
            "non-Kithkin must NOT gain first strike"
        );
    }

    // CR 305.1 + CR 611.1 + CR 611.2c + CR 115.1: A `GenericEffect` whose target
    // slot resolves to a player (Pardic Miner: "Target player can't play lands
    // this turn") must register a transient continuous effect bound to
    // `SpecificPlayer { id }` for the chosen player. Mirrors the
    // `generic_effect_binds_targeted_object_to_specific_object` test for the
    // object-target branch — proves the player branch in
    // `register_transient_effect` fires symmetrically.
    #[test]
    fn generic_effect_binds_targeted_player_to_specific_player() {
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Pardic Miner".to_string(),
            Zone::Battlefield,
        );

        let static_def = StaticDefinition::new(StaticMode::Other("CantPlayLand".to_string()))
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::Other("CantPlayLand".to_string()),
            }]);

        let target_player = PlayerId(1);
        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::Player),
            },
            vec![TargetRef::Player(target_player)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.transient_continuous_effects.len(),
            1,
            "single TCE bound to the chosen target player"
        );
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificPlayer { id: target_player },
            "TCE must bind to SpecificPlayer for the chosen target"
        );
        assert_eq!(tce.duration, Duration::UntilEndOfTurn);
        assert_eq!(
            tce.modifications,
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::Other("CantPlayLand".to_string()),
            }]
        );

        // End-to-end check: player_has_static_other must now see the prohibition.
        assert!(
            crate::game::static_abilities::player_has_static_other(
                &state,
                target_player,
                "CantPlayLand"
            ),
            "player_has_static_other must observe the TCE-installed CantPlayLand"
        );
        // The activating player must NOT be affected.
        assert!(
            !crate::game::static_abilities::player_has_static_other(
                &state,
                PlayerId(0),
                "CantPlayLand"
            ),
            "non-targeted player must not be under CantPlayLand"
        );
    }

    #[test]
    fn generic_effect_snapshots_dynamic_pt_modifications_at_resolution() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Chorus of Might".to_string(),
            Zone::Battlefield,
        );
        let target_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Target".to_string(),
            Zone::Battlefield,
        );
        let ally_a = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Ally A".to_string(),
            Zone::Battlefield,
        );
        let ally_b = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Ally B".to_string(),
            Zone::Battlefield,
        );
        for id in [target_creature, ally_a, ally_b] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let creature_count = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::ParentTarget)
            .modifications(vec![
                ContinuousModification::AddDynamicPower {
                    value: creature_count.clone(),
                },
                ContinuousModification::AddDynamicToughness {
                    value: creature_count,
                },
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Trample,
                },
            ]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: Some(TargetFilter::Typed(TypedFilter::creature())),
            },
            vec![TargetRef::Object(target_creature)],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let late_ally = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Late Ally".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&late_ally)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let modifications = &state.transient_continuous_effects[0].modifications;
        assert!(
            modifications.contains(&ContinuousModification::AddPower { value: 3 }),
            "dynamic power should snapshot to the creature count at resolution, got {modifications:?}"
        );
        assert!(
            modifications.contains(&ContinuousModification::AddToughness { value: 3 }),
            "dynamic toughness should snapshot to the creature count at resolution, got {modifications:?}"
        );
        assert!(
            !modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddDynamicPower { .. }
                    | ContinuousModification::AddDynamicToughness { .. }
            )),
            "transient P/T pump must not remain live after resolution: {modifications:?}"
        );
    }

    /// CR 702.16j end-to-end: parse Teferi's-Protection-style clause, feed
    /// the parsed effect into `resolve`, and verify the single-authority
    /// query reports the controller as protected. This exercises the full
    /// pipeline from Oracle text to runtime enforcement hook.
    #[test]
    fn parse_and_resolve_you_gain_protection_from_everything_grants_player_protection() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Protection".to_string(),
            Zone::Battlefield,
        );

        let parsed = parse_effect_chain("you gain protection from everything", AbilityKind::Spell);
        let ability = ResolvedAbility::new((*parsed.effect).clone(), vec![], source, PlayerId(0))
            .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            crate::game::static_abilities::player_has_protection_from_everything(
                &state,
                PlayerId(0)
            ),
            "controller must be protected after resolution"
        );
        assert!(
            !crate::game::static_abilities::player_has_protection_from_everything(
                &state,
                PlayerId(1)
            ),
            "opponent must NOT gain protection — scoping is per-controller"
        );
    }

    /// CR 113.10 + CR 702.16j: When a GenericEffect carries `affected:
    /// Controller`, `register_transient_effect` must bind the transient to
    /// `SpecificPlayer { id: ability.controller }`. This is the runtime hook
    /// for Teferi's-Protection-style player-scoped keyword grants.
    #[test]
    fn generic_effect_controller_affected_binds_to_specific_player() {
        use crate::types::ability::TargetFilter;
        use crate::types::keywords::{Keyword, ProtectionTarget};

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Protection".to_string(),
            Zone::Battlefield,
        );

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::Controller)
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(
            tce.affected,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            "Controller-scoped keyword grant must bind to SpecificPlayer for the ability's controller"
        );
        // End-to-end: the registered effect is observable via the single-
        // authority query used by targeting/damage/attack enforcement.
        assert!(
            crate::game::static_abilities::player_has_protection_from_everything(
                &state,
                PlayerId(0)
            )
        );
    }

    /// CR 119.7 + CR 119.8: Teferi's-Protection-style life-lock.
    /// Parse "your life total can't change", feed the parsed effect into
    /// `resolve`, and verify the single-authority queries report the controller
    /// as both can't-gain-life and can't-lose-life. The parser emits these
    /// player-scoped statics with `affected: Typed(controller: You)`; the
    /// runtime registration must bind them to `SpecificPlayer { controller }`
    /// so the transient-table queries used by life-gain/loss/cost enforcement
    /// can find them. Without that, "your life total can't change" silently
    /// never applies for an instant (Teferi's Protection).
    #[test]
    fn parse_and_resolve_your_life_total_cant_change_locks_controller_life() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Teferi's Protection".to_string(),
            Zone::Battlefield,
        );

        let parsed = parse_effect_chain("your life total can't change", AbilityKind::Spell);
        let ability = ResolvedAbility::new((*parsed.effect).clone(), vec![], source, PlayerId(0))
            .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(0)),
            "controller must be locked against life gain after resolution"
        );
        assert!(
            crate::game::static_abilities::player_has_cant_lose_life(&state, PlayerId(0)),
            "controller must be locked against life loss after resolution"
        );
        assert!(
            !crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(1)),
            "opponent must NOT be locked — scoping is per-controller"
        );
        assert!(
            !crate::game::static_abilities::player_has_cant_lose_life(&state, PlayerId(1)),
            "opponent must NOT be locked — scoping is per-controller"
        );

        // CR 119.7 + CR 119.8 end-to-end: a subsequent life gain and life loss
        // on the locked controller are both suppressed — the user-visible
        // behavior the report was about.
        let life_before = state.players[0].life;
        let gain = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 5 },
                player: TargetFilter::Controller,
            },
            vec![],
            source,
            PlayerId(0),
        );
        crate::game::effects::life::resolve_gain(&mut state, &gain, &mut events).unwrap();
        let lose = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 3 },
                target: None,
            },
            vec![TargetRef::Player(PlayerId(0))],
            source,
            PlayerId(0),
        );
        crate::game::effects::life::resolve_lose(&mut state, &lose, &mut events).unwrap();
        assert_eq!(
            state.players[0].life, life_before,
            "locked controller's life total must not change"
        );
    }

    /// CR 119.7: An *opponent*-scoped life-lock ("your opponents' life totals
    /// can't change") binds to each opponent — exercising the same player-scope
    /// `Typed` registration arm with `ControllerRef::Opponent`.
    #[test]
    fn parse_and_resolve_opponents_life_total_cant_change_locks_opponents() {
        use crate::parser::oracle_effect::parse_effect_chain;
        use crate::types::ability::AbilityKind;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Opponent Life Lock".to_string(),
            Zone::Battlefield,
        );

        let parsed = parse_effect_chain(
            "your opponents' life totals can't change",
            AbilityKind::Spell,
        );
        let ability = ResolvedAbility::new((*parsed.effect).clone(), vec![], source, PlayerId(0))
            .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(1)),
            "opponent must be locked against life gain"
        );
        assert!(
            crate::game::static_abilities::player_has_cant_lose_life(&state, PlayerId(1)),
            "opponent must be locked against life loss"
        );
        assert!(
            !crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(0)),
            "controller must NOT be locked — opponent scope excludes the controller"
        );
    }

    /// CR 119.7 + CR 608.2c: Screaming Nemesis's rider end-to-end. A
    /// `GenericEffect { affected: ParentTarget, CantGainLife }` whose parent
    /// (the redirect) targeted a PLAYER must lock exactly that player against
    /// life gain; the same effect whose parent targeted a CREATURE must lock
    /// NO player (CR 119.7 governs players only). This proves the player-gating
    /// is intrinsic to the `ParentTarget`->TargetRef binding, not a parser
    /// guess.
    #[test]
    fn parent_target_cant_gain_life_locks_player_target_only() {
        use crate::types::ability::TargetFilter;
        use crate::types::statics::StaticMode;

        let make_def = || {
            StaticDefinition::new(StaticMode::CantGainLife)
                .affected(TargetFilter::ParentTarget)
                .modifications(vec![ContinuousModification::AddStaticMode {
                    mode: StaticMode::CantGainLife,
                }])
        };
        let make_effect = || Effect::GenericEffect {
            static_abilities: vec![make_def()],
            duration: Some(Duration::Permanent),
            target: None,
        };

        // Case 1: parent target is a player -> that player is locked.
        {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Screaming Nemesis".to_string(),
                Zone::Battlefield,
            );
            let ability = ResolvedAbility::new(
                make_effect(),
                vec![TargetRef::Player(PlayerId(1))],
                source,
                PlayerId(0),
            )
            .duration(Duration::Permanent);
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            assert!(
                crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(1)),
                "player redirect target must be locked against life gain"
            );
            assert!(
                !crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(0)),
                "the source's controller must NOT be locked"
            );
        }

        // Case 2: parent target is a creature -> NO player is locked (CR 119.7).
        {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Screaming Nemesis".to_string(),
                Zone::Battlefield,
            );
            let creature = create_object(
                &mut state,
                CardId(2),
                PlayerId(1),
                "Grizzly Bears".to_string(),
                Zone::Battlefield,
            );
            let ability = ResolvedAbility::new(
                make_effect(),
                vec![TargetRef::Object(creature)],
                source,
                PlayerId(0),
            )
            .duration(Duration::Permanent);
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            assert!(
                !crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(0)),
                "creature redirect target must not lock its controller (CR 119.7)"
            );
            assert!(
                !crate::game::static_abilities::player_has_cant_gain_life(&state, PlayerId(1)),
                "creature redirect target must not lock its controller (CR 119.7)"
            );
        }
    }

    #[test]
    fn generic_effect_binds_tracked_set_sentinel_to_latest_chain_set() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let returned = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Returned Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .tracked_object_sets
            .insert(TrackedSetId(7), vec![returned]);
        state.chain_tracked_set_id = Some(TrackedSetId(7));

        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            })
            .modifications(vec![ContinuousModification::AddSubtype {
                subtype: "Vampire".to_string(),
            }]);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![static_def],
                duration: Some(Duration::Permanent),
                target: None,
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.transient_continuous_effects.len(), 1);
        let tce = &state.transient_continuous_effects[0];
        assert_eq!(tce.affected, TargetFilter::SpecificObject { id: returned });
        assert!(tce.modifications.iter().any(|modification| matches!(
            modification,
            ContinuousModification::AddSubtype { subtype } if subtype == "Vampire"
        )));
    }

    /// CR 305.6 + CR 305.7 + CR 611.2a: Energybending end-to-end. Parsing the
    /// full Oracle text and resolving the land-type clause against a Forest must
    /// give that Forest all five basic land subtypes AND the intrinsic mana
    /// ability for every color (CR 305.6). Drives the real parse → lower →
    /// resolve → layer pipeline: the `GenericEffect { AddAllBasicLandTypes }`
    /// over "lands you control" binds a transient continuous effect to the
    /// Forest, and `apply_intrinsic_basic_land_mana_abilities` grants the five
    /// `{T}: Add <color>` abilities during layer evaluation.
    ///
    /// Revert guard: without the parser change the land-type clause lowers to
    /// `Effect::Unimplemented`, no GenericEffect is found, and the subtype /
    /// per-color mana-ability assertions below all fail.
    #[test]
    fn energybending_grants_a_forest_all_basic_land_types_and_mana() {
        use crate::game::layers::evaluate_layers;
        use crate::parser::oracle::parse_oracle_text;
        use crate::types::ability::{AbilityCost, AbilityKind, BasicLandType, ManaProduction};
        use crate::types::mana::ManaColor;

        let parsed = parse_oracle_text(
            "Lands you control gain all basic land types until end of turn.\nDraw a card.",
            "Energybending",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        let land_type_effect = parsed
            .abilities
            .iter()
            .map(|ability| (*ability.effect).clone())
            .find(|effect| {
                matches!(
                    effect,
                    Effect::GenericEffect { static_abilities, .. }
                        if static_abilities.iter().any(|sd| sd
                            .modifications
                            .iter()
                            .any(|m| matches!(m, ContinuousModification::AddAllBasicLandTypes)))
                )
            })
            .expect("Energybending must lower to a GenericEffect adding all basic land types");

        let mut state = GameState::new_two_player(42);
        let p0 = PlayerId(0);

        // A single basic Forest under the spell controller's control.
        let forest = create_object(
            &mut state,
            CardId(0),
            p0,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let ts = state.next_timestamp();
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.base_card_types = obj.card_types.clone();
            obj.timestamp = ts;
        }

        let source = create_object(
            &mut state,
            CardId(1),
            p0,
            "Energybending".to_string(),
            Zone::Stack,
        );

        let ability = ResolvedAbility::new(land_type_effect, vec![], source, p0);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        let obj = state.objects.get(&forest).unwrap();
        for land_type in BasicLandType::all() {
            let subtype = land_type.as_subtype_str().to_string();
            assert!(
                obj.card_types.subtypes.contains(&subtype),
                "Forest must gain the {subtype} basic land type, got {:?}",
                obj.card_types.subtypes
            );
        }

        // CR 305.6: each basic land type grants its intrinsic `{T}: Add <color>`.
        for color in ManaColor::ALL {
            let count = obj
                .abilities
                .iter()
                .filter(|a| {
                    matches!(a.kind, AbilityKind::Activated)
                        && matches!(a.cost, Some(AbilityCost::Tap))
                        && matches!(
                            &*a.effect,
                            Effect::Mana {
                                produced: ManaProduction::Fixed { colors, .. },
                                ..
                            } if colors.as_slice() == [color]
                        )
                })
                .count();
            assert_eq!(
                count, 1,
                "Forest must produce {color:?} via its intrinsic mana ability after gaining all basic land types"
            );
        }
    }

    // ── Issue #444: in-effect "if" gate on GenericEffect StaticDefinitions ──

    use crate::game::layers::evaluate_layers;
    use crate::types::ability::{FilterProp, StaticCondition};
    use crate::types::keywords::Keyword as Kw;

    /// Build a creature on `player`'s battlefield, optionally with an intrinsic
    /// keyword.
    fn make_creature_with(
        state: &mut GameState,
        card: u64,
        player: PlayerId,
        name: &str,
        keyword: Option<Kw>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(card),
            player,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        if let Some(kw) = keyword {
            obj.base_keywords.push(kw.clone());
            obj.keywords.push(kw);
        }
        id
    }

    /// One conditioned `StaticDefinition` modeling an Odric grant arm: grant
    /// `keyword` to creatures you control, gated on a creature you control
    /// having that keyword.
    fn odric_grant_arm(keyword: Kw) -> StaticDefinition {
        StaticDefinition::continuous()
            .affected(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            ))
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: keyword.clone(),
            }])
            .condition(StaticCondition::IsPresent {
                filter: Some(TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::WithKeyword { value: keyword }]),
                )),
            })
    }

    /// CR 603.4 + CR 608.2h — DISCRIMINATING: a creature with flying but NOT
    /// first strike means the flying arm's gate is satisfied and the first
    /// strike arm's gate is not. Only the flying grant applies — observed via
    /// the keyword set after layers, not by TCE count (a satisfied broadcast
    /// grant registers one TCE per affected creature).
    #[test]
    fn odric_grant_applies_only_satisfied_keyword_arms() {
        let mut state = GameState::new_two_player(42);
        let odric = make_creature_with(&mut state, 1, PlayerId(0), "Odric", None);
        // A creature you control with flying, no first strike.
        let _flyer = make_creature_with(&mut state, 2, PlayerId(0), "Flyer", Some(Kw::Flying));

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![
                    odric_grant_arm(Kw::FirstStrike),
                    odric_grant_arm(Kw::Flying),
                ],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            odric,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // The first-strike arm's gate failed → no first-strike TCE; only the
        // flying arm registered. Every registered TCE must have its
        // resolution-time gate zeroed so layers never re-evaluate it.
        assert!(
            !state.transient_continuous_effects.is_empty(),
            "the satisfied flying arm must register at least one TCE"
        );
        assert!(
            state
                .transient_continuous_effects
                .iter()
                .all(|tce| tce.condition.is_none()),
            "resolution-time gate must be zeroed so layers never re-evaluate it"
        );
        assert!(
            state
                .transient_continuous_effects
                .iter()
                .all(|tce| tce.modifications.iter().all(|m| !matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::FirstStrike
                    }
                ))),
            "no first-strike grant should exist — its gate was not satisfied"
        );

        evaluate_layers(&mut state);
        let odric_obj = state.objects.get(&odric).unwrap();
        assert!(
            odric_obj.has_keyword(&Kw::Flying),
            "Odric must gain flying (a creature you control has flying)"
        );
        assert!(
            !odric_obj.has_keyword(&Kw::FirstStrike),
            "Odric must NOT gain first strike (no creature you control has first strike)"
        );
    }

    /// CR 611.2c + CR 608.2h — ONE-SHOT SNAPSHOT: a satisfied grant persists
    /// for its duration even after the creature that satisfied the gate loses
    /// the keyword. The grant's truth was determined once, at resolution.
    #[test]
    fn odric_grant_persists_after_gating_creature_loses_keyword() {
        let mut state = GameState::new_two_player(42);
        let odric = make_creature_with(&mut state, 1, PlayerId(0), "Odric", None);
        let striker =
            make_creature_with(&mut state, 2, PlayerId(0), "Striker", Some(Kw::FirstStrike));

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![odric_grant_arm(Kw::FirstStrike)],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            odric,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Grant applied — Odric has first strike while the gate creature still
        // has it.
        evaluate_layers(&mut state);
        assert!(
            state
                .objects
                .get(&odric)
                .unwrap()
                .has_keyword(&Kw::FirstStrike),
            "the first strike grant must apply at resolution"
        );

        // Remove first strike from the only first-striker.
        {
            let s = state.objects.get_mut(&striker).unwrap();
            s.base_keywords.clear();
            s.keywords.clear();
        }
        evaluate_layers(&mut state);

        // The grant persists — it was snapshotted at resolution (CR 611.2c).
        assert!(
            state
                .objects
                .get(&odric)
                .unwrap()
                .has_keyword(&Kw::FirstStrike),
            "the first strike grant must persist for its duration after the gate creature loses the keyword"
        );
    }

    // ── CR 611.2d: resolution-context quantity snapshot (Hammer Helper) ──

    /// CR 706.2 + CR 611.2d: an `EventContextAmount` leaf is snapshotted to the
    /// most recent amount-yielding event — the preceding `DieRolled`'s result.
    #[test]
    fn snapshot_replaces_event_context_amount_with_die_result() {
        let events = vec![
            GameEvent::DieRolled {
                player_id: PlayerId(0),
                sides: 6,
                result: Some(4),
            },
            GameEvent::EffectResolved {
                kind: EffectKind::RollDie,
                source_id: ObjectId(1),
            },
        ];
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        assert_eq!(
            snapshot_resolution_context_quantity(&expr, &events),
            QuantityExpr::Fixed { value: 4 },
        );
    }

    /// Persistent game-state refs must be left untouched — only resolution-
    /// context refs are snapshotted.
    #[test]
    fn snapshot_leaves_persistent_refs_unchanged() {
        let events = vec![GameEvent::DieRolled {
            player_id: PlayerId(0),
            sides: 6,
            result: Some(4),
        }];
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature()),
            },
        };
        assert_eq!(snapshot_resolution_context_quantity(&expr, &events), expr,);
    }

    /// GAP-3 silent-failure guard: an `EventContextAmount` leaf with no source
    /// event must trip the `debug_assert!` rather than silently snapshot to 0.
    #[test]
    #[should_panic(expected = "EventContextAmount leaf found no source event")]
    fn snapshot_panics_on_event_context_amount_without_source_event() {
        let events: Vec<GameEvent> = Vec::new();
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        };
        let _ = snapshot_resolution_context_quantity(&expr, &events);
    }

    /// CR 706.2 + CR 611.2d end-to-end: roll a die, then register a continuous
    /// `+X/+0` pump where X is the result (the Hammer Helper shape). The pump
    /// must snapshot to the rolled value at resolution and stay stable across
    /// later layer recomputes — never collapse to +0/+0, and never re-resolve
    /// against a later effect's published amount.
    ///
    /// The source object is itself a creature so the parsed GenericEffect's
    /// `affected: SelfRef` binds the pump to it — 07f flags the affected-
    /// subject misparse (the pump should target the gained-control creature)
    /// as a separate out-of-scope defect.
    #[test]
    fn die_result_pump_snapshots_to_roll_and_stays_stable() {
        let mut state = GameState::new_two_player(42);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Helped Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Chain: RollDie (six-sided) → GenericEffect (+X/+0, X = the result).
        let pump = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::AddDynamicPower {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
            }]);
        let generic = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![pump],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            creature,
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 6,
                results: vec![],
                modifier: None,
            },
            vec![],
            creature,
            PlayerId(0),
        )
        .sub_ability(generic);

        let mut events = Vec::new();
        crate::game::effects::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let roll = events
            .iter()
            .find_map(|e| match e {
                GameEvent::DieRolled { result, .. } => result.map(i32::from),
                _ => None,
            })
            .expect("RollDie must emit a DieRolled event");
        assert!((1..=6).contains(&roll), "d6 result out of range: {roll}");

        evaluate_layers(&mut state);
        let power_after = state.objects.get(&creature).unwrap().power;
        assert_eq!(
            power_after,
            Some(2 + roll),
            "pump must snapshot to the rolled result ({roll}); +0/+0 = silent-failure regression",
        );
        assert_ne!(
            power_after,
            Some(2),
            "a +0/+0 pump means the snapshot missed"
        );

        // Stability: a later layer recompute must not re-resolve the pump.
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&creature).unwrap().power,
            Some(2 + roll),
            "snapshotted pump must stay stable across layer recomputes",
        );

        // A later effect that publishes an amount must NOT bleed into the
        // pump: the snapshot froze it. An un-snapshotted `EventContextAmount`
        // would fall back to `last_effect_amount` and corrupt the pump.
        state.last_effect_amount = Some(99);
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects.get(&creature).unwrap().power,
            Some(2 + roll),
            "a later effect's amount must not bleed into the snapshotted pump",
        );
    }

    /// CR 603.4 — NEGATIVE: no creature has any of the gated keywords, so
    /// zero TCEs are registered.
    #[test]
    fn odric_grant_registers_nothing_when_no_keyword_present() {
        let mut state = GameState::new_two_player(42);
        let odric = make_creature_with(&mut state, 1, PlayerId(0), "Odric", None);
        let _vanilla = make_creature_with(&mut state, 2, PlayerId(0), "Bear", None);

        let ability = ResolvedAbility::new(
            Effect::GenericEffect {
                static_abilities: vec![
                    odric_grant_arm(Kw::FirstStrike),
                    odric_grant_arm(Kw::Flying),
                    odric_grant_arm(Kw::Trample),
                ],
                duration: Some(Duration::UntilEndOfTurn),
                target: None,
            },
            vec![],
            odric,
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfTurn);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            state.transient_continuous_effects.is_empty(),
            "no keyword present on any creature → zero TCEs registered"
        );
    }
}
