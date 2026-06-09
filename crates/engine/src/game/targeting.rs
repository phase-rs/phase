use crate::types::ability::{
    ControllerRef, FilterProp, ResolvedAbility, TargetFilter, TargetRef, TypeFilter, TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, StackEntry, StackEntryKind};
use crate::types::identifiers::{ObjectId, TrackedSetId};
use crate::types::keywords::{HexproofFilter, Keyword};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;
use std::collections::HashSet;

/// Find legal targets using a typed TargetFilter (CR 115.2 + CR 702.16b).
///
/// Evaluates battlefield objects against the filter using the typed filter system,
/// and includes players/stack spells where appropriate.
pub fn find_legal_targets(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    let target_ctx =
        super::filter::FilterContext::from_source_with_controller(source_id, source_controller);
    find_legal_targets_with_context(state, filter, source_controller, source_id, &target_ctx)
}

pub(crate) fn find_legal_targets_for_ability(
    state: &GameState,
    filter: &TargetFilter,
    ability: &ResolvedAbility,
) -> Vec<TargetRef> {
    let target_ctx = super::filter::FilterContext::from_ability(ability);
    find_legal_targets_with_context(
        state,
        filter,
        ability.controller,
        ability.source_id,
        &target_ctx,
    )
}

pub(crate) fn find_legal_targets_for_ability_with_controller(
    state: &GameState,
    filter: &TargetFilter,
    ability: &ResolvedAbility,
    source_controller: PlayerId,
) -> Vec<TargetRef> {
    let target_ctx =
        super::filter::FilterContext::from_ability_with_controller(ability, source_controller);
    find_legal_targets_with_context(
        state,
        filter,
        source_controller,
        ability.source_id,
        &target_ctx,
    )
}

fn find_legal_targets_with_context(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
    target_ctx: &super::filter::FilterContext,
) -> Vec<TargetRef> {
    let mut targets = Vec::new();

    // SpecificObject is runtime-bound (not used for target selection)
    if matches!(filter, TargetFilter::SpecificObject { .. }) {
        return targets;
    }

    // ParentTarget inherits targets from the parent ability at resolution time.
    // No new targeting needed — the sub_ability chain copies parent targets automatically.
    if matches!(filter, TargetFilter::ParentTarget) {
        return targets;
    }

    if let TargetFilter::Or { filters } = filter {
        let mut seen = HashSet::new();
        for branch in filters {
            for target in find_legal_targets_with_context(
                state,
                branch,
                source_controller,
                source_id,
                target_ctx,
            ) {
                if seen.insert(target.clone()) {
                    targets.push(target);
                }
            }
        }
        return targets;
    }

    // StackAbility: only match non-mana activated/triggered abilities on the stack.
    if filter_targets_stack_abilities(filter) {
        add_stack_abilities(state, filter, source_controller, source_id, &mut targets);
        return targets;
    }

    if matches!(filter, TargetFilter::AttachedTo) {
        if let Some(target) = resolve_event_context_target(state, filter, source_id) {
            targets.push(target);
        }
        return targets;
    }

    // The "any other target" shape: `Typed { type_filters: [], controller: None,
    // properties: [Another] }`. Per CR 115.4 ("any target"/"another target" may
    // be a creature, player, planeswalker, or battle), this is an any-target
    // filter with the source object excluded — NOT the player-only shape the
    // empty-`type_filters` branch below handles. Enumerate it like
    // `TargetFilter::Any` (players + battlefield objects, matching the engine's
    // existing `Any` breadth) but exclude the source; the object loop's
    // `matches_target_filter` honors `FilterProp::Another` (CR 109.1) to drop the
    // source. This is what lets Screaming Nemesis redirect "to any other target"
    // hit a creature, not just a player.
    let is_any_other_target = matches!(
        filter,
        TargetFilter::Typed(tf)
            if tf.type_filters.is_empty()
                && tf.controller.is_none()
                && tf.properties.iter().any(|p| matches!(p, FilterProp::Another))
    );

    // Check if filter could match players
    if matches!(filter, TargetFilter::Any | TargetFilter::Player) || is_any_other_target {
        add_players(state, &mut targets, source_id);
    }

    if let TargetFilter::SpecificPlayer { id } = filter {
        add_specific_player(state, &mut targets, *id, source_id);
        return targets;
    }

    // Typed filter with no type_filters targets players, not permanents.
    // e.g. "target opponent" → Typed { type_filters: [], controller: Opponent }
    // The "any other target" shape (handled above as `is_any_other_target`) is
    // the sole exception: it adds players above and falls through to the object
    // enumeration below instead of collapsing to players-only here.
    if let TargetFilter::Typed(ref tf) = filter {
        if tf.type_filters.is_empty() && !is_any_other_target {
            let controller = &tf.controller;
            for player in &state.players {
                // Player-phasing exclusion (mirrors CR 702.26b for permanents).
                if player.is_phased_out() {
                    continue;
                }
                // CR 800.4a: Eliminated players are not legal targets.
                if player.is_eliminated {
                    continue;
                }
                // CR 702.16b + CR 702.16j: A player with protection from the
                // spell/ability's source can't be targeted by it.
                if super::static_abilities::player_protection_from(
                    state,
                    player.id,
                    Some(source_id),
                ) {
                    continue;
                }
                let include = match controller {
                    Some(ControllerRef::Opponent) => {
                        super::players::is_opponent(state, source_controller, player.id)
                    }
                    Some(ControllerRef::You) => player.id == source_controller,
                    // CR 109.4: TargetPlayer is nonsensical when enumerating target
                    // candidates (the "target player" is what's being chosen here).
                    // Fail closed.
                    Some(ControllerRef::ScopedPlayer) => false,
                    Some(ControllerRef::TargetPlayer) => false,
                    Some(ControllerRef::ParentTargetController) => false,
                    Some(ControllerRef::DefendingPlayer) => false,
                    // CR 613.1: a persisted chosen player isn't a target
                    // candidate here. Fail closed.
                    Some(ControllerRef::SourceChosenPlayer) => false,
                    // CR 109.4: A chosen player is fixed during resolution, not
                    // enumerated as a target candidate. Fail closed.
                    Some(ControllerRef::ChosenPlayer { .. }) => false,
                    // CR 603.2 + CR 109.4: The triggering player is fixed by
                    // the event, not enumerated as a target candidate. Fail closed.
                    Some(ControllerRef::TriggeringPlayer) => false,
                    None => true,
                };
                if include {
                    targets.push(TargetRef::Player(player.id));
                }
            }
            return targets;
        }
    }

    let explicit_zones = extract_explicit_zones(filter);

    if !explicit_zones.is_empty() {
        // Explicit zone search: ONLY search the specified zones
        for zone in &explicit_zones {
            match zone {
                Zone::Battlefield => {
                    for &obj_id in &state.battlefield {
                        if super::filter::matches_target_filter(state, obj_id, filter, target_ctx) {
                            let obj = match state.objects.get(&obj_id) {
                                Some(o) => o,
                                None => continue,
                            };
                            if can_target(obj, source_controller, source_id, state) {
                                targets.push(TargetRef::Object(obj_id));
                            }
                        }
                    }
                }
                Zone::Exile => add_zone_targets(
                    state,
                    state.exile.iter().copied(),
                    filter,
                    target_ctx,
                    false,
                    &mut targets,
                ),
                Zone::Graveyard => {
                    for player in &state.players {
                        add_zone_targets(
                            state,
                            player.graveyard.iter().copied(),
                            filter,
                            target_ctx,
                            false,
                            &mut targets,
                        );
                    }
                }
                Zone::Hand => {
                    for player in &state.players {
                        add_zone_targets(
                            state,
                            player.hand.iter().copied(),
                            filter,
                            target_ctx,
                            false,
                            &mut targets,
                        );
                    }
                }
                Zone::Library => {
                    for player in &state.players {
                        add_zone_targets(
                            state,
                            player.library.iter().copied(),
                            filter,
                            target_ctx,
                            false,
                            &mut targets,
                        );
                    }
                }
                Zone::Stack => {
                    for entry in &state.stack {
                        let obj_id = entry.id;
                        if stack_entry_matches_filter_with_context(
                            state,
                            entry,
                            filter,
                            source_controller,
                            source_id,
                            target_ctx,
                        ) {
                            let obj = match state.objects.get(&obj_id) {
                                Some(o) => o,
                                None => continue,
                            };
                            if !is_protected_from(obj, source_id, state) {
                                targets.push(TargetRef::Object(obj_id));
                            }
                        }
                    }
                }
                Zone::Command => {}
            }
        }
    } else {
        // No explicit zone: default behavior (battlefield + stack for Card type)
        if filter_targets_stack_spells(filter) {
            add_stack_spells(
                state,
                filter,
                source_controller,
                source_id,
                target_ctx,
                &mut targets,
            );
        }

        for &obj_id in &state.battlefield {
            if super::filter::matches_target_filter(state, obj_id, filter, target_ctx) {
                let obj = match state.objects.get(&obj_id) {
                    Some(o) => o,
                    None => continue,
                };
                if can_target(obj, source_controller, source_id, state) {
                    targets.push(TargetRef::Object(obj_id));
                }
            }
        }
    }

    targets
}

/// Recheck targets on resolution using typed filter, returns only still-legal targets.
pub fn validate_targets(
    state: &GameState,
    targets: &[TargetRef],
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    let legal = find_legal_targets(state, filter, source_controller, source_id);
    validate_targets_against_legal(targets, legal)
}

pub(crate) fn validate_targets_for_ability(
    state: &GameState,
    targets: &[TargetRef],
    filter: &TargetFilter,
    ability: &ResolvedAbility,
) -> Vec<TargetRef> {
    let legal = find_legal_targets_for_ability(state, filter, ability);
    validate_targets_against_legal(targets, legal)
}

fn validate_targets_against_legal(targets: &[TargetRef], legal: Vec<TargetRef>) -> Vec<TargetRef> {
    if legal.len() <= 8 {
        targets
            .iter()
            .filter(|t| legal.contains(t))
            .cloned()
            .collect()
    } else {
        let legal_set: HashSet<TargetRef> = legal.into_iter().collect();
        targets
            .iter()
            .filter(|t| legal_set.contains(*t))
            .cloned()
            .collect()
    }
}

/// Returns true if ALL original targets are now illegal (spell fizzles per CR 608.2b).
pub fn check_fizzle(original_targets: &[TargetRef], legal_targets: &[TargetRef]) -> bool {
    if original_targets.is_empty() {
        return false; // Spells with no targets never fizzle
    }
    legal_targets.is_empty()
}

