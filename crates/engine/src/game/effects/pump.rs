use crate::game::filter;
use crate::game::quantity::{
    quantity_expr_uses_recipient, resolve_quantity_with_targets,
    resolve_quantity_with_targets_and_recipient,
};
use crate::types::ability::{
    ContinuousModification, DoublePTMode, Duration, Effect, EffectError, EffectKind, PtValue,
    QuantityExpr, ResolvedAbility, TargetFilter,
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

    // CR 608.2h + CR 613.4c: the pump amount is determined once, as the effect
    // resolves. When the P/T references the pumped object itself ("+X for each
    // OTHER creature … that shares a type with it": FilterProp::Another /
    // AttachedToRecipient), the count MUST be resolved with THAT object bound as
    // the recipient so "other X" excludes the specific creature being pumped
    // rather than the ability source (the Plane in the command zone would
    // otherwise leave the attacker wrongly counted). Recipient-invariant
    // quantities keep the single shared resolution reused across all targets.
    let per_recipient = pt_uses_recipient(power) || pt_uses_recipient(toughness);
    let shared = (!per_recipient).then(|| pt_modifications(power, toughness, state, ability, None));

    for obj_id in ids {
        if !state.objects.contains_key(&obj_id) {
            return Err(EffectError::ObjectNotFound(obj_id));
        }
        let modifications = match &shared {
            Some(m) => m.clone(),
            None => pt_modifications(power, toughness, state, ability, Some(obj_id)),
        };
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
        subject: None,
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

    // CR 608.2h + CR 613.4c: same recipient-relative parity as `resolve` — an
    // eager PumpAll whose per-creature amount references the pumped object
    // ("each creature you control gets +1/+1 for each OTHER creature you control
    // that shares a type with it") resolves the count per recipient; otherwise
    // the shared single resolution is reused.
    let per_recipient = pt_uses_recipient(power) || pt_uses_recipient(toughness);
    let shared = (!per_recipient).then(|| pt_modifications(power, toughness, state, ability, None));

    // Collect matching object IDs first to avoid borrow conflicts.
    let matching = pump_all_affected_objects(state, ability, &target_filter);

    for obj_id in matching {
        let modifications = match &shared {
            Some(m) => m.clone(),
            None => pt_modifications(power, toughness, state, ability, Some(obj_id)),
        };
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
        subject: None,
    });

    Ok(())
}