/// Resolve event-context TargetFilter variants using the current trigger event.
/// These variants auto-resolve at effect resolution time from `state.current_trigger_event`
/// without requiring player selection (CR 603.2).
///
/// Returns `Some(TargetRef)` if the event context can provide a target,
/// `None` if the filter is not an event-context variant or no event is available.
pub fn resolve_event_context_target(
    state: &GameState,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> Option<TargetRef> {
    match filter {
        TargetFilter::DefendingPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget => {
            resolve_event_context_target_for_event_or_state(state, filter, source_id, None)
        }
        // CR 108.3 + CR 608.2c: `ParentTargetOwner` may fall back to the source's
        // AttachedTo host (Enslave's "enchanted creature deals 1 damage to its
        // owner" — phase trigger has no event source). Allow the no-event path so
        // the AttachedTo branch in the inner resolver runs even when no trigger
        // event is active.
        TargetFilter::ParentTargetOwner => {
            let event = state.current_trigger_event.as_ref();
            resolve_event_context_target_for_event_or_state(state, filter, source_id, event)
        }
        _ => {
            let event = state.current_trigger_event.as_ref()?;
            resolve_event_context_target_for_event_or_state(state, filter, source_id, Some(event))
        }
    }
}

/// Resolve all targets supplied by the current trigger event batch.
///
/// Singular event-context callers should use `resolve_event_context_target`; this
/// plural form is for filters whose semantics can compare against any object in
/// a simultaneous trigger batch, such as `SharesQuality`.
pub fn resolve_event_context_targets(
    state: &GameState,
    filter: &TargetFilter,
    source_id: ObjectId,
) -> Vec<TargetRef> {
    if state.current_trigger_events.is_empty() {
        return resolve_event_context_target(state, filter, source_id)
            .into_iter()
            .collect();
    }

    let mut seen = HashSet::new();
    state
        .current_trigger_events
        .iter()
        .filter_map(|event| {
            resolve_event_context_target_for_event_or_state(state, filter, source_id, Some(event))
        })
        .filter(|target| seen.insert(target.clone()))
        .collect()
}

/// CR 608.2c + CR 603.10a: Resolve the effective targets for a resolving
/// ability across the three Oracle-text target sources, in priority order:
///
/// 1. **Self-reference**: `TargetFilter::SelfRef` always resolves to the
///    source object itself, regardless of `ability.targets`. This is the
///    parser's `~` anaphor — "Exile Treasured Find", "Sacrifice Arc Blade",
///    "When ~ enters, ..." — and it is semantically distinct from the
///    parent's chosen target. When a chained sub-ability's filter is
///    `SelfRef`, the chain target propagation in `effects::mod.rs` may have
///    injected the parent's targets into `ability.targets`; the `SelfRef`
///    semantic must override that injection (issue #323 — Treasured Find's
///    "Exile ~" was self-exiling whichever object the parent bounce had
///    targeted instead of the spell itself).
/// 2. **None / ParentTarget fallback**: when these filters appear and
///    `ability.targets` is empty, the subject is the source object (the
///    "it" anaphor on top-level LTB triggers — Rancor, Spirit Loop). When
///    `ability.targets` is non-empty, `ParentTarget` semantically inherits
///    the parent's chosen targets, so fall through to tier 3.
/// 3. **Event context**: filters like `TriggeringSource`, `DefendingPlayer`,
///    `AttachedTo` resolve from `state.current_trigger_event` without
///    requiring player selection (CR 603.7c).
/// 4. **Pre-selected targets**: the ability's chosen targets from CR 601.2c
///    casting / CR 603.3d trigger placement.
///
/// Returns the targets from the first non-empty tier, owning the result so
/// callers don't need to branch over which tier resolved.
pub fn resolved_targets(
    ability: &ResolvedAbility,
    target_filter: &TargetFilter,
    state: &GameState,
) -> Vec<TargetRef> {
    // CR 608.2c: SelfRef is the printed-name anaphor (`~`) — its referent is
    // the source object itself, never a chosen target. Must short-circuit
    // before the `ability.targets` fallback so chained "Exile ~" sub-abilities
    // don't accidentally inherit the parent's targets via the chain target
    // propagation in `effects::mod.rs::resolve_chain`.
    if matches!(target_filter, TargetFilter::SelfRef) {
        return vec![TargetRef::Object(ability.source_id)];
    }
    if matches!(target_filter, TargetFilter::SourceOrPaired) {
        return state
            .objects
            .get(&ability.source_id)
            .and_then(|source| source.paired_with)
            .map(|partner| {
                vec![
                    TargetRef::Object(ability.source_id),
                    TargetRef::Object(partner),
                ]
            })
            .unwrap_or_default();
    }
    // CR 608.2k: "the exiled/sacrificed/discarded <noun>" — an untargeted
    // reference to the object referred to by this ability's cost. Resolved
    // from the recursively-stamped `cost_paid_object`. Mirrors the local
    // resolution `token_copy.rs` already performs for `CopyTokenOf`; this is
    // the general chokepoint for every effect that targets a cost-paid object.
    if matches!(target_filter, TargetFilter::CostPaidObject) {
        return ability
            .cost_paid_object
            .iter()
            .map(|snap| TargetRef::Object(snap.object_id))
            .collect();
    }
    if matches!(target_filter, TargetFilter::ParentTarget) && ability.targets.is_empty() {
        if let Some(target) = resolve_event_context_target(state, target_filter, ability.source_id)
        {
            return vec![target];
        }
    }
    let use_self = matches!(
        target_filter,
        TargetFilter::None | TargetFilter::ParentTarget
    ) && ability.targets.is_empty();
    if use_self {
        return vec![TargetRef::Object(ability.source_id)];
    }
    if let Some(target) = resolve_event_context_target(state, target_filter, ability.source_id) {
        return vec![target];
    }
    ability.targets.clone()
}

/// Resolve a `TargetFilter` to object ids for effects that operate over every
/// object in the resolved set rather than a single target slot.
pub(crate) fn resolved_object_ids_for_filter(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    match filter {
        TargetFilter::SelfRef => vec![ability.source_id],
        TargetFilter::ParentTarget => object_targets(&ability.targets).collect(),
        TargetFilter::ParentTargetSlot { index } => ability
            .targets
            .get(*index)
            .and_then(target_ref_object)
            .into_iter()
            .collect(),
        TargetFilter::LastCreated => state.last_created_token_ids.clone(),
        TargetFilter::TriggeringSource | TargetFilter::AttachedTo => {
            resolve_event_context_target(state, filter, ability.source_id)
                .and_then(|target| target_ref_object(&target))
                .into_iter()
                .collect()
        }
        TargetFilter::TrackedSet { .. } | TargetFilter::TrackedSetFiltered { .. } => {
            let effective_filter = resolve_tracked_set_sentinel(state, filter.clone());
            let ctx = super::filter::FilterContext::from_ability(ability);
            state
                .battlefield
                .iter()
                .copied()
                .filter(|id| {
                    super::filter::matches_target_filter(state, *id, &effective_filter, &ctx)
                })
                .collect()
        }
        TargetFilter::Any | TargetFilter::None | TargetFilter::Player => {
            object_targets(&ability.targets).collect()
        }
        _ => {
            let ctx = super::filter::FilterContext::from_ability(ability);
            let explicit_targets: Vec<ObjectId> = object_targets(&ability.targets)
                .filter(|id| super::filter::matches_target_filter(state, *id, filter, &ctx))
                .collect();
            if !explicit_targets.is_empty() {
                return explicit_targets;
            }

            state
                .battlefield
                .iter()
                .copied()
                .filter(|id| super::filter::matches_target_filter(state, *id, filter, &ctx))
                .collect()
        }
    }
}

fn object_targets(targets: &[TargetRef]) -> impl Iterator<Item = ObjectId> + '_ {
    targets.iter().filter_map(target_ref_object)
}

fn target_ref_object(target: &TargetRef) -> Option<ObjectId> {
    match target {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    }
}

pub(crate) fn resolve_event_context_target_for_event_or_state(
    state: &GameState,
    filter: &TargetFilter,
    source_id: ObjectId,
    event: Option<&GameEvent>,
) -> Option<TargetRef> {
    match filter {
        TargetFilter::TriggeringSpellController => {
            let event = event?;
            let source_obj_id = extract_source_from_event(event)?;
            let controller = state.objects.get(&source_obj_id)?.controller;
            Some(TargetRef::Player(controller))
        }
        TargetFilter::TriggeringSpellOwner => {
            let event = event?;
            let source_obj_id = extract_source_from_event(event)?;
            let owner = state.objects.get(&source_obj_id)?.owner;
            Some(TargetRef::Player(owner))
        }
        TargetFilter::TriggeringPlayer => {
            let event = event?;
            let player = extract_player_from_event(event, state)?;
            Some(TargetRef::Player(player))
        }
        TargetFilter::TriggeringSource => {
            let event = event?;
            let obj_id = extract_source_from_event(event)?;
            Some(TargetRef::Object(obj_id))
        }
        TargetFilter::ParentTarget => {
            let event = event?;
            blocked_attacker_from_event(event, source_id).map(TargetRef::Object)
        }
        TargetFilter::StackSpell => {
            let event = event?;
            // CR 601.2i + CR 603.2: On a spell-cast trigger, "that spell" /
            // "copy it" (Mendicant Core, Guidelight) is the spell that caused
            // the trigger, not an intervening triggered ability above it.
            extract_source_from_event(event).map(TargetRef::Object)
        }
        // CR 506.3d: "defending player" — look up from combat state using the source creature.
        TargetFilter::DefendingPlayer => {
            let combat = state.combat.as_ref()?;
            let attacker_info = combat.attackers.iter().find(|a| a.object_id == source_id)?;
            Some(TargetRef::Player(attacker_info.defending_player))
        }
        TargetFilter::AttachedTo => {
            let host = state.objects.get(&source_id)?.attached_to?;
            match host {
                crate::game::game_object::AttachTarget::Object(id) => Some(TargetRef::Object(id)),
                crate::game::game_object::AttachTarget::Player(player) => {
                    Some(TargetRef::Player(player))
                }
            }
        }
        TargetFilter::ParentTargetController => {
            let event = event?;
            let source_obj_id = extract_source_from_event(event)?;
            let controller = state.objects.get(&source_obj_id)?.controller;
            Some(TargetRef::Player(controller))
        }
        // CR 108.3 + CR 608.2c: `ParentTargetOwner` mirrors `ParentTargetController`
        // but returns the *owner* of the resolved object. When no trigger event
        // supplies a source object (Enslave's phase trigger), fall back to the
        // ability source's AttachedTo host — the Aura/Equipment context where
        // "its owner" anaphorically refers to the equipped/enchanted permanent.
        TargetFilter::ParentTargetOwner => {
            if let Some(event) = event {
                if let Some(source_obj_id) = extract_source_from_event(event) {
                    if let Some(owner) = state.objects.get(&source_obj_id).map(|o| o.owner) {
                        return Some(TargetRef::Player(owner));
                    }
                }
            }
            // CR 301.5 + CR 303.4: Aura/Equipment fallback — the source's
            // attached host is the implicit "it" subject of the sentence.
            let host = state.objects.get(&source_id)?.attached_to?;
            match host {
                crate::game::game_object::AttachTarget::Object(id) => state
                    .objects
                    .get(&id)
                    .map(|obj| TargetRef::Player(obj.owner)),
                crate::game::game_object::AttachTarget::Player(player) => {
                    Some(TargetRef::Player(player))
                }
            }
        }
        // CR 615.5 + CR 609.7: "the source's controller" / "that source's
        // controller" inside a prevention follow-up resolves to the controller
        // of the prevented event's damage source. Stashed by the prevention
        // applier at `replacement.rs:Prevented`; read here during follow-up
        // resolution. Returns `None` if invoked outside the post-replacement
        // window — caller should never reach this filter from elsewhere.
        TargetFilter::PostReplacementSourceController => {
            let source_obj_id = state.post_replacement_event_source?;
            let controller = state.objects.get(&source_obj_id)?.controller;
            Some(TargetRef::Player(controller))
        }
        TargetFilter::PostReplacementDamageTarget => state.post_replacement_event_target.clone(),
        _ => None,
    }
}

fn blocked_attacker_from_event(
    event: &crate::types::events::GameEvent,
    source_id: ObjectId,
) -> Option<ObjectId> {
    let crate::types::events::GameEvent::BlockersDeclared { assignments } = event else {
        return None;
    };
    let mut attackers = assignments
        .iter()
        .filter_map(|(blocker, attacker)| (*blocker == source_id).then_some(*attacker));
    let first = attackers.next()?;
    attackers.all(|attacker| attacker == first).then_some(first)
}

/// Resolve a player reference carried in an effect target slot.
///
/// `TargetFilter::ParentTargetController` first consults the resolving
/// ability's inherited targets, which is the spell-resolution path for
/// "target spell unless its controller pays". It then checks the stack by
/// target id/source id before falling back to event-context refs.
pub fn resolve_effect_player_ref(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<PlayerId> {
    match filter {
        // CR 109.5: "you" in an ability is its controller, independent of any
        // resolution-scoped player. Player-scope iteration rebinds
        // `ability.controller` to the scoped player (effects/mod.rs), so reading
        // `controller` already yields the per-iteration player there. Reading
        // `scoped_player` here instead conflated the two whenever a path set
        // `scoped_player` WITHOUT rebinding `controller` — most visibly a
        // villainous choice (CR 701.55a), where the chooser is bound as
        // `scoped_player` but a "you …" branch's controller must stay the
        // source's controller. Mirror the sibling resolver
        // `effects::resolve_player_for_context_ref`, which resolves `Controller`
        // straight to `ability.controller`.
        TargetFilter::Controller => Some(ability.controller),
        // CR 109.5: The ability's original controller — fixed even when
        // `player_scope` iteration has rebound `ability.controller`.
        TargetFilter::OriginalController => {
            Some(ability.original_controller.unwrap_or(ability.controller))
        }
        TargetFilter::ScopedPlayer => ability.scoped_player,
        TargetFilter::Player => ability.targets.iter().find_map(|target| match target {
            TargetRef::Player(player) => Some(*player),
            _ => None,
        }),
        TargetFilter::ParentTargetController => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Object(id) => state
                    .stack
                    .iter()
                    .find(|entry| entry.id == *id || entry.source_id == *id)
                    .map(|entry| entry.controller)
                    .or_else(|| state.objects.get(id).map(|obj| obj.controller)),
                TargetRef::Player(player) => Some(*player),
            })
            .or_else(|| {
                resolve_event_context_target(state, filter, ability.source_id).and_then(|target| {
                    match target {
                        TargetRef::Player(player) => Some(player),
                        TargetRef::Object(id) => state.objects.get(&id).map(|obj| obj.controller),
                    }
                })
            }),
        // CR 108.3 + CR 608.2c: Parent target's *owner* — mirrors the controller
        // path above, but resolves through `parent_target_owner` and falls back
        // to the event-context resolver (which itself may fall back to the
        // source's AttachedTo host for Aura phase triggers).
        TargetFilter::ParentTargetOwner => {
            crate::game::ability_utils::parent_target_owner(ability, state).or_else(|| {
                resolve_event_context_target(state, filter, ability.source_id).and_then(|target| {
                    match target {
                        TargetRef::Player(player) => Some(player),
                        TargetRef::Object(id) => state.objects.get(&id).map(|obj| obj.owner),
                    }
                })
            })
        }
        // CR 608.2c + CR 109.4: A player-only reference to the Nth chosen
        // player resolves from the resolution-scoped `chosen_players` list.
        TargetFilter::Typed(_) if filter.chosen_player_index().is_some() => {
            let index = filter.chosen_player_index().expect("checked by guard");
            ability.chosen_players.get(index as usize).copied()
        }
        _ => resolve_event_context_target(state, filter, ability.source_id).and_then(|target| {
            match target {
                TargetRef::Player(player) => Some(player),
                TargetRef::Object(id) => state.objects.get(&id).map(|obj| obj.controller),
            }
        }),
    }
}

/// Extract the source object ID from a trigger event.
pub(crate) fn extract_source_from_event(
    event: &crate::types::events::GameEvent,
) -> Option<ObjectId> {
    use crate::types::events::GameEvent;
    match event {
        GameEvent::BecomesTarget { source_id, .. } => Some(*source_id),
        GameEvent::SpellCast { object_id, .. } => Some(*object_id),
        GameEvent::DamageDealt { source_id, .. } => Some(*source_id),
        GameEvent::AbilityActivated { source_id, .. } => Some(*source_id),
        GameEvent::ZoneChanged { object_id, .. } => Some(*object_id),
        GameEvent::PermanentTapped { object_id, .. } => Some(*object_id),
        GameEvent::PermanentUntapped { object_id } => Some(*object_id),
        // CR 106.3 + CR 605.1a: For TapsForMana triggers, "that land" / "that permanent"
        // resolves to the mana source — the land/permanent being tapped for mana.
        GameEvent::ManaAdded { source_id, .. } => Some(*source_id),
        // CR 106.12a: `TappedForMana` is the per-resolution event a `TapsForMana`
        // trigger fires from; `source_id` is the permanent tapped for mana.
        GameEvent::TappedForMana { source_id, .. } => Some(*source_id),
        GameEvent::CounterAdded { object_id, .. } => Some(*object_id),
        GameEvent::Evolved { object_id } => Some(*object_id),
        GameEvent::CounterRemoved { object_id, .. } => Some(*object_id),
        GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
        GameEvent::CreatureDestroyed { object_id } => Some(*object_id),
        GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
        GameEvent::Unattached {
            old_target: TargetRef::Object(object_id),
            ..
        } => Some(*object_id),
        GameEvent::Discarded { object_id, .. } => Some(*object_id),
        GameEvent::Transformed { object_id } => Some(*object_id),
        GameEvent::TurnedFaceUp { object_id } => Some(*object_id),
        GameEvent::Cycled { object_id, .. } => Some(*object_id),
        GameEvent::CreatureSuspected { object_id } => Some(*object_id),
        GameEvent::Detained { object_id } => Some(*object_id),
        GameEvent::CaseSolved { object_id } => Some(*object_id),
        GameEvent::AttackersDeclared { attacker_ids, .. } if attacker_ids.len() == 1 => {
            attacker_ids.first().copied()
        }
        // CR 509.1: For a `Blocks` / `AttacksOrBlocks` trigger, "it" / the
        // triggering source is the creature that blocked. A single creature
        // blocking multiple attackers yields one `(blocker, attacker)` entry
        // per attacker, all sharing the same blocker — still an unambiguous
        // source. The source is only ambiguous when distinct blockers were
        // declared, in which case no single triggering object exists.
        GameEvent::BlockersDeclared { assignments } => {
            let mut blockers = assignments.iter().map(|(blocker, _)| *blocker);
            let first = blockers.next()?;
            blockers.all(|blocker| blocker == first).then_some(first)
        }
        _ => None,
    }
}

/// Extract the relevant player from a trigger event.
pub(crate) fn extract_player_from_event(
    event: &crate::types::events::GameEvent,
    state: &GameState,
) -> Option<PlayerId> {
    use crate::types::events::GameEvent;
    match event {
        GameEvent::LifeChanged { player_id, .. } => Some(*player_id),
        // CR 106.4 + CR 605.1b: `ManaAdded` carries the player whose pool gained
        // the mana — equivalently, the player who tapped the source for mana.
        // For TapsForMana triggers (Fertile Ground / Wild Growth / Utopia Sprawl
        // and the wider "its controller adds…" Aura class), this is the
        // enchanted land's controller, which `PlayerFilter::TriggeringPlayer`
        // rebinds as the resolving ability's controller so the bonus mana
        // routes to the tapper even when the Aura is opponent-controlled.
        GameEvent::ManaAdded { player_id, .. } => Some(*player_id),
        // CR 106.12a + CR 605.1b: `TappedForMana` carries the player who tapped
        // the source for mana — the triggering player for `TapsForMana`.
        GameEvent::TappedForMana { player_id, .. } => Some(*player_id),
        GameEvent::CardsDrawn { player_id, .. } => Some(*player_id),
        GameEvent::CardDrawn { player_id, .. } => Some(*player_id),
        GameEvent::Discarded { player_id, .. } => Some(*player_id),
        GameEvent::LandPlayed { player_id, .. } => Some(*player_id),
        GameEvent::SpellCast { controller, .. } => Some(*controller),
        // CR 602.2a: "Its controller is the player who activated the ability."
        // For "Whenever a player activates an ability, … deals 1 damage to that
        // player" triggers (Burning-Tree Shaman, Flamescroll Celebrant),
        // `TriggeringPlayer` / "that player" binds to the activating player
        // carried on the event.
        GameEvent::AbilityActivated { player_id, .. } => Some(*player_id),
        GameEvent::PermanentSacrificed { player_id, .. } => Some(*player_id),
        GameEvent::Unattached {
            old_target: TargetRef::Player(player_id),
            ..
        } => Some(*player_id),
        GameEvent::Cycled { player_id, .. } => Some(*player_id),
        GameEvent::PlayerPerformedAction { player_id, .. } => Some(*player_id),
        GameEvent::CrimeCommitted { player_id, .. } => Some(*player_id),
        GameEvent::PlayerEliminated { player_id, .. } => Some(*player_id),
        // CR 506.2 + CR 508.1: The attacking player is the common controller of the
        // declared attackers in this batch. All attackers in one AttackersDeclared
        // batch share the active player as their controller.
        GameEvent::AttackersDeclared { attacker_ids, .. } => attacker_ids
            .iter()
            .find_map(|id| state.objects.get(id).map(|obj| obj.controller)),
        GameEvent::BecomesTarget { target, source_id } => match target {
            TargetRef::Player(player_id) => Some(*player_id),
            TargetRef::Object(_) => state.objects.get(source_id).map(|obj| obj.controller),
        },
        // CR 603.7c: "that player" for DamageDone triggers refers to the damaged player.
        GameEvent::DamageDealt { target, .. } => match target {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(oid) => state.objects.get(oid).map(|obj| obj.controller),
        },
        // CR 120.1 + CR 510.2: Combat damage to a player binds `TriggeringPlayer`
        // / "that player" to the damaged player. Rev, Tithe Extractor's exile-top
        // effect must read the damaged opponent's library, not the ability
        // controller's.
        GameEvent::CombatDamageDealtToPlayer { player_id, .. } => Some(*player_id),
        // CR 500.2 + CR 603.7c: Phase-change triggers like "at the beginning of
        // each player's upkeep" bind "that player" / `TriggeringPlayer` to the
        // active player — the player whose phase is currently beginning.
        // Without this, Ruthless Winnower ("that player sacrifices a non-Elf
        // creature") would have no player anchor and the sacrifice filter
        // would match across all players.
        GameEvent::PhaseChanged { .. } => Some(state.active_player),
        // CR 603.6 + CR 109.4: For zone-change triggers ("whenever a creature
        // enters", "whenever an opponent's creature enters", "whenever a card
        // is put into a graveyard from anywhere"), the `TriggeringPlayer` /
        // "that player" referent is the moving object's controller as
        // recorded by the `ZoneChangeRecord` snapshot — preserved per CR
        // 603.10a so leaves-the-battlefield triggers still see the correct
        // controller after the object has transferred or left play. Without
        // this arm, ETB and dies-trigger sub-effects with `target:
        // TriggeringPlayer` fell back to the ability controller, hitting the
        // wrong player (Suture Priest #560, Bloodchief Ascension #546).
        GameEvent::ZoneChanged { record, .. } => Some(record.controller),
        _ => None,
    }
}

/// CR 603.7c: Extract a numeric amount from a trigger event.
/// Returns the quantity relevant to the event type (damage dealt, life changed, etc.).
pub(crate) fn extract_amount_from_event(event: &crate::types::events::GameEvent) -> Option<i32> {
    use crate::types::events::GameEvent;
    match event {
        GameEvent::DamageDealt { amount, .. } => Some(*amount as i32),
        // CR 615.5: Prevention effects' additional effects refer to the amount of
        // damage that was prevented. Exposing the prevented amount here lets
        // `EventContextAmount` resolve the "for each 1 damage prevented this way"
        // class (Phyrexian Hydra, Vigor, Stormwild Capridor, Hostility) when the
        // post-replacement follow-up resolves.
        GameEvent::DamagePrevented { amount, .. } => Some(*amount as i32),
        GameEvent::LifeChanged { amount, .. } => Some(amount.abs()),
        GameEvent::CardsDrawn { count, .. } => Some(*count as i32),
        GameEvent::CounterAdded { count, .. } => Some(*count as i32),
        GameEvent::CounterRemoved { count, .. } => Some(*count as i32),
        GameEvent::Discarded { .. } => Some(1),
        // CR 508.1m + CR 603.2c: Batched attack-trigger context stores the
        // attackers that satisfied the trigger subject, so "that many" reads
        // the size of that contextual attack event.
        GameEvent::AttackersDeclared { attacker_ids, .. } => Some(attacker_ids.len() as i32),
        // CR 706.2: the final number of a die roll is its result. Lets
        // `EventContextAmount` resolve "where X is the result" pump effects.
        GameEvent::DieRolled { result, .. } => Some(*result as i32),
        // CR 120.1 + CR 603.7c: total combat damage dealt to this player by the
        // matching source set. For DamageDoneOnceByController triggers, this is
        // the filtered total stamped by matching_damage_done_once_by_controller_event.
        GameEvent::CombatDamageDealtToPlayer { total_damage, .. } => Some(*total_damage as i32),
        _ => None,
    }
}

// --- Internal helpers ---

/// Find activated/triggered (non-mana) abilities on the stack as legal targets.
/// Mana abilities never go on the stack, so all ActivatedAbility/TriggeredAbility
/// entries are valid. Excludes the source ability itself.
fn add_stack_abilities(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
    targets: &mut Vec<TargetRef>,
) {
    for entry in &state.stack {
        if entry.id == source_id {
            continue; // Don't target yourself
        }
        if stack_ability_matches_filter(entry, filter, source_controller) {
            targets.push(TargetRef::Object(entry.id));
        }
    }
}

pub(crate) fn stack_entry_matches_filter(
    state: &GameState,
    entry: &StackEntry,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
) -> bool {
    let target_ctx =
        super::filter::FilterContext::from_source_with_controller(source_id, source_controller);
    stack_entry_matches_filter_with_context(
        state,
        entry,
        filter,
        source_controller,
        source_id,
        &target_ctx,
    )
}