/// CR 611.2c: the population `Effect::PumpAll` affects, fixed at the moment its
/// continuous effect begins and never changing afterwards.
///
/// SINGLE AUTHORITY: `resolve_all` installs the transient effects over exactly
/// this list, and `effects::affected_objects_from_events` publishes exactly this
/// list as the chain tracked set, so a later "Untap those creatures"
/// (CR 701.26b) binds the creatures actually pumped. Both call sites go through
/// this ONE enumeration function — never a second, hand-written scan at the
/// publish site. Be precise about what that does and does not buy: this
/// function IS handed the head filter and DOES re-run the scan against a later
/// `state`, so the two enumerations agree because they are the same code over
/// an unchanged board, not because the list was memoised.
///
/// FLUSH HAZARD — the load-bearing precondition. The board must not change
/// between `resolve_all` and the publish, and in particular the pump's own
/// modifications must not have MATERIALISED. They do not today:
/// `GameState::add_transient_continuous_effect` installs the effect and marks
/// the layer cache dirty, while materialisation happens in
/// `layers::flush_layers`. The precise claim — the falsifiable one is weaker
/// than it looks — is NOT that `flush_layers` is never called during
/// resolution: several effect resolvers call it (`amass`, `attach`,
/// `become_copy`, among others), and a reader who greps for it will find them.
/// It is that nothing flushes on the SPAN between this node's own
/// `resolve_effect` and this node's publish, because the publish site runs
/// immediately after that call with no other resolver in between. Introduce
/// a flush on that span and a filter that reads a modified characteristic
/// (power, toughness, controller, a granted type) would enumerate DIFFERENTLY
/// at the publish site than at resolution — the published set would silently
/// stop being the frozen population. If you add such a flush, snapshot this
/// list at resolution instead of re-scanning.
///
/// The unit test in this module pins the identity for a CONTROLLER filter,
/// which no pump modification can move, so it would NOT catch that regression.
/// A discriminating test for the flush hazard needs a filter reading a pumped
/// characteristic (e.g. "creatures with power 2 or less").
///
/// NOTE for a future zone-aware `resolve_all`: this scans the battlefield, so
/// Elvish Elegy's `InZone: Graveyard` filter returns `[]` today. That is NOT
/// what keeps its milled tracked set intact — the `is_sole_chain_producer`
/// guard at the publish site does, because the preceding `Mill` already
/// published. Making this zone-aware is therefore safe.
pub(crate) fn pump_all_affected_objects(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    // CR 608.2c: concretize contextual filters against this ability's inherited
    // target before the mass scan — e.g. `Not{ParentTarget}` from a "target X and
    // all other X with the same name get -N/-M" chain (Bile Blight) becomes
    // `Not{SpecificObject{target}}`, excluding the already-pumped target so it is
    // not shrunk twice. No-op for filters without contextual refs, mirroring the
    // single `resolve` and `destroy`/`bounce`.
    let target_filter = crate::game::effects::resolved_object_filter(ability, target);
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = filter::FilterContext::from_ability(ability);
    // CR 702.26b: a phased-out permanent "is treated as though it does not
    // exist". CR 702.26e is the specific rule for this producer: a continuous
    // effect from a resolving spell/ability that modifies characteristics does
    // NOT include phased-out permanents in its set of affected objects.
    //
    // Deliberately redundant with `filter_inner`'s own CR 702.26b choke point in
    // `filter.rs`, which already excludes phased-out objects from every
    // `matches_target_filter` call — measured (see the test's DISCRIMINATION
    // table), not assumed. Kept because this function is the single authority for
    // BOTH the pump and the published tracked set, so it holds the invariant
    // locally rather than inheriting it from a matcher whose own comment reserves
    // the right to be bypassed by "targeted callers". Enumerates exactly as the
    // sibling `goad_targets` authority does.
    state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| filter::matches_target_filter(state, *id, &target_filter, &ctx))
        .collect()
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
        subject: None,
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
        subject: None,
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

/// CR 608.2c: True when a `PtValue` is a dynamic quantity that reads the pumped
/// object itself ("+X for each OTHER creature …", "… attached to it") and so
/// must be resolved per recipient rather than against the ability source.
fn pt_uses_recipient(pt: &PtValue) -> bool {
    matches!(pt, PtValue::Quantity(expr) if quantity_expr_uses_recipient(expr))
}

/// Resolve a dynamic P/T quantity, binding `recipient` (the object being pumped)
/// when present so recipient-relative filters ("other", "shares … with it")
/// exclude that specific object; otherwise resolve against the ability source.
fn resolve_pt_quantity(
    expr: &QuantityExpr,
    state: &GameState,
    ability: &ResolvedAbility,
    recipient: Option<ObjectId>,
) -> i32 {
    match recipient {
        Some(id) => resolve_quantity_with_targets_and_recipient(state, expr, ability, id),
        None => resolve_quantity_with_targets(state, expr, ability),
    }
}