fn stack_entry_matches_filter_with_context(
    state: &GameState,
    entry: &StackEntry,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
    target_ctx: &super::filter::FilterContext,
) -> bool {
    match &entry.kind {
        StackEntryKind::Spell { .. } => {
            stack_spell_entry_matches_filter(state, entry, filter, source_id, target_ctx)
        }
        StackEntryKind::ActivatedAbility { .. }
        | StackEntryKind::TriggeredAbility { .. }
        | StackEntryKind::KeywordAction { .. } => {
            filter_targets_stack_abilities(filter)
                && stack_ability_matches_filter(entry, filter, source_controller)
        }
    }
}

fn stack_ability_matches_filter(
    entry: &StackEntry,
    filter: &TargetFilter,
    source_controller: PlayerId,
) -> bool {
    match filter {
        TargetFilter::StackAbility { controller } => {
            if !matches!(
                &entry.kind,
                // CR 113.3b / CR 113.3c: Activated and triggered abilities are
                // objects on the stack. Mana abilities do not reach the stack, so
                // entries of these kinds are targetable stack abilities.
                StackEntryKind::ActivatedAbility { .. }
                    | StackEntryKind::TriggeredAbility { .. }
                    | StackEntryKind::KeywordAction { .. }
            ) {
                return false;
            }
            stack_entry_controller_matches(entry, controller.as_ref(), source_controller)
        }
        TargetFilter::Typed(tf) => {
            if !tf.type_filters.is_empty()
                && !tf
                    .type_filters
                    .iter()
                    .all(|ty| matches!(ty, TypeFilter::Card))
            {
                return false;
            }
            if tf.controller.is_some()
                && !stack_entry_controller_matches(entry, tf.controller.as_ref(), source_controller)
            {
                return false;
            }
            tf.properties.iter().all(|property| match property {
                FilterProp::HasSingleTarget => entry
                    .ability()
                    .is_some_and(|ability| ability.targets.len() == 1),
                FilterProp::InZone { zone } => *zone == Zone::Stack,
                _ => true,
            })
        }
        TargetFilter::And { filters } => filters
            .iter()
            .all(|filter| stack_ability_matches_filter(entry, filter, source_controller)),
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|filter| stack_ability_matches_filter(entry, filter, source_controller)),
        TargetFilter::Not { filter } => {
            !stack_ability_matches_filter(entry, filter, source_controller)
        }
        TargetFilter::Any => true,
        _ => false,
    }
}

fn stack_entry_controller_matches(
    entry: &StackEntry,
    controller: Option<&ControllerRef>,
    source_controller: PlayerId,
) -> bool {
    let Some(controller) = controller else {
        return true;
    };
    let is_you = entry.controller == source_controller;
    match controller {
        ControllerRef::You => is_you,
        ControllerRef::Opponent => !is_you,
        _ => false,
    }
}

fn add_zone_targets(
    state: &GameState,
    object_ids: impl IntoIterator<Item = ObjectId>,
    filter: &TargetFilter,
    target_ctx: &super::filter::FilterContext,
    require_full_targeting: bool,
    targets: &mut Vec<TargetRef>,
) {
    let source_id = target_ctx.source_id;
    let source_controller = target_ctx
        .source_controller
        .expect("target enumeration context must include a source controller");
    for obj_id in object_ids {
        if super::filter::matches_target_filter(state, obj_id, filter, target_ctx) {
            let obj = match state.objects.get(&obj_id) {
                Some(o) => o,
                None => continue,
            };
            if require_full_targeting {
                if can_target(obj, source_controller, source_id, state) {
                    targets.push(TargetRef::Object(obj_id));
                }
            } else if !is_protected_from(obj, source_id, state) {
                targets.push(TargetRef::Object(obj_id));
            }
        }
    }
}

fn add_stack_spells(
    state: &GameState,
    filter: &TargetFilter,
    source_controller: PlayerId,
    source_id: ObjectId,
    target_ctx: &super::filter::FilterContext,
    targets: &mut Vec<TargetRef>,
) {
    for entry in &state.stack {
        if !stack_spell_entry_matches_filter(state, entry, filter, source_id, target_ctx) {
            continue;
        }

        let obj = match state.objects.get(&entry.id) {
            Some(o) => o,
            None => continue,
        };
        if can_target(obj, source_controller, source_id, state) {
            targets.push(TargetRef::Object(entry.id));
        }
    }
}

fn stack_spell_entry_matches_filter(
    state: &GameState,
    entry: &StackEntry,
    filter: &TargetFilter,
    source_id: ObjectId,
    target_ctx: &super::filter::FilterContext,
) -> bool {
    if !matches!(entry.kind, StackEntryKind::Spell { .. }) {
        return false;
    }

    let requires_single_target = filter_requires_single_target(filter);
    let targets_only_constraint = super::filter::extract_targets_only(filter);
    let targets_constraint = super::filter::extract_targets(filter);
    let source_controller_opt = state.objects.get(&source_id).map(|o| o.controller);

    // CR 115.9a: "with a single target" counts the spell's chosen target instances.
    if requires_single_target {
        let targets = entry.ability().map(|a| &a.targets[..]).unwrap_or(&[]);
        if targets.len() != 1 {
            return false;
        }
    }

    let bare_ctx = super::filter::FilterContext::from_source(state, source_id);
    // CR 115.9c: "that targets only [X]" — all targets must match the constraint filter.
    if let Some(ref constraint) = targets_only_constraint {
        let targets = entry.ability().map(|a| &a.targets[..]).unwrap_or(&[]);
        if targets.is_empty()
            || !targets.iter().all(|t| match t {
                TargetRef::Object(id) => {
                    super::filter::matches_target_filter(state, *id, constraint, &bare_ctx)
                }
                TargetRef::Player(pid) => super::filter::player_matches_target_filter_in_state(
                    state,
                    constraint,
                    *pid,
                    source_controller_opt,
                ),
            })
        {
            return false;
        }
    }
    // CR 115.9b: "that targets [X]" — at least one target must match (.any() semantics).
    if let Some(ref constraint) = targets_constraint {
        let targets = entry.ability().map(|a| &a.targets[..]).unwrap_or(&[]);
        if targets.is_empty()
            || !targets.iter().any(|t| match t {
                TargetRef::Object(id) => {
                    super::filter::matches_target_filter(state, *id, constraint, &bare_ctx)
                }
                TargetRef::Player(pid) => super::filter::player_matches_target_filter_in_state(
                    state,
                    constraint,
                    *pid,
                    source_controller_opt,
                ),
            })
        {
            return false;
        }
    }

    stack_spell_matches_filter(state, entry.id, filter, target_ctx)
}

fn stack_spell_matches_filter(
    state: &GameState,
    object_id: ObjectId,
    filter: &TargetFilter,
    ctx: &super::filter::FilterContext,
) -> bool {
    match filter {
        TargetFilter::StackSpell => true,
        TargetFilter::StackAbility { .. } => false,
        TargetFilter::Typed(_) => {
            super::filter::matches_target_filter(state, object_id, filter, ctx)
        }
        TargetFilter::And { filters } => filters
            .iter()
            .all(|filter| stack_spell_matches_filter(state, object_id, filter, ctx)),
        TargetFilter::Or { filters } => filters
            .iter()
            .any(|filter| stack_spell_matches_filter(state, object_id, filter, ctx)),
        TargetFilter::Not { filter } => !stack_spell_matches_filter(state, object_id, filter, ctx),
        other => super::filter::matches_target_filter(state, object_id, other, ctx),
    }
}

/// Check if a filter contains a `HasSingleTarget` property anywhere in its tree.
fn filter_requires_single_target(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::HasSingleTarget)),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_requires_single_target)
        }
        _ => false,
    }
}

fn filter_targets_stack_spells(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::StackSpell => true,
        TargetFilter::Typed(TypedFilter {
            type_filters,
            properties,
            ..
        }) => {
            let in_stack = properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone } if *zone == Zone::Stack));
            in_stack || type_filters.contains(&TypeFilter::Card)
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_targets_stack_spells)
        }
        TargetFilter::Not { filter } => filter_targets_stack_spells(filter),
        _ => false,
    }
}

fn filter_targets_stack_abilities(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::StackAbility { .. } => true,
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_targets_stack_abilities)
        }
        TargetFilter::Not { filter } => filter_targets_stack_abilities(filter),
        _ => false,
    }
}

fn add_players(state: &GameState, targets: &mut Vec<TargetRef>, source_id: ObjectId) {
    // Player-phasing exclusion: a phased-out player is treated as though they
    // don't exist for targeting purposes (mirrors CR 702.26b for permanents,
    // applied to players via card Oracle text like "you phase out").
    for player in &state.players {
        if player.is_phased_out() {
            continue;
        }
        // CR 800.4a: When a player leaves the game in a multiplayer game, all
        // objects they own/control leave the game and the player ceases to be
        // a valid target. Eliminated players cannot be targeted by any spell
        // or ability (CR 608.2b illegal-target fizzle applies on resolution).
        if player.is_eliminated {
            continue;
        }
        // CR 702.16b: A player with protection from the spell/ability's source
        // can't be targeted by it.
        if super::static_abilities::player_protection_from(state, player.id, Some(source_id)) {
            continue;
        }
        targets.push(TargetRef::Player(player.id));
    }
}

fn add_specific_player(
    state: &GameState,
    targets: &mut Vec<TargetRef>,
    player_id: PlayerId,
    source_id: ObjectId,
) {
    let Some(player) = state.players.iter().find(|player| player.id == player_id) else {
        return;
    };
    if player.is_phased_out() || player.is_eliminated {
        return;
    }
    if super::static_abilities::player_protection_from(state, player.id, Some(source_id)) {
        return;
    }
    targets.push(TargetRef::Player(player.id));
}

/// CR 702.16b: Protection prevents targeting from sources with the relevant quality.
fn is_protected_from(
    obj: &crate::game::game_object::GameObject,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let Some(source_obj) = state.objects.get(&source_id) else {
        return false;
    };

    for kw in &obj.keywords {
        if let Keyword::Protection(protection) = kw {
            if crate::game::keywords::source_matches_protection_target(protection, obj, source_obj)
            {
                return true;
            }
        }
    }
    false
}

/// CR 702.11d: Check if a source matches a HexproofFilter.
fn hexproof_filter_matches(
    filter: &HexproofFilter,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    let source_obj = match state.objects.get(&source_id) {
        Some(o) => o,
        None => return false,
    };
    match filter {
        HexproofFilter::Color(color) => source_obj.color.contains(color),
        HexproofFilter::CardType(type_name) => {
            crate::game::keywords::source_matches_card_type(source_obj, type_name)
        }
        HexproofFilter::Quality(quality) => {
            crate::game::keywords::source_matches_quality(source_obj, quality)
        }
        // CR 702.11d + CR 702.16 + CR 609.6: `ChosenColor` is normally
        // resolved to a concrete `Color(_)` at layer application time (see
        // `layers::apply_continuous_effect`). The intrinsic variant arm
        // remains for cards whose printed text resolves "the chosen color" on
        // the same object that chose it — mirrors
        // `source_matches_protection_target` for `ProtectionTarget::ChosenColor`.
        HexproofFilter::ChosenColor => state
            .objects
            .get(&source_id)
            .and_then(|src| src.chosen_color())
            .is_some_and(|color| source_obj.color.contains(&color)),
    }
}

/// Full battlefield targeting check: shroud + hexproof + protection (CR 702.16b).
fn can_target(
    obj: &crate::game::game_object::GameObject,
    source_controller: PlayerId,
    source_id: ObjectId,
    state: &GameState,
) -> bool {
    // CR 702.18a: Shroud prevents targeting by any player.
    if obj.has_keyword(&Keyword::Shroud) {
        return false;
    }
    // CR 702.11e: An "ignore hexproof" effect (Detection Tower) lets the targeting
    // source's controller target a permanent "as though it didn't have hexproof".
    // It bypasses Hexproof / Hexproof from [quality] only — never Shroud.
    let ignores_hexproof =
        crate::game::static_abilities::player_ignores_hexproof(state, source_controller);
    // CR 702.11a: Hexproof prevents targeting by opponents.
    if !ignores_hexproof
        && obj.has_keyword(&Keyword::Hexproof)
        && obj.controller != source_controller
    {
        return false;
    }
    // CR 702.11d: "Hexproof from [quality]" prevents targeting by opponents' sources
    // with the matching quality. CR 702.11e: IgnoreHexproof bypasses this.
    if !ignores_hexproof && obj.controller != source_controller {
        for kw in &obj.keywords {
            if let Keyword::HexproofFrom(ref filter) = kw {
                if hexproof_filter_matches(filter, source_id, state) {
                    return false;
                }
            }
        }
    }
    if is_protected_from(obj, source_id, state) {
        return false;
    }
    // CR 702.18a: A static "can't be the target of spells or abilities" is the
    // descriptive (non-keyworded) form of Shroud — the permanent can't be the
    // target of any spell or ability, regardless of controller. It is modeled as
    // `StaticMode::CantBeTargeted`, living on the object's own static definitions
    // (a self-referential static, or propagated onto a subject via `AddStaticMode`
    // — see `static_mode_needs_grant_propagation`). The opponent-scoped variant
    // ("... your opponents control") is parsed as `Keyword::Hexproof` instead, so
    // it is handled by the Hexproof branch above rather than here.
    if super::functioning_abilities::active_static_definitions(state, obj)
        .any(|def| matches!(def.mode, crate::types::statics::StaticMode::CantBeTargeted))
    {
        return false;
    }
    // CR 702.21a: Ward is a triggered ability, not a targeting restriction.
    // Targeting is legal; the ward trigger fires via process_triggers() and
    // counters the spell/ability unless the opponent pays the ward cost.
    // TODO(CR 115.7): Retargeting (Willbender-style) not implemented.
    true
}

/// CR 400.1: Returns all object IDs in the given zone.
///
/// Per-player zones (Hand, Library, Graveyard) are aggregated across all players.
/// Shared zones (Battlefield, Exile, Stack, Command) return the global list.
///
/// CR 702.26b: Phased-out battlefield permanents are treated as though they
/// don't exist — excluded from the `Zone::Battlefield` listing. Zones other
/// than battlefield can't contain phased-out permanents (phasing is a
/// battlefield-only status, CR 702.26d).
pub(crate) fn zone_object_ids(state: &GameState, zone: Zone) -> Vec<ObjectId> {
    match zone {
        Zone::Battlefield => state
            .battlefield
            .iter()
            .copied()
            .filter(|id| state.objects.get(id).is_some_and(|obj| obj.is_phased_in()))
            .collect(),
        Zone::Stack => state.stack.iter().map(|e| e.id).collect(),
        Zone::Exile => state.exile.iter().copied().collect(),
        Zone::Graveyard => state
            .players
            .iter()
            .flat_map(|p| p.graveyard.iter().copied())
            .collect(),
        Zone::Hand => state
            .players
            .iter()
            .flat_map(|p| p.hand.iter().copied())
            .collect(),
        Zone::Library => state
            .players
            .iter()
            .flat_map(|p| p.library.iter().copied())
            .collect(),
        Zone::Command => vec![],
    }
}

/// Extract all explicit zone restrictions from a target filter, recursing through combinators.
fn extract_explicit_zones(filter: &TargetFilter) -> Vec<Zone> {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => {
            let mut explicit_zones = Vec::new();
            for property in properties {
                match property {
                    FilterProp::InZone { zone } => explicit_zones.push(*zone),
                    FilterProp::InAnyZone { zones } => explicit_zones.extend(zones.iter().copied()),
                    _ => {}
                }
            }
            explicit_zones
        }
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().flat_map(extract_explicit_zones).collect()
        }
        TargetFilter::Not { filter } => extract_explicit_zones(filter),
        _ => vec![],
    }
}

/// CR 608.2c: Find the id of the most recently published non-empty tracked
/// object set.
///
/// The parser emits `TargetFilter::TrackedSet`/`TrackedSetFiltered` with the
/// sentinel id `TrackedSetId(0)` for inline "the milled/revealed/exiled cards"
/// continuations; the concrete set id is only known at resolution time. An
/// effect that publishes its affected objects records them under a fresh,
/// monotonically increasing `TrackedSetId`, so the highest non-empty id is the
/// set the immediately following continuation refers to.
///
/// Empty sets are skipped because a continuation can only meaningfully refer to
/// a set that still has members. Returns `None` when no non-empty set exists.
pub(crate) fn latest_tracked_set_id(state: &GameState) -> Option<TrackedSetId> {
    state
        .tracked_object_sets
        .iter()
        .filter(|(_, objects)| !objects.is_empty())
        .max_by_key(|(id, _)| id.0)
        .map(|(&id, _)| id)
}

/// CR 510.2 + CR 608.2c: In a simultaneous combat-damage event, "those
/// creatures" on the resolving trigger can refer to the filtered source set
/// carried by `CombatDamageDealtToPlayer`.
pub(crate) fn current_combat_damage_source_filter(state: &GameState) -> Option<TargetFilter> {
    let source_amounts = match state.current_trigger_event.as_ref()? {
        GameEvent::CombatDamageDealtToPlayer { source_amounts, .. } => source_amounts,
        _ => return None,
    };

    match source_amounts.as_slice() {
        [] => None,
        [(id, _)] => Some(TargetFilter::SpecificObject { id: *id }),
        pairs => Some(TargetFilter::Or {
            filters: pairs
                .iter()
                .map(|(id, _)| TargetFilter::SpecificObject { id: *id })
                .collect(),
        }),
    }
}