/// Build `ContinuousModification` entries for a P/T pump effect.
/// CR 608.2h: both fixed and dynamic quantities are snapshotted to fixed
/// `AddPower`/`AddToughness` (with the count resolved) as the effect resolves —
/// no `AddDynamic*` variant is emitted; the layer system applies the frozen
/// amount.
///
/// `recipient` binds the object being pumped for recipient-relative dynamic
/// quantities (see `pt_uses_recipient`); `None` uses source-relative resolution
/// shared across all pumped objects. Fixed / Variable(X) arms are
/// recipient-invariant, so passing `Some`/`None` is immaterial for them.
fn pt_modifications(
    power: &PtValue,
    toughness: &PtValue,
    state: &GameState,
    ability: &ResolvedAbility,
    recipient: Option<ObjectId>,
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
            let resolved = resolve_pt_quantity(expr, state, ability, recipient);
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
            let resolved = resolve_pt_quantity(expr, state, ability, recipient);
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
    use crate::game::game_object::{PhaseOutCause, PhaseStatus};
    use crate::game::layers::evaluate_layers;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, FilterProp, PtValue, QuantityRef, TargetFilter, TargetRef, TypedFilter,
    };
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

    /// CR 702.26b + CR 702.26e: a phased-out permanent "is treated as though it
    /// does not exist", and a continuous effect from a resolving spell/ability
    /// does NOT include it in its set of affected objects.
    ///
    /// Covers BOTH halves the review asked for with one assertion each, because
    /// `pump_all_affected_objects` is the single authority for the pump AND for
    /// the published tracked set (that identity is pinned by the sibling test
    /// below): excluding the phased-out creature from this population excludes
    /// it from the set a later "those creatures" consumer reads.
    ///
    /// DISCRIMINATION, measured as a 2x2 rather than claimed — two independent
    /// guards enforce this invariant, so reverting either ALONE leaves the test
    /// green, and only the conjunction is load-bearing:
    ///
    /// | `filter_inner` CR 702.26b choke point | this fn's `battlefield_phased_in_ids()` | result |
    /// |---|---|---|
    /// | on  | on  | pass |
    /// | on  | off | pass |
    /// | off | on  | pass |
    /// | off | off | **FAIL** — population comes back `[phased_in, phased_out]` |
    ///
    /// So this test does not prove the local enumeration is *necessary* today; it
    /// pins that the producer keeps excluding phased-out permanents if EITHER
    /// guard is later removed or routed around. The phased-IN creature's
    /// assertions are the paired non-vacuity witness: without them a helper that
    /// returned `[]` for everything would pass.
    #[test]
    fn pump_all_excludes_a_phased_out_creature_from_the_population_and_the_pump() {
        let mut state = GameState::new_two_player(7);
        let phased_in = make_creature(&mut state, "Phased In", 2, 2, PlayerId(0));
        let phased_out = make_creature(&mut state, "Phased Out", 2, 2, PlayerId(0));
        if let Some(obj) = state.objects.get_mut(&phased_out) {
            obj.phase_status = PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Directly,
            };
        }

        let yours: TargetFilter = TypedFilter::creature()
            .controller(ControllerRef::You)
            .into();
        let ability = ResolvedAbility::new(
            Effect::PumpAll {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: yours.clone(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        assert_eq!(
            pump_all_affected_objects(&state, &ability, &yours),
            vec![phased_in],
            "CR 702.26e: a phased-out permanent must not enter the frozen population, \
             which is also the set the publish site republishes"
        );

        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();
        // The pump installs a transient continuous effect; P/T only materializes
        // once the layer system evaluates (see this module's FLUSH HAZARD note).
        evaluate_layers(&mut state);
        assert_eq!(
            state.objects[&phased_in].power,
            Some(3),
            "non-vacuity: the phased-IN creature must actually be pumped, else the \
             exclusion above could pass on an empty population"
        );
        assert_eq!(
            state.objects[&phased_out].power,
            Some(2),
            "CR 702.26b: the phased-out creature must be untouched by the pump"
        );
    }

    /// CR 611.2c (issue #6857): `pump_all_affected_objects` is the SINGLE
    /// AUTHORITY for the population `resolve_all` freezes — the publish site
    /// republishes this exact list rather than re-deriving it.
    ///
    /// Two properties, both parameter-level rather than card-level:
    ///  * the helper's list IS the pumped set, for whatever filter it is handed
    ///    (identity between publisher and producer);
    ///  * the filter is honoured, and a BROADER filter genuinely widens it — so
    ///    a `target: Any` head (the mass-pump shape whose head filter names the
    ///    whole battlefield) proves why the publish site may not substitute its
    ///    own re-enumeration for the resolver's.
    #[test]
    fn pump_all_affected_objects_is_the_filtered_population_that_resolve_all_pumps() {
        let mut state = GameState::new_two_player(42);
        let mine_a = make_creature(&mut state, "Mine A", 2, 2, PlayerId(0));
        let mine_b = make_creature(&mut state, "Mine B", 1, 1, PlayerId(0));
        let theirs = make_creature(&mut state, "Theirs", 3, 3, PlayerId(1));

        let yours: TargetFilter = TypedFilter::creature()
            .controller(ControllerRef::You)
            .into();
        let ability = ResolvedAbility::new(
            Effect::PumpAll {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                target: yours.clone(),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut affected = pump_all_affected_objects(&state, &ability, &yours);
        affected.sort();
        let mut expected = vec![mine_a, mine_b];
        expected.sort();
        assert_eq!(
            affected, expected,
            "the controller filter must survive: the opponent's creature is not in the frozen population"
        );

        // The same filter under a broader head names strictly more — the
        // enumeration is parameterized, not a fixed battlefield scan.
        let mut broad = pump_all_affected_objects(&state, &ability, &TargetFilter::Any);
        broad.sort();
        let mut all = vec![mine_a, mine_b, theirs];
        all.sort();
        assert_eq!(broad, all);
        assert_ne!(
            affected, broad,
            "if a broad filter returned the same list, the filter argument would be inert \
             and this test could not detect a publish-site re-enumeration"
        );

        // Identity with the producer: exactly the helper's list gets pumped.
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);
        assert_eq!(state.objects[&mine_a].power, Some(3));
        assert_eq!(state.objects[&mine_b].power, Some(2));
        assert_eq!(state.objects[&theirs].power, Some(3), "not pumped");
    }

    /// Issue #4727 (CR 611.2a): "Target creature and all other creatures with the
    /// same name as that creature get -N/-M" (Bile Blight). The mass
    /// `PumpAll{ And[ SameNameAsParentTarget, Not{ParentTarget} ] }` sub-ability
    /// must shrink OTHER same-name creatures but EXCLUDE the announced target
    /// (which the paired single `Pump` already covers) so it is not shrunk twice.
    /// Guards the `resolved_object_filter` line in `resolve_all`: without it,
    /// `Not{ParentTarget}` never concretizes and the target is wrongly caught.
    #[test]
    fn pump_all_same_name_excludes_parent_target() {
        let mut state = GameState::new_two_player(42);
        let target = make_creature(&mut state, "Grizzly Bears", 5, 5, PlayerId(0));
        let same_name = make_creature(&mut state, "Grizzly Bears", 5, 5, PlayerId(0));
        let other = make_creature(&mut state, "Runeclaw Bear", 5, 5, PlayerId(0));

        let ability = ResolvedAbility::new(
            Effect::PumpAll {
                power: PtValue::Fixed(-3),
                toughness: PtValue::Fixed(-3),
                target: TargetFilter::And {
                    filters: vec![
                        TypedFilter::creature()
                            .properties(vec![FilterProp::SameNameAsParentTarget])
                            .into(),
                        TargetFilter::Not {
                            filter: Box::new(TargetFilter::ParentTarget),
                        },
                    ],
                },
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        // Announced target is EXCLUDED from the mass debuff (Not{ParentTarget}).
        assert_eq!(state.objects[&target].power, Some(5));
        assert_eq!(state.objects[&target].toughness, Some(5));
        // Another creature sharing the name IS shrunk -3/-3.
        assert_eq!(state.objects[&same_name].power, Some(2));
        assert_eq!(state.objects[&same_name].toughness, Some(2));
        // Differently-named creature is unaffected.
        assert_eq!(state.objects[&other].power, Some(5));
        assert_eq!(state.objects[&other].toughness, Some(5));
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

    /// CR 608.2h + CR 613.4c (Mondassian Colony Ship / Shared Animosity class):
    /// an eager `Effect::Pump` whose amount counts "other creature[s] its
    /// controller controls" must exclude the SPECIFIC creature being pumped, not
    /// the ability source. Here the attacker + 2 allies are all controlled by
    /// P0; the amount is "+N/+N for each OTHER creature you control"
    /// (`FilterProp::Another`). Bound to the attacker as recipient, `Another`
    /// excludes the attacker → counts the 2 allies → +2/+2 → 4/4. Before Fix 2
    /// the count resolved against the source (recipient = None), so `Another`
    /// excluded the off-battlefield source instead and the attacker was wrongly
    /// counted → 5/5. The 4-vs-5 gap is the fail-on-revert discriminator.
    #[test]
    fn pump_for_each_other_creature_excludes_pumped_recipient() {
        let mut state = GameState::new_two_player(42);
        let attacker = make_creature(&mut state, "Attacker", 2, 2, PlayerId(0));
        let _ally1 = make_creature(&mut state, "Ally 1", 1, 1, PlayerId(0));
        let _ally2 = make_creature(&mut state, "Ally 2", 1, 1, PlayerId(0));
        // Opponent creature — excluded by `controller: You`, never counted.
        let _opp = make_creature(&mut state, "Opp", 3, 3, PlayerId(1));

        let count = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another]),
                ),
            },
        };
        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Quantity(count.clone()),
                toughness: PtValue::Quantity(count),
                target: TargetFilter::Any,
            },
            // The pumped creature is the attacker (the trigger's TriggeringSource).
            vec![TargetRef::Object(attacker)],
            // Source is the Plane — deliberately NOT on the battlefield, so a
            // source-relative `Another` would exclude nothing and overcount.
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&attacker].power,
            Some(4),
            "attacker gains +2 from the 2 OTHER creatures its controller controls, not +3"
        );
        assert_eq!(state.objects[&attacker].toughness, Some(4));
    }

    /// Fix 2 regression: a dynamic pump whose count does NOT reference the pumped
    /// object ("+N/+0 for each creature you control", no "other") keeps the single
    /// shared resolution and counts the recipient itself. Confirms
    /// `pt_uses_recipient` gates to `false` here and the shared-path amount is
    /// unchanged — the pumped creature is included (3 = target + 2 allies).
    #[test]
    fn pump_for_each_creature_shared_path_counts_self() {
        let mut state = GameState::new_two_player(42);
        let target = make_creature(&mut state, "Target", 2, 2, PlayerId(0));
        let _ally1 = make_creature(&mut state, "Ally 1", 1, 1, PlayerId(0));
        let _ally2 = make_creature(&mut state, "Ally 2", 1, 1, PlayerId(0));

        let count = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        };
        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Quantity(count),
                toughness: PtValue::Fixed(0),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        assert_eq!(
            state.objects[&target].power,
            Some(5),
            "no \"other\": all 3 creatures you control counted, self included"
        );
        assert_eq!(state.objects[&target].toughness, Some(2));
    }

    /// CR 608.2h + CR 613.4c (Shared Animosity class via `Effect::PumpAll`):
    /// "each creature you control gets +N/+N for each OTHER creature you
    /// control". The per-creature amount references the pumped object
    /// (`FilterProp::Another`), so `resolve_all` must resolve the count PER
    /// recipient — each of the 3 creatures excludes itself and counts the 2
    /// others → +2/+2. The source is deliberately off-battlefield (`ObjectId`
    /// 500), so if the per-recipient gating regressed to the shared source-
    /// relative path, `Another` would exclude nothing on the battlefield and
    /// every creature would count all 3 → +3/+3. Distinct base P/T (2/3/4)
    /// confirms the frozen amount is applied independently to each recipient:
    /// each ends exactly +2 above its base (4/5/6), never +3 (5/6/7).
    #[test]
    fn pump_all_for_each_other_creature_excludes_each_recipient() {
        let mut state = GameState::new_two_player(42);
        let c1 = make_creature(&mut state, "C1", 2, 2, PlayerId(0));
        let c2 = make_creature(&mut state, "C2", 3, 3, PlayerId(0));
        let c3 = make_creature(&mut state, "C3", 4, 4, PlayerId(0));
        // Opponent creature — excluded by `controller: You`, never counted/pumped.
        let opp = make_creature(&mut state, "Opp", 3, 3, PlayerId(1));

        let count = QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another]),
                ),
            },
        };
        let ability = ResolvedAbility::new(
            Effect::PumpAll {
                power: PtValue::Quantity(count.clone()),
                toughness: PtValue::Quantity(count),
                target: TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .into(),
            },
            vec![],
            // Source off the battlefield — a source-relative `Another` would
            // exclude nothing and overcount to +3/+3.
            ObjectId(500),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve_all(&mut state, &ability, &mut events).unwrap();
        evaluate_layers(&mut state);

        // Each recipient excludes itself → counts the 2 others → +2/+2.
        assert_eq!(state.objects[&c1].power, Some(4), "C1 base 2 +2 others");
        assert_eq!(state.objects[&c1].toughness, Some(4));
        assert_eq!(state.objects[&c2].power, Some(5), "C2 base 3 +2 others");
        assert_eq!(state.objects[&c2].toughness, Some(5));
        assert_eq!(state.objects[&c3].power, Some(6), "C3 base 4 +2 others");
        assert_eq!(state.objects[&c3].toughness, Some(6));
        // Opponent creature untouched.
        assert_eq!(state.objects[&opp].power, Some(3));
        assert_eq!(state.objects[&opp].toughness, Some(3));
    }
}