/// CR 608.2c: Bind the `TrackedSetId(0)` sentinel in a `TargetFilter` to the
/// most recent non-empty tracked set.
///
/// Handles both the bare `TrackedSet` continuation ("the milled cards", "the
/// exiled card") and its type-filtered intersection `TrackedSetFiltered` ("X
/// cards revealed this way"). Filters that are not sentinel-backed — already
/// bound tracked-set filters and every non-tracked-set filter — are returned
/// unchanged. The active chain-local set wins first; when no chain set is
/// available, combat-damage trigger context can supply a filtered source set;
/// otherwise the latest non-empty tracked set is used for legacy callers. If
/// none of those exists, the sentinel is left in place so downstream resolution
/// still sees a (vacuously matching nothing) filter rather than a silently
/// mismatched concrete id.
///
/// This is the single authority for sentinel binding: `ChangeZone` resolution,
/// chained-ability resolution, and the delayed-trigger / counter / permission
/// resolvers all route through it so every path resolves the sentinel
/// identically.
pub(crate) fn resolve_tracked_set_sentinel(
    state: &GameState,
    filter: TargetFilter,
) -> TargetFilter {
    match filter {
        TargetFilter::TrackedSet {
            id: TrackedSetId(0),
        } => state
            .chain_tracked_set_id
            .map(|id| TargetFilter::TrackedSet { id })
            .or_else(|| current_combat_damage_source_filter(state))
            .or_else(|| latest_tracked_set_id(state).map(|id| TargetFilter::TrackedSet { id }))
            .unwrap_or(TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            }),
        TargetFilter::TrackedSetFiltered {
            id: TrackedSetId(0),
            filter,
        } => {
            if let Some(id) = state.chain_tracked_set_id {
                TargetFilter::TrackedSetFiltered { id, filter }
            } else if let Some(source_filter) = current_combat_damage_source_filter(state) {
                TargetFilter::And {
                    filters: vec![source_filter, *filter],
                }
            } else if let Some(id) = latest_tracked_set_id(state) {
                TargetFilter::TrackedSetFiltered { id, filter }
            } else {
                TargetFilter::TrackedSetFiltered {
                    id: TrackedSetId(0),
                    filter,
                }
            }
        }
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::game_object::AttachTarget;
    use crate::game::zones::create_object;
    use crate::types::ability::{Comparator, ContinuousModification, Duration, QuantityExpr};
    use crate::types::card_type::CoreType;
    use crate::types::game_state::CastingVariant;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::{HexproofFilter, ProtectionTarget};
    use crate::types::mana::ManaColor;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    #[test]
    fn extract_amount_from_combat_damage_dealt_to_player_returns_total_damage() {
        let event = GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_amounts: vec![(ObjectId(1), 7)],
            total_damage: 7,
        };
        assert_eq!(extract_amount_from_event(&event), Some(7));
    }

    #[test]
    fn extract_player_from_combat_damage_dealt_to_player_returns_damaged_player() {
        let (state, _c0, _c1) = setup_with_creatures();
        let event = GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_amounts: vec![(ObjectId(1), 3)],
            total_damage: 3,
        };
        assert_eq!(extract_player_from_event(&event, &state), Some(PlayerId(1)));
    }

    fn setup_with_creatures() -> (GameState, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);

        // Creature controlled by player 0
        let c0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c0).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Creature controlled by player 1
        let c1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c1).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        (state, c0, c1)
    }

    fn creature_filter() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::creature())
    }

    #[test]
    fn post_replacement_source_controller_resolves_to_event_source_controller() {
        // CR 615.5 + CR 609.7: When `state.post_replacement_event_source` is
        // populated (set by the prevention applier's Prevented arm), the new
        // filter resolves to the controller of that object — NOT to the
        // ability source's controller. Swans of Bryn Argoll's regression test:
        // damage was prevented from a P1-controlled source, so P1 (the source's
        // controller) draws the cards, not Swans's controller (P0).
        let (mut state, c0, _c1) = setup_with_creatures();
        // c0 is controlled by P0 — pretend it's the prevented damage source
        // and the prevention shield (e.g. Swans) is controlled by P1.
        state.post_replacement_event_source = Some(c0);
        let result = resolve_event_context_target(
            &state,
            &TargetFilter::PostReplacementSourceController,
            ObjectId(999), // arbitrary ability source — unused for this filter
        );
        assert_eq!(result, Some(TargetRef::Player(PlayerId(0))));
    }

    #[test]
    fn post_replacement_source_controller_returns_none_when_slot_empty() {
        // Defensive: filter only resolves inside the post-replacement window.
        // Outside that window the slot is `None` and the filter should return
        // `None`, letting callers fall back to controller / target_player.
        let (state, _c0, _c1) = setup_with_creatures();
        assert!(state.post_replacement_event_source.is_none());
        let result = resolve_event_context_target(
            &state,
            &TargetFilter::PostReplacementSourceController,
            ObjectId(999),
        );
        assert_eq!(result, None);
    }

    #[test]
    fn stack_spell_resolves_spell_cast_trigger() {
        let mut state = GameState::new_two_player(42);
        let spell_id = ObjectId(10);
        state.current_trigger_event = Some(crate::types::events::GameEvent::SpellCast {
            card_id: CardId(1),
            object_id: spell_id,
            controller: PlayerId(0),
        });
        assert_eq!(
            resolve_event_context_target(&state, &TargetFilter::StackSpell, ObjectId(20)),
            Some(TargetRef::Object(spell_id))
        );
    }

    #[test]
    fn find_legal_targets_creature_returns_only_creatures() {
        let (state, c0, c1) = setup_with_creatures();
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn attached_to_resolves_player_host() {
        let mut state = GameState::new_two_player(42);
        let curse = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Curse".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&curse).unwrap().attached_to =
            Some(AttachTarget::Player(PlayerId(1)));

        assert_eq!(
            resolve_event_context_target(&state, &TargetFilter::AttachedTo, curse),
            Some(TargetRef::Player(PlayerId(1)))
        );
        assert_eq!(
            find_legal_targets(&state, &TargetFilter::AttachedTo, PlayerId(0), curse),
            vec![TargetRef::Player(PlayerId(1))]
        );
    }

    #[test]
    fn hexproof_creature_not_targetable_by_opponent() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        assert!(!targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn hexproof_creature_targetable_by_controller() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(1), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn ignore_hexproof_lets_controller_target_opponents_hexproof_creature() {
        // CR 702.11e: Detection Tower — while the targeting player has an active
        // "ignore hexproof" effect, opponents' hexproof permanents are legal targets.
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        // Baseline: P0 can't target P1's hexproof creature.
        assert!(
            !find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99))
                .contains(&TargetRef::Object(c1))
        );

        // Grant P0 IgnoreHexproof (the player-scoped transient a bypass effect creates).
        state.add_transient_continuous_effect(
            ObjectId(99),
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::IgnoreHexproof,
            }],
            None,
        );

        // Now P0 may target it; the grant is player-scoped to P0.
        assert!(
            find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99))
                .contains(&TargetRef::Object(c1))
        );
    }

    #[test]
    fn ignore_hexproof_bypasses_hexproof_from_quality() {
        // CR 702.11e: "as though it didn't have hexproof" also bypasses
        // hexproof from [quality].
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)));
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Red);

        assert!(!can_target(
            state.objects.get(&c1).unwrap(),
            PlayerId(0),
            source_id,
            &state
        ));

        state.add_transient_continuous_effect(
            source_id,
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::IgnoreHexproof,
            }],
            None,
        );

        assert!(can_target(
            state.objects.get(&c1).unwrap(),
            PlayerId(0),
            source_id,
            &state
        ));
    }

    #[test]
    fn ignore_hexproof_does_not_bypass_shroud() {
        // CR 702.18a: IgnoreHexproof bypasses hexproof only — never shroud.
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Shroud);
        state.add_transient_continuous_effect(
            ObjectId(99),
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(0) },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::IgnoreHexproof,
            }],
            None,
        );

        assert!(
            !find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99))
                .contains(&TargetRef::Object(c1))
        );
    }

    #[test]
    fn shroud_creature_not_targetable_by_anyone() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Shroud);

        let targets_p0 = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        let targets_p1 = find_legal_targets(&state, &creature_filter(), PlayerId(1), ObjectId(99));
        assert!(!targets_p0.contains(&TargetRef::Object(c1)));
        assert!(!targets_p1.contains(&TargetRef::Object(c1)));
    }

    /// CR 702.18a: A `StaticMode::CantBeTargeted` static (the descriptive Shroud
    /// form, "~ can't be the target of spells or abilities") makes the permanent
    /// untargetable by EVERY player, including its own controller — distinguishing
    /// it from Hexproof, which only blocks opponents.
    #[test]
    fn cant_be_targeted_static_blocks_all_players() {
        let (mut state, _c0, c1) = setup_with_creatures();
        // c1 is controlled by P1. Grant it the blanket static directly, mirroring
        // a self-referential static / the `AddStaticMode` propagation onto a subject.
        state.objects.get_mut(&c1).unwrap().static_definitions.push(
            crate::types::ability::StaticDefinition::new(
                crate::types::statics::StaticMode::CantBeTargeted,
            )
            .affected(crate::types::ability::TargetFilter::SelfRef),
        );

        let targets_p0 = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        let targets_p1 = find_legal_targets(&state, &creature_filter(), PlayerId(1), ObjectId(99));
        assert!(
            !targets_p0.contains(&TargetRef::Object(c1)),
            "opponent cannot target a CantBeTargeted permanent"
        );
        assert!(
            !targets_p1.contains(&TargetRef::Object(c1)),
            "the controller cannot target it either (Shroud semantics, not Hexproof)"
        );
    }

    #[test]
    fn validate_targets_filters_out_removed_objects() {
        let (mut state, c0, c1) = setup_with_creatures();
        let original = vec![TargetRef::Object(c0), TargetRef::Object(c1)];

        state.battlefield.retain(|id| *id != c1);

        let legal = validate_targets(
            &state,
            &original,
            &creature_filter(),
            PlayerId(0),
            ObjectId(99),
        );
        assert!(legal.contains(&TargetRef::Object(c0)));
        assert!(!legal.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn check_fizzle_all_targets_illegal() {
        let original = vec![
            TargetRef::Object(ObjectId(1)),
            TargetRef::Object(ObjectId(2)),
        ];
        let legal: Vec<TargetRef> = vec![];
        assert!(check_fizzle(&original, &legal));
    }

    #[test]
    fn check_fizzle_some_targets_legal() {
        let original = vec![
            TargetRef::Object(ObjectId(1)),
            TargetRef::Object(ObjectId(2)),
        ];
        let legal = vec![TargetRef::Object(ObjectId(1))];
        assert!(!check_fizzle(&original, &legal));
    }

    #[test]
    fn check_fizzle_no_targets_never_fizzles() {
        let original: Vec<TargetRef> = vec![];
        let legal: Vec<TargetRef> = vec![];
        assert!(!check_fizzle(&original, &legal));
    }

    #[test]
    fn protection_from_red_prevents_red_source_targeting() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();

        // Give c1 protection from red
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));

        // Create a red source spell
        let red_source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&red_source)
            .unwrap()
            .color
            .push(ManaColor::Red);

        // Red source cannot target creature with protection from red
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), red_source);
        assert!(!targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn protection_from_red_allows_blue_source_targeting() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();

        // Give c1 protection from red
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));

        // Create a blue source spell
        let blue_source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Unsummon".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&blue_source)
            .unwrap()
            .color
            .push(ManaColor::Blue);

        // Blue source CAN target creature with protection from red
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), blue_source);
        assert!(targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn protection_from_each_color_blocks_every_color_source() {
        // CR 702.16b + CR 105.2: "Protection from each color" — Akroma's Will
        // / Iridescent Angel scenario. End-to-end: parse the Oracle text via
        // `extract_keyword_line` (which routes through `expand_protection_parts`
        // and emits 5 typed `Protection(Color(X))` keywords), attach the
        // parsed keywords to a creature, and verify every monocolored source
        // is rejected by `find_legal_targets`. Regression test for the bug
        // where "protection from each color" was emitted as the no-op
        // `ProtectionTarget::CardType("each color")`, letting black sources
        // like Dark Impostor target a creature buffed by Akroma's Will.
        use crate::types::mana::ManaColor;

        let keywords = crate::parser::oracle_keyword::extract_keyword_line(
            "protection from each color",
            &["protection".to_string()],
        )
        .expect("'protection from each color' should parse as a keyword line");

        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .extend(keywords);

        for (idx, color) in [
            ManaColor::White,
            ManaColor::Blue,
            ManaColor::Black,
            ManaColor::Red,
            ManaColor::Green,
        ]
        .into_iter()
        .enumerate()
        {
            let source = create_object(
                &mut state,
                CardId(100u64 + idx as u64),
                PlayerId(0),
                format!("{color:?} Source"),
                Zone::Battlefield,
            );
            state.objects.get_mut(&source).unwrap().color.push(color);

            let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), source);
            assert!(
                !targets.contains(&TargetRef::Object(c1)),
                "creature with protection from each color must reject {color:?} source"
            );
        }
    }

    #[test]
    fn ward_does_not_prevent_targeting() {
        // Ward should be recognized but not block targeting (cost enforcement deferred)
        let (mut state, _c0, c1) = setup_with_creatures();

        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Ward(crate::types::keywords::WardCost::Mana(
                crate::types::mana::ManaCost::Cost {
                    generic: 2,
                    shards: vec![],
                },
            )));

        // Ward creature can still be targeted (cost enforcement is separate)
        let targets = find_legal_targets(&state, &creature_filter(), PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c1)));
    }

    // ---- find_legal_targets tests ----

    use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter};

    fn setup_with_typed_creatures() -> (GameState, ObjectId, ObjectId, ObjectId) {
        let mut state = GameState::new_two_player(42);

        // Creature controlled by player 0
        let c0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c0).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Creature controlled by player 1
        let c1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&c1).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
        }

        // Land controlled by player 1
        let land = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Mountain".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
        }

        (state, c0, c1, land)
    }

    #[test]
    fn find_legal_targets_creature_filter() {
        let (state, c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed(TypedFilter::creature());
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn find_legal_targets_permanent_opponent_nonland() {
        let (state, _c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::Opponent)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        // Should find opponent's creature but not their land
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn find_legal_targets_permanent_opponent_nonland_via_type_filter() {
        // TypeFilter::Non is case-insensitive via type_filter_matches, so a single test suffices
        let (state, _c0, c1, _land) = setup_with_typed_creatures();
        let filter = TargetFilter::Typed(
            TypedFilter::permanent()
                .controller(ControllerRef::Opponent)
                .with_type(TypeFilter::Non(Box::new(TypeFilter::Land))),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert_eq!(targets.len(), 1);
    }

    #[test]
    fn find_legal_targets_honors_in_any_zone() {
        let mut state = GameState::new_two_player(42);
        let hand_card = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Hand Creature".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&hand_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let graveyard_card = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Graveyard Creature".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&graveyard_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let battlefield_card = create_object(
            &mut state,
            CardId(12),
            PlayerId(1),
            "Battlefield Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&battlefield_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter = TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::Opponent)
                .properties(vec![FilterProp::InAnyZone {
                    zones: vec![Zone::Hand, Zone::Graveyard],
                }]),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(hand_card)));
        assert!(targets.contains(&TargetRef::Object(graveyard_card)));
        assert!(!targets.contains(&TargetRef::Object(battlefield_card)));
        assert_eq!(targets.len(), 2);
    }

    #[test]
    fn find_legal_targets_any_returns_creatures_and_players() {
        let (state, c0, c1, land) = setup_with_typed_creatures();
        let targets = find_legal_targets(&state, &TargetFilter::Any, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(c0)));
        assert!(targets.contains(&TargetRef::Object(c1)));
        assert!(targets.contains(&TargetRef::Object(land)));
        assert!(targets.contains(&TargetRef::Player(PlayerId(0))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_player_returns_only_players() {
        let (state, _c0, _c1, _land) = setup_with_typed_creatures();
        let targets = find_legal_targets(&state, &TargetFilter::Player, PlayerId(0), ObjectId(99));
        assert_eq!(targets.len(), 2);
        assert!(targets.contains(&TargetRef::Player(PlayerId(0))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_specific_player_returns_only_that_player() {
        let (state, _c0, _c1, _land) = setup_with_typed_creatures();
        let targets = find_legal_targets(
            &state,
            &TargetFilter::SpecificPlayer { id: PlayerId(1) },
            PlayerId(0),
            ObjectId(99),
        );
        assert_eq!(targets, vec![TargetRef::Player(PlayerId(1))]);
    }

    #[test]
    fn find_legal_targets_specific_player_excludes_ineligible_player() {
        let (mut state, _c0, _c1, _land) = setup_with_typed_creatures();
        state.players[1].is_eliminated = true;
        let targets = find_legal_targets(
            &state,
            &TargetFilter::SpecificPlayer { id: PlayerId(1) },
            PlayerId(0),
            ObjectId(99),
        );
        assert!(targets.is_empty());
    }

    /// CR 800.4a: Eliminated players are not legal targets in multiplayer.
    /// Regression: AI was targeting dead opponents in commander multiplayer.
    #[test]
    fn find_legal_targets_excludes_eliminated_player() {
        let (mut state, _c0, _c1, _land) = setup_with_typed_creatures();
        state.players[1].is_eliminated = true;
        state.eliminated_players.push(PlayerId(1));

        let player_targets =
            find_legal_targets(&state, &TargetFilter::Player, PlayerId(0), ObjectId(99));
        assert!(
            !player_targets.contains(&TargetRef::Player(PlayerId(1))),
            "eliminated player must not appear in legal targets"
        );

        let any_targets = find_legal_targets(&state, &TargetFilter::Any, PlayerId(0), ObjectId(99));
        assert!(
            !any_targets.contains(&TargetRef::Player(PlayerId(1))),
            "eliminated player must not appear under TargetFilter::Any either"
        );

        let opponent_filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        let opp_targets = find_legal_targets(&state, &opponent_filter, PlayerId(0), ObjectId(99));
        assert!(
            !opp_targets.contains(&TargetRef::Player(PlayerId(1))),
            "eliminated opponent must not match 'target opponent'"
        );
    }

    #[test]
    fn find_legal_targets_opponent_as_player() {
        let (state, _c0, _c1, _land) = setup_with_typed_creatures();
        let filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert_eq!(targets.len(), 1);
        assert!(targets.contains(&TargetRef::Player(PlayerId(1))));
    }

    #[test]
    fn find_legal_targets_respects_hexproof() {
        let (mut state, _c0, c1, _land) = setup_with_typed_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);
        let filter = TargetFilter::Typed(TypedFilter::creature());
        // Player 0 can't target hexproof creature controlled by player 1
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(!targets.contains(&TargetRef::Object(c1)));
    }

    #[test]
    fn find_legal_targets_card_returns_stack_spells() {
        let (mut state, _c0, _c1, _land) = setup_with_typed_creatures();
        // Add a spell to the stack
        let spell_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(0),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(100),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let filter = TargetFilter::Typed(TypedFilter::card());
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(spell_id)));
    }

    #[test]
    fn find_legal_targets_stack_restriction_excludes_battlefield() {
        use crate::types::ability::FilterProp;
        let (mut state, c0, _c1, _land) = setup_with_typed_creatures();

        // Make c0 an artifact permanent on the battlefield.
        state
            .objects
            .get_mut(&c0)
            .unwrap()
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);

        // Add an artifact spell to the stack.
        let spell_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(1),
            "Artifact Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(200),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let spell_obj = state.objects.get_mut(&spell_id).unwrap();
        spell_obj
            .card_types
            .core_types
            .push(crate::types::card_type::CoreType::Artifact);
        spell_obj.zone = crate::types::zones::Zone::Stack;

        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .properties(vec![FilterProp::InZone { zone: Zone::Stack }]),
        );
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(spell_id)));
        assert!(!targets.contains(&TargetRef::Object(c0)));
    }

    #[test]
    fn aang_airbend_filter_targets_stack_spells_and_other_creatures() {
        use crate::types::ability::Effect;

        let (mut state, source_id, other_creature, land) = setup_with_typed_creatures();
        let spell_id = create_object(
            &mut state,
            CardId(300),
            PlayerId(1),
            "Mightform Harmonizer".to_string(),
            Zone::Stack,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
        }
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(300),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let effect = crate::parser::oracle_effect::parse_effect(
            "airbend up to one other target creature or spell",
        );
        let filter = match effect {
            Effect::ChangeZone { target, .. } => target,
            other => panic!("expected ChangeZone target, got {other:?}"),
        };

        let targets = find_legal_targets(&state, &filter, PlayerId(0), source_id);
        assert!(targets.contains(&TargetRef::Object(other_creature)));
        assert!(targets.contains(&TargetRef::Object(spell_id)));
        assert!(!targets.contains(&TargetRef::Object(source_id)));
        assert!(!targets.contains(&TargetRef::Object(land)));
    }

    #[test]
    fn stack_spell_or_creature_filter_matches_spells_and_creatures_only() {
        let (mut state, source_id, creature, land) = setup_with_typed_creatures();
        let spell_id = create_object(
            &mut state,
            CardId(301),
            PlayerId(1),
            "Stack Spell".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: spell_id,
            source_id: spell_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(301),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let ability_id = create_object(
            &mut state,
            CardId(302),
            PlayerId(1),
            "Stack Ability".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: ability_id,
            source_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::KeywordAction {
                action: crate::types::ability::KeywordAction::Equip {
                    equipment_id: source_id,
                    target_creature_id: creature,
                },
            },
        });

        let filter = TargetFilter::Or {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Typed(TypedFilter::creature()),
            ],
        };
        let targets = find_legal_targets(&state, &filter, PlayerId(0), source_id);

        assert!(targets.contains(&TargetRef::Object(spell_id)));
        assert!(targets.contains(&TargetRef::Object(creature)));
        assert!(!targets.contains(&TargetRef::Object(ability_id)));
        assert!(!targets.contains(&TargetRef::Object(land)));
    }

    #[test]
    fn explicit_stack_zone_composed_stack_spell_filter_matches_instant_spell() {
        let (mut state, source_id, creature, _land) = setup_with_typed_creatures();
        let instant_id = create_object(
            &mut state,
            CardId(303),
            PlayerId(1),
            "Instant Spell".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&instant_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: instant_id,
            source_id: instant_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(303),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let sorcery_id = create_object(
            &mut state,
            CardId(304),
            PlayerId(1),
            "Sorcery Spell".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&sorcery_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: sorcery_id,
            source_id: sorcery_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::Spell {
                card_id: CardId(304),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let ability_id = create_object(
            &mut state,
            CardId(305),
            PlayerId(1),
            "Stack Ability".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: ability_id,
            source_id,
            controller: PlayerId(1),
            kind: crate::types::game_state::StackEntryKind::KeywordAction {
                action: crate::types::ability::KeywordAction::Equip {
                    equipment_id: source_id,
                    target_creature_id: creature,
                },
            },
        });

        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::StackSpell,
                TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Instant)
                        .properties(vec![FilterProp::InZone { zone: Zone::Stack }]),
                ),
            ],
        };
        let targets = find_legal_targets(&state, &filter, PlayerId(0), source_id);

        assert!(targets.contains(&TargetRef::Object(instant_id)));
        assert!(!targets.contains(&TargetRef::Object(sorcery_id)));
        assert!(!targets.contains(&TargetRef::Object(ability_id)));
    }

    #[test]
    fn find_legal_targets_graveyard_finds_graveyard_cards() {
        let mut state = GameState::new_two_player(42);

        // Card in player 0's graveyard
        let gy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Dead Bear".to_string(),
            Zone::Graveyard,
        );
        state
            .objects
            .get_mut(&gy_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Card on battlefield (should NOT be found)
        let bf_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Live Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bf_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(gy_card)));
        assert!(!targets.contains(&TargetRef::Object(bf_card)));
    }

    #[test]
    fn find_legal_targets_graveyard_excludes_battlefield() {
        let mut state = GameState::new_two_player(42);

        let bf_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bf_card)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.is_empty());
    }

    #[test]
    fn protection_blocks_graveyard_targeting() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = GameState::new_two_player(42);

        let gy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Protected Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_card).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords
                .push(Keyword::Protection(ProtectionTarget::Color(ManaColor::Red)));
        }

        // Red source trying to target graveyard card
        let red_source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Red Spell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&red_source)
            .unwrap()
            .color
            .push(ManaColor::Red);

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), red_source);
        assert!(!targets.contains(&TargetRef::Object(gy_card)));
    }

    #[test]
    fn hexproof_does_not_block_graveyard_targeting() {
        let mut state = GameState::new_two_player(42);

        let gy_card = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hexproof Creature".to_string(),
            Zone::Graveyard,
        );
        {
            let obj = state.objects.get_mut(&gy_card).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Hexproof);
        }

        let filter =
            TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::InZone {
                zone: Zone::Graveyard,
            }]));
        // Opponent (player 0) CAN target hexproof card in graveyard
        let targets = find_legal_targets(&state, &filter, PlayerId(0), ObjectId(99));
        assert!(targets.contains(&TargetRef::Object(gy_card)));
    }

    #[test]
    fn extract_player_from_damage_dealt_returns_damaged_player() {
        // CR 603.7c: "that player" for DamageDone triggers refers to the damaged player.
        let state = GameState::new_two_player(42);
        let event = crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(10),
            target: TargetRef::Player(PlayerId(1)),
            amount: 3,
            is_combat: true,
            excess: 0,
        };
        let result = extract_player_from_event(&event, &state);
        // Should return the damaged player (PlayerId(1)), not the source's controller.
        assert_eq!(result, Some(PlayerId(1)));
    }

    #[test]
    fn extract_player_from_damage_dealt_to_creature_returns_controller() {
        // When damage targets a creature, "that player" resolves to the creature's controller.
        let mut state = GameState::new_two_player(42);
        let creature_id = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let event = crate::types::events::GameEvent::DamageDealt {
            source_id: ObjectId(10),
            target: TargetRef::Object(creature_id),
            amount: 2,
            is_combat: false,
            excess: 0,
        };
        let result = extract_player_from_event(&event, &state);
        assert_eq!(result, Some(PlayerId(1)));
    }

    /// CR 603.6 + CR 109.4 + CR 603.10a: For `ZoneChanged` events
    /// (ETB, dies, discard, return-to-hand), `TriggeringPlayer` must
    /// resolve to the moving object's controller as captured in the
    /// `ZoneChangeRecord` snapshot — NOT the ability controller and
    /// NOT `None`. Regression discriminator for #546 (Bloodchief
    /// Ascension) and #560 (Suture Priest), where the wildcard arm's
    /// `None` fallback caused `LoseLife { target: TriggeringPlayer }`
    /// to revert to the Suture Priest / Bloodchief controller via
    /// `resolve_player_for_context_ref`'s ability-controller fallback,
    /// damaging the wrong player.
    ///
    /// Table-driven across ETB (None→Battlefield), dies
    /// (Battlefield→Graveyard), and discard (Hand→Graveyard) so a
    /// future arm that accidentally discriminates by `from_zone` would
    /// be caught.
    #[test]
    fn extract_player_from_zone_change_returns_moving_objects_controller() {
        use crate::types::events::GameEvent;
        use crate::types::game_state::ZoneChangeRecord;
        use crate::types::zones::Zone;

        let state = GameState::new_two_player(42);

        for (label, from, to) in [
            (
                "ETB (Suture Priest #560 opponent creature enters)",
                None,
                Zone::Battlefield,
            ),
            (
                "Dies (Bloodchief Ascension #546 battlefield→graveyard)",
                Some(Zone::Battlefield),
                Zone::Graveyard,
            ),
            (
                "Discard (Bloodchief Ascension #546 hand→graveyard)",
                Some(Zone::Hand),
                Zone::Graveyard,
            ),
        ] {
            let record = ZoneChangeRecord {
                controller: PlayerId(1),
                ..ZoneChangeRecord::test_minimal(ObjectId(7), from, to)
            };
            let event = GameEvent::ZoneChanged {
                object_id: ObjectId(7),
                from,
                to,
                record: Box::new(record),
            };
            let result = extract_player_from_event(&event, &state);
            assert_eq!(
                result,
                Some(PlayerId(1)),
                "{label}: ZoneChanged must surface the moving object's controller (was: {result:?})",
            );
        }
    }

    /// End-to-end integration discriminator through the resolver chain
    /// `resolve_player_for_context_ref → resolve_event_context_target →
    /// extract_player_from_event` — the actual code path the bug report
    /// hit. Pre-fix the inner helper returned `None` for `ZoneChanged`,
    /// the outer resolver fell back through to `ability.controller`,
    /// and Suture Priest's "its controller loses 1 life" deducted from
    /// the Priest's owner (P0) rather than the entering creature's
    /// controller (P1). Post-fix the chain surfaces P1.
    ///
    /// This is the SUTURE-PRIEST scenario from #560 in miniature: a
    /// `LoseLife` ability owned by P0, triggered by a ZoneChanged event
    /// whose record's controller is P1. The assertion proves the
    /// resolver routes the life loss to P1, not P0. Reverting the
    /// new `ZoneChanged` arm in `extract_player_from_event` makes this
    /// test return `PlayerId(0)` and the assertion fires.
    #[test]
    fn resolve_player_for_context_ref_uses_zone_change_controller_not_ability_controller() {
        use crate::game::effects::resolve_player_for_context_ref;
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
        use crate::types::events::GameEvent;
        use crate::types::game_state::ZoneChangeRecord;
        use crate::types::zones::Zone;

        let mut state = GameState::new_two_player(42);
        // Suture Priest is controlled by P0; its trigger says "opponent's
        // creature entered, its controller loses 1 life." The entering
        // creature is controlled by P1. The trigger event must carry the
        // entering controller (P1) in the record.
        let suture_priest_id = ObjectId(100);
        let entering_creature_id = ObjectId(200);
        let record = ZoneChangeRecord {
            controller: PlayerId(1),
            ..ZoneChangeRecord::test_minimal(entering_creature_id, None, Zone::Battlefield)
        };
        state.current_trigger_event = Some(GameEvent::ZoneChanged {
            object_id: entering_creature_id,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(record),
        });

        // The LoseLife effect with `target: TriggeringPlayer` is the
        // shape that Suture Priest's second trigger lowers to. Build a
        // ResolvedAbility for it whose `controller` is P0 (the Priest's
        // owner) — the asymmetry between ability.controller (P0) and
        // the record's controller (P1) is what discriminates the fix.
        let ability = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::TriggeringPlayer),
            },
            Vec::new(),
            suture_priest_id,
            PlayerId(0),
        );

        let resolved =
            resolve_player_for_context_ref(&state, &ability, &TargetFilter::TriggeringPlayer);
        assert_eq!(
            resolved,
            PlayerId(1),
            "TriggeringPlayer on a ZoneChanged event must resolve to the entering \
             creature's controller (P1), not the Suture Priest controller (P0)",
        );
    }

    #[test]
    fn extract_player_from_player_action_returns_acting_player() {
        let state = GameState::new_two_player(42);
        let event = crate::types::events::GameEvent::PlayerPerformedAction {
            player_id: PlayerId(1),
            action: crate::types::events::PlayerActionKind::Scry,
        };
        let result = extract_player_from_event(&event, &state);
        assert_eq!(result, Some(PlayerId(1)));
    }

    // --- CR 702.11d: HexproofFrom targeting tests ---

    #[test]
    fn hexproof_from_color_prevents_opponent_targeting() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        // Give c1 (player 1's creature) hexproof from red
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)));

        // Create a red source spell on the stack controlled by player 0
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Red);

        // Player 0 (opponent) targeting c1 with a red source — should fail
        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));
    }

    #[test]
    fn hexproof_from_color_allows_non_matching_opponent_targeting() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)));

        // Create a blue source
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Counterspell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Blue);

        // Player 0 targeting c1 with a blue source — should succeed
        let obj = state.objects.get(&c1).unwrap();
        assert!(can_target(obj, PlayerId(0), source_id, &state));
    }

    #[test]
    fn hexproof_from_color_allows_controller_targeting() {
        use crate::types::keywords::HexproofFilter;
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Red)));

        // Create a red source controlled by the same player (player 1)
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(1),
            "Own Red Spell".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Red);

        // Controller targeting own creature — should succeed regardless
        let obj = state.objects.get(&c1).unwrap();
        assert!(can_target(obj, PlayerId(1), source_id, &state));
    }

    #[test]
    fn hexproof_filter_matches_card_type() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::CardType(
                "artifacts".to_string(),
            )));

        // Create an artifact source
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Artifact Source".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);

        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));
    }

    #[test]
    fn hexproof_filter_matches_monocolored() {
        use crate::types::mana::ManaColor;

        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::HexproofFrom(HexproofFilter::Quality(
                "monocolored".to_string(),
            )));

        // Monocolored source (exactly 1 color)
        let source_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Mono Red".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .color
            .push(ManaColor::Red);

        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));

        // Multicolored source — NOT blocked by "hexproof from monocolored"
        let multi_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Multi Source".to_string(),
            Zone::Battlefield,
        );
        {
            let multi = state.objects.get_mut(&multi_id).unwrap();
            multi.color.push(ManaColor::Red);
            multi.color.push(ManaColor::Blue);
        }
        let obj = state.objects.get(&c1).unwrap();
        assert!(can_target(obj, PlayerId(0), multi_id, &state));
    }

    #[test]
    fn protection_from_instants_blocks_targeting() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::CardType(
                "instants".to_string(),
            )));

        let source_id = create_object(
            &mut state,
            CardId(102),
            PlayerId(0),
            "Shock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), source_id, &state));
    }

    #[test]
    fn protection_from_mana_value_filter_blocks_targeting() {
        let (mut state, _c0, c1) = setup_with_creatures();
        state
            .objects
            .get_mut(&c1)
            .unwrap()
            .keywords
            .push(Keyword::Protection(ProtectionTarget::Filter(
                TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
                    comparator: Comparator::LE,
                    value: QuantityExpr::Fixed { value: 3 },
                }])),
            )));

        let low_mv_source = create_object(
            &mut state,
            CardId(103),
            PlayerId(0),
            "Small Spell".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&low_mv_source).unwrap().mana_cost =
            crate::types::mana::ManaCost::Cost {
                generic: 3,
                shards: vec![],
            };

        let high_mv_source = create_object(
            &mut state,
            CardId(104),
            PlayerId(0),
            "Large Spell".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&high_mv_source).unwrap().mana_cost =
            crate::types::mana::ManaCost::Cost {
                generic: 4,
                shards: vec![],
            };

        let obj = state.objects.get(&c1).unwrap();
        assert!(!can_target(obj, PlayerId(0), low_mv_source, &state));
        assert!(can_target(obj, PlayerId(0), high_mv_source, &state));
    }

    /// CR 702.16b + CR 702.16j: A player with protection from everything
    /// cannot be a legal target of any spell or ability from any source.
    /// `find_legal_targets` must exclude that player from the "any target"
    /// scan.
    #[test]
    fn find_legal_targets_excludes_player_protection_from_everything() {
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source Spell".to_string(),
            Zone::Battlefield,
        );
        // Protect PlayerId(1) via a transient continuous effect.
        state.add_transient_continuous_effect(
            source,
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );

        // "any target" should list PlayerId(0) (unprotected) but not PlayerId(1).
        let targets = find_legal_targets(&state, &TargetFilter::Any, PlayerId(0), source);
        assert!(
            targets.contains(&TargetRef::Player(PlayerId(0))),
            "PlayerId(0) should be a legal target, got {:?}",
            targets
        );
        assert!(
            !targets.contains(&TargetRef::Player(PlayerId(1))),
            "PlayerId(1) has protection from everything — must NOT be targetable, got {:?}",
            targets
        );
    }

    /// CR 702.16b + CR 702.16j: "target opponent" (Typed filter with no
    /// type_filters and ControllerRef::Opponent) must also exclude a protected
    /// opponent — verifies the typed-player-target branch was updated.
    #[test]
    fn find_legal_targets_typed_opponent_excludes_protected_player() {
        use crate::types::ability::{ContinuousModification, ControllerRef, Duration, TypedFilter};
        use crate::types::keywords::Keyword;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source Spell".to_string(),
            Zone::Battlefield,
        );
        state.add_transient_continuous_effect(
            source,
            PlayerId(1),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(ProtectionTarget::Everything),
            }],
            None,
        );

        let filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));
        let targets = find_legal_targets(&state, &filter, PlayerId(0), source);
        assert!(
            !targets.contains(&TargetRef::Player(PlayerId(1))),
            "protected opponent must not be a legal target, got {:?}",
            targets
        );
    }

    /// CR 102.3 + CR 115.9c: In team multiplayer, "target opponent" excludes
    /// teammates and includes opposing-team players.
    #[test]
    fn find_legal_targets_typed_opponent_excludes_two_headed_giant_teammate() {
        use crate::types::ability::{ControllerRef, TypedFilter};
        use crate::types::format::FormatConfig;

        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source Spell".to_string(),
            Zone::Battlefield,
        );
        let filter =
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent));

        let targets = find_legal_targets(&state, &filter, PlayerId(0), source);
        assert!(
            !targets.contains(&TargetRef::Player(PlayerId(1))),
            "teammate must not be a legal target opponent, got {:?}",
            targets
        );
        assert!(targets.contains(&TargetRef::Player(PlayerId(2))));
        assert!(targets.contains(&TargetRef::Player(PlayerId(3))));
    }

    fn make_resolved_with_targets(
        targets: Vec<TargetRef>,
        source: ObjectId,
    ) -> crate::types::ability::ResolvedAbility {
        crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Draw {
                count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            targets,
            source,
            PlayerId(0),
        )
    }

    /// CR 109.5 + CR 701.55a: A villainous-choice "you …" branch is resolved
    /// with `controller = source controller` and `scoped_player = the chooser`
    /// (an opponent). "you"/`Controller` must resolve to the controller, not to
    /// the chooser bound as `scoped_player`; "that player"/`ScopedPlayer` still
    /// resolves to the chooser. Pre-fix, `Controller` read
    /// `scoped_player.unwrap_or(controller)`, so a "you" branch acted on the
    /// opponent who made the choice.
    #[test]
    fn controller_player_ref_ignores_scoped_player() {
        let state = GameState::new_two_player(7);
        let mut ability = make_resolved_with_targets(vec![], ObjectId(1));
        // controller is PlayerId(0) (the source's controller).
        ability.scoped_player = Some(PlayerId(1)); // the opponent who chose the branch
        assert_eq!(
            resolve_effect_player_ref(&state, &ability, &TargetFilter::Controller),
            Some(PlayerId(0)),
            "\"you\" must resolve to the controller, not the chooser bound as scoped_player"
        );
        assert_eq!(
            resolve_effect_player_ref(&state, &ability, &TargetFilter::ScopedPlayer),
            Some(PlayerId(1)),
            "\"that player\" must still resolve to the scoped chooser"
        );
    }

    /// CR 608.2c + 603.10a: Tier 1 — `SelfRef` with empty `ability.targets`
    /// resolves to the source object (the parser's `~` anaphor).
    #[test]
    fn resolved_targets_self_ref_with_empty_targets_returns_source() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );
        let ability = make_resolved_with_targets(vec![], source);
        let result = resolved_targets(&ability, &TargetFilter::SelfRef, &state);
        assert_eq!(
            result,
            vec![TargetRef::Object(source)],
            "SelfRef + empty targets should resolve to source object"
        );
    }

    /// CR 506.2: Tier 2 — event-context filters like `DefendingPlayer` resolve
    /// from game state (here, `state.combat.attackers`) without consuming
    /// `ability.targets`. Verifies the helper routes through the event-context
    /// tier when it applies and returns its target.
    #[test]
    fn resolved_targets_event_context_resolves_from_combat_state() {
        use crate::game::combat::{AttackTarget, AttackerInfo};
        let (mut state, _c0, c1) = setup_with_creatures();
        // Mark c1 as attacking player 0 so DefendingPlayer resolves to player 0.
        let combat = state.combat.get_or_insert_with(Default::default);
        combat.attackers.push(AttackerInfo::new(
            c1,
            AttackTarget::Player(PlayerId(0)),
            PlayerId(0),
        ));
        let ability = make_resolved_with_targets(vec![], c1);
        let result = resolved_targets(&ability, &TargetFilter::DefendingPlayer, &state);
        assert_eq!(
            result,
            vec![TargetRef::Player(PlayerId(0))],
            "DefendingPlayer should resolve to the attacked player"
        );
    }

    /// CR 608.2c (issue #323): `SelfRef` always resolves to the source object,
    /// even when `ability.targets` is non-empty. The chained "Exile ~"
    /// sub-ability of cards like Treasured Find / Arc Blade gets its
    /// `targets` populated by the chain target-propagation in
    /// `effects::mod.rs::resolve_chain` (it copies the parent's targets when
    /// the sub's targets are empty). Without the SelfRef short-circuit, the
    /// sub-ability would target the parent's chosen object instead of the
    /// source, exiling the wrong thing.
    #[test]
    fn resolved_targets_self_ref_overrides_propagated_parent_targets() {
        let (mut state, c0, c1) = setup_with_creatures();
        // Source = c0; ability.targets = [c1] (simulating the parent's chosen
        // bounce target propagated into the sub-ability via the chain
        // target-propagation in effects::mod.rs).
        let ability = make_resolved_with_targets(vec![TargetRef::Object(c1)], c0);
        let result = resolved_targets(&ability, &TargetFilter::SelfRef, &state);
        assert_eq!(
            result,
            vec![TargetRef::Object(c0)],
            "SelfRef must always resolve to source, not the propagated parent target"
        );
        // Suppress unused-variable warning when setup_with_creatures changes.
        let _ = &mut state;
    }

    /// CR 509.1g + CR 608.2c: for "When this creature blocks a creature,
    /// destroy that creature", `ParentTarget` resolves to the blocked attacker
    /// carried by the split `BlockersDeclared` trigger event.
    #[test]
    fn resolved_targets_parent_target_for_block_event_returns_blocked_attacker() {
        let (mut state, blocker, attacker) = setup_with_creatures();
        state.current_trigger_event = Some(crate::types::events::GameEvent::BlockersDeclared {
            assignments: vec![(blocker, attacker)],
        });
        let ability = make_resolved_with_targets(vec![], blocker);

        let result = resolved_targets(&ability, &TargetFilter::ParentTarget, &state);

        assert_eq!(result, vec![TargetRef::Object(attacker)]);
    }

    /// CR 601.2c: Tier 3 — when neither self-ref nor event-context applies,
    /// fall through to the ability's pre-selected targets.
    #[test]
    fn resolved_targets_falls_back_to_ability_targets() {
        let (state, _c0, c1) = setup_with_creatures();
        // Use `Any` filter (not self-ref-eligible) and supply a chosen target.
        let ability = make_resolved_with_targets(vec![TargetRef::Object(c1)], c1);
        let result = resolved_targets(&ability, &TargetFilter::Any, &state);
        assert_eq!(
            result,
            vec![TargetRef::Object(c1)],
            "Should fall through to ability.targets when no other tier applies"
        );
    }

    /// CR 706.2: a die roll's result is the amount `EventContextAmount`
    /// resolves "where X is the result" against.
    #[test]
    fn extract_amount_from_die_rolled_returns_result() {
        let event = crate::types::events::GameEvent::DieRolled {
            player_id: PlayerId(0),
            sides: 8,
            result: 7,
        };
        assert_eq!(extract_amount_from_event(&event), Some(7));
    }

    /// CR 602.2a: For Burning-Tree Shaman / Flamescroll Celebrant's "deals 1
    /// damage to that player" effect, `TriggeringPlayer` must resolve to the
    /// player who activated the ability — carried directly on the event, not
    /// inferred from the source object's controller (which would be wrong
    /// when an opponent activates a granted ability).
    #[test]
    fn extract_player_from_ability_activated_returns_activator() {
        let (state, _c0, _c1) = setup_with_creatures();
        let event = crate::types::events::GameEvent::AbilityActivated {
            player_id: PlayerId(1),
            source_id: ObjectId(99),
        };
        assert_eq!(extract_player_from_event(&event, &state), Some(PlayerId(1)));
    }
}
