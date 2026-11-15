//! CR 702.26: Phase Out / Phase In resolvers for the `Effect::PhaseOut` and
//! `Effect::PhaseIn` variants. All phasing primitives live in
//! `game::phasing`; this module is the thin effect-handler glue that
//! dispatches resolved targets to those primitives and emits the
//! `EffectResolved` bookkeeping event.
//!
//! Both resolvers handle player and object targets in a single pass:
//! explicit `TargetRef::Player` targets and player-typed mass filters
//! (`Controller`, `Player`, `Typed { type_filters: [], … }`) route through
//! `phase_out_player`/`phase_in_player`; everything else routes through the
//! permanent path (CR 702.26 proper). Player phasing has no formal CR rule
//! and follows the small set of card Oracle text that says "you phase out".

use std::collections::HashSet;

use crate::game::filter::{
    matches_target_filter, matches_target_filter_including_phased_out, FilterContext,
};
use crate::game::game_object::{ObjectIncarnationRefExt, PhaseOutCause};
use crate::game::phasing::{phase_in_object, phase_in_player, phase_out_object, phase_out_player};
use crate::types::ability::{
    phase_out_filter_is_context_union, Effect, EffectError, EffectKind, ResolvedAbility,
    TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};

/// CR 702.26a: Resolve `Effect::PhaseOut` by phasing out every targeted
/// permanent (or every permanent matching the effect's mass filter, e.g.
/// "All permanents you control phase out" from Teferi's Protection). Phased-
/// out objects remain on the battlefield (CR 702.26d); we delegate to
/// `phase_out_object` which also cascades to indirectly-phased attachments
/// and removes everything from combat (CR 506.4 + CR 702.26g).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::PhaseOut { target } => target.clone(),
        _ => return Ok(()),
    };

    // Player-phasing branch. Mirrors `collect_object_targets` for the
    // permanent path: explicit `TargetRef::Player` targets win, then a
    // player-typed mass filter (`Controller`, `Typed { type_filters: [], … }`,
    // `Player`) expands to the matching set of player ids. This dispatches
    // before the object branch so a player target never silently becomes a
    // no-op via `collect_object_targets`.
    let player_targets =
        crate::game::ability_utils::collect_player_targets(state, ability, &target);
    for pid in &player_targets {
        phase_out_player(state, *pid, events);
    }

    let object_targets = collect_object_targets(state, ability, &target);
    let target_set: HashSet<ObjectId> = object_targets.iter().copied().collect();
    for oid in object_targets {
        // CR 702.26h: attachments whose host is also in this mass set phase out
        // only indirectly via the host's CR 702.26g cascade, not as direct targets.
        if attachment_host_in_set(state, oid, &target_set) {
            continue;
        }
        phase_out_object(state, oid, PhaseOutCause::Directly, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PhaseOut,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// CR 702.26c: Resolve `Effect::PhaseIn` by phasing in every targeted
/// permanent. Rare; most phasing-in happens during the untap-step TBA.
pub fn resolve_phase_in(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let target = match &ability.effect {
        Effect::PhaseIn { target } => target.clone(),
        _ => return Ok(()),
    };

    // Player-phasing branch — same idiom as `resolve` for symmetry. Phased-out
    // players don't appear in the targeting choke point, so callers wanting
    // to phase them back in must use an explicit `TargetRef::Player` target
    // (or a player-typed mass filter such as `Controller`).
    let player_targets =
        crate::game::ability_utils::collect_player_targets(state, ability, &target);
    for pid in &player_targets {
        phase_in_player(state, *pid, events);
    }

    // CR 400.7 + CR 603.7c + CR 603.7b: the trigger fired and resolved; it
    // affected nothing. ALTITUDE IS LOAD-BEARING — this guard cannot live in
    // `collect_phase_in_targets`, which returns a bare `Vec<ObjectId>` with no
    // `events` in scope and, worse, converts an emptied target list into a
    // CR 702.26b battlefield-wide sweep for phased-out permanents matching the
    // filter. That is a POOL-SCAN FALLBACK binding different objects, so the
    // guard is hoisted here, above the call that would perform it.
    //
    // Placed AFTER the player-phasing branch deliberately:
    // `pinned_object_targets_all_stale` is scoped to object refs, and
    // `live_object_targets` passes `TargetRef::Player` through by construction.
    // Guarding above the player branch would suppress a legitimate player
    // phase-in on an ability that carries both a player ref and a stale object
    // ref.
    if ability.pinned_object_targets_all_stale(state) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::PhaseIn,
            source_id: ability.source_id,
            subject: None,
        });
        return Ok(());
    }

    // CR 702.26b: Filter choke point normally excludes phased-out objects, so
    // we can't rely on the standard target expansion for phase-in. Instead,
    // enumerate state.battlefield directly and match the filter manually,
    // skipping the phased-out exclusion.
    let targets = collect_phase_in_targets(state, ability, &target);
    for oid in targets {
        // CR 101.2: an explicit "phase in" effect can't override an active
        // "can't phase in" restriction. The restriction is condition-gated
        // (CR 611.2b), so The Pandorica's own delayed-trigger phase-in — which
        // resolves only after the source has untapped or left, when the lock has
        // lapsed — passes, while any phase-in attempted while the lock is active
        // is a no-op.
        if crate::game::static_abilities::object_has_active_cant_phase_in(state, oid) {
            continue;
        }
        phase_in_object(state, oid, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::PhaseIn,
        source_id: ability.source_id,
        subject: None,
    });
    Ok(())
}

/// True when `oid` is attached to another permanent that is also in `targets`.
fn attachment_host_in_set(state: &GameState, oid: ObjectId, targets: &HashSet<ObjectId>) -> bool {
    state
        .objects
        .get(&oid)
        .and_then(|obj| obj.attached_to.as_ref())
        .and_then(|t| t.as_object())
        .is_some_and(|host| targets.contains(&host))
}

/// CR 509.1 + CR 608.2k: a blocking trigger's "that creature" is its combat
/// partner. The event-only parent reader owns both blocking orientations.
fn phase_out_event_referent(
    state: &GameState,
    source_id: ObjectId,
    event: &GameEvent,
) -> Option<ObjectId> {
    let filter = match event {
        GameEvent::BlockersDeclared { .. }
        | GameEvent::AttackerBecameBlockedByFilteredBlocker { .. } => TargetFilter::ParentTarget,
        _ => TargetFilter::TriggeringSource,
    };
    crate::game::targeting::resolve_event_context_target_for_event_or_state(
        state,
        &filter,
        source_id,
        Some(event),
    )
    .and_then(|target| match target {
        TargetRef::Object(id) => Some(id),
        TargetRef::Player(_) => None,
    })
}

fn uses_event_context_pins(ability: &ResolvedAbility) -> bool {
    fn uses_triggering_source(filter: &TargetFilter) -> bool {
        match filter {
            TargetFilter::Or { filters } => filters.iter().any(uses_triggering_source),
            other => matches!(other, TargetFilter::TriggeringSource),
        }
    }
    matches!(
        &ability.effect,
        Effect::PhaseOut {
            target: target @ TargetFilter::Or { .. },
        } if phase_out_filter_is_context_union(target) && uses_triggering_source(target)
    )
}

/// CR 400.7 + CR 603.3 + CR 608.2k: capture event referents before response
/// priority, independently of announced targets. Existing serialized context
/// pins preserve these identities across saves and non-declaring continuations.
pub(crate) fn seed_event_context_pins(
    state: &GameState,
    ability: &mut ResolvedAbility,
    event: &GameEvent,
) {
    if uses_event_context_pins(ability) {
        if let Some(pin) = phase_out_event_referent(state, ability.source_id, event)
            .and_then(|id| state.objects.get(&id))
            .map(ObjectIncarnationRef::from_object)
        {
            let existing = ability
                .target_incarnations
                .iter()
                .position(|captured| captured.object_id == pin.object_id);
            if existing.is_none_or(|index| {
                ability.target_incarnations[index].incarnation
                    == crate::types::identifiers::LEGACY_INCARNATION
            }) {
                let captured = ability
                    .target_incarnations
                    .iter()
                    .find(|captured| {
                        captured.object_id == pin.object_id
                            && captured.incarnation != crate::types::identifiers::LEGACY_INCARNATION
                    })
                    .copied()
                    .unwrap_or(pin);
                let mut pins = ability.target_incarnations.clone();
                if let Some(index) = existing {
                    pins[index] = captured;
                } else {
                    pins.push(captured);
                }
                ability.set_target_incarnations_recursive(pins);
            }
        }
    }
    for (_, child) in ability.child_links_mut() {
        seed_event_context_pins(state, child, event);
    }
}

/// CR 400.7 + CR 608.2k: a live raw id cannot reconstruct a lost event
/// incarnation. Retain exact stale pins, but refuse ambiguous live referents.
pub(crate) fn validate_event_context_pins(
    state: &GameState,
    ability: &ResolvedAbility,
    event: Option<&GameEvent>,
) -> Result<(), String> {
    let Some(event) = event else {
        return Ok(());
    };
    if uses_event_context_pins(ability) {
        if let Some(partner) =
            phase_out_event_referent(state, ability.source_id, event).filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|object| object.zone == crate::types::zones::Zone::Battlefield)
            })
        {
            if !ability
                .target_incarnations
                .iter()
                .find(|pin| pin.object_id == partner)
                .is_some_and(|pin| pin.incarnation != crate::types::identifiers::LEGACY_INCARNATION)
            {
                return Err(format!(
                    "phase-out event context lacks a captured incarnation for {partner:?}"
                ));
            }
        }
    }
    for (_, child) in ability.child_links() {
        validate_event_context_pins(state, child, Some(event))?;
    }
    Ok(())
}

/// Resolve the target object set for a `PhaseOut` effect. Explicit
/// `ability.targets` (from the targeting phase) take precedence; mass filters
/// (e.g., `Typed Permanent / You`) are expanded against the battlefield.
pub(super) fn collect_object_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    match target {
        TargetFilter::Or { filters } if phase_out_filter_is_context_union(target) => {
            // CR 702.26a + CR 702.26h: gather the whole batch before phasing any
            // member, so an attachment selected with its host phases indirectly.
            let mut seen = HashSet::new();
            let ctx = FilterContext::from_ability(ability);
            return filters
                .iter()
                .flat_map(|filter| {
                    match filter {
                        // CR 201.5 + CR 400.7: source identity is independent of the
                        // announced target list, including a later same-ID incarnation.
                        TargetFilter::SelfRef => {
                            if ability.self_ref_is_current(state)
                                && state
                                    .battlefield_phased_in_ids()
                                    .contains(&ability.source_id)
                            {
                                vec![ability.source_id]
                            } else {
                                Vec::new()
                            }
                        }
                        // Preserve empty parent bindings and declared slot numbers:
                        // the shared object reader selects a slot before checking its
                        // legality and incarnation, never after filtering the list.
                        TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. } => {
                            crate::game::targeting::resolved_object_ids_for_filter_with_context(
                                state, ability, filter, &ctx,
                            )
                        }
                        // CR 608.2k: the trigger's named combat partner is an
                        // untargeted event referent, not an announced target or a
                        // population filter. CR 702.26b: only phased-in battlefield
                        // objects can join this phase-out batch.
                        TargetFilter::TriggeringSource => {
                            state
                                .current_trigger_event
                                .as_ref()
                                .and_then(|event| {
                                    phase_out_event_referent(state, ability.source_id, event)
                                })
                                .filter(|id| {
                                    // CR 400.7: missing capture is not authority to bind a
                                    // current incarnation here, after response priority.
                                    ability
                                        .target_incarnations
                                        .iter()
                                        .find(|pin| pin.object_id == *id)
                                        .is_some_and(|pin| pin.is_current(state))
                                })
                                .filter(|id| {
                                    state.objects.get(id).is_some_and(|object| {
                                        object.zone == crate::types::zones::Zone::Battlefield
                                            && object.is_phased_in()
                                    })
                                })
                                .into_iter()
                                .collect()
                        }
                        TargetFilter::Or { .. } => collect_object_targets(state, ability, filter),
                        TargetFilter::None
                        | TargetFilter::Any
                        | TargetFilter::Player
                        | TargetFilter::Controller
                        | TargetFilter::SourceController
                        | TargetFilter::ControllerAndControlledPermanents { .. }
                        | TargetFilter::Opponent
                        | TargetFilter::GrantingObject { .. }
                        | TargetFilter::SourceOrPaired
                        | TargetFilter::Typed(_)
                        | TargetFilter::Not { .. }
                        | TargetFilter::And { .. }
                        | TargetFilter::StackAbility { .. }
                        | TargetFilter::StackSpell
                        | TargetFilter::CastingSpell
                        | TargetFilter::SpecificObject { .. }
                        | TargetFilter::SpecificPlayer { .. }
                        | TargetFilter::PlayerWhoChoseLabel { .. }
                        | TargetFilter::PlayerMatching { .. }
                        | TargetFilter::Neighbor { .. }
                        | TargetFilter::ScopedPlayer
                        | TargetFilter::AttachedTo
                        | TargetFilter::LastCreated
                        | TargetFilter::LastRevealed
                        | TargetFilter::LastZoneChanged
                        | TargetFilter::CostPaidObject
                        | TargetFilter::AmassedArmy
                        | TargetFilter::ChosenCard
                        | TargetFilter::TrackedSet { .. }
                        | TargetFilter::TrackedSetFiltered { .. }
                        | TargetFilter::ExiledBySource
                        | TargetFilter::ExiledCardByIndex { .. }
                        | TargetFilter::TriggeringSpellController
                        | TargetFilter::TriggeringSpellOwner
                        | TargetFilter::TriggeringPlayer
                        | TargetFilter::AttackedPlayerOrPlaneswalker
                        | TargetFilter::EventTarget
                        | TargetFilter::TriggeringSourceController
                        | TargetFilter::EventTargetController
                        | TargetFilter::DeclaredPlayer { .. }
                        | TargetFilter::ParentTargetController
                        | TargetFilter::ParentTargetOwner
                        | TargetFilter::AffectedObjectOwner
                        | TargetFilter::SourceChosenPlayer
                        | TargetFilter::OriginalController
                        | TargetFilter::OriginalSource
                        | TargetFilter::PostReplacementSourceController
                        | TargetFilter::PostReplacementDamageSource
                        | TargetFilter::PostReplacementDamageTarget
                        | TargetFilter::PostReplacementDamageTargetOwner
                        | TargetFilter::DefendingPlayer
                        | TargetFilter::HasChosenName
                        | TargetFilter::ChosenDamageSource { .. }
                        | TargetFilter::Named { .. }
                        | TargetFilter::Owner
                        | TargetFilter::AllPlayers => Vec::new(),
                    }
                })
                .filter(|id| seen.insert(*id))
                .collect();
        }
        _ => {}
    }
    let from_targets: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    // CR 400.7: a self-reference cannot phase out a later same-ID incarnation.
    // Explicit targets retain precedence; other filters retain their mass lookup.
    if matches!(target, TargetFilter::SelfRef) && !ability.self_ref_is_current(state) {
        return Vec::new();
    }

    // Mass filter — expand against the phased-in battlefield.
    let ctx = FilterContext::from_ability(ability);
    state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| matches_target_filter(state, *id, target, &ctx))
        .collect()
}

/// Resolve target object set for a `PhaseIn` effect. Because the filter
/// choke point treats phased-out objects as nonexistent, we iterate
/// `state.battlefield` directly and evaluate only the non-phased-out aspects
/// of the filter here.
fn collect_phase_in_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    target: &TargetFilter,
) -> Vec<ObjectId> {
    // CR 400.7 + CR 603.7c: drop pinned referents that became new objects. This
    // is a RAW read — `resolve_phase_in` never calls `resolved_targets`, so the
    // targeting chokepoint cannot see this pin.
    //
    // Substitution here handles the PARTIALLY-stale case (some referents live,
    // some not). It deliberately does NOT handle the all-stale case, because
    // emptying `from_targets` falls through to the CR 702.26b battlefield sweep
    // below, which would phase in a DIFFERENT set of permanents. That case is
    // caught by the `pinned_object_targets_all_stale` early return hoisted into
    // `resolve_phase_in` — see the comment there for why the guard cannot live
    // in this function.
    let from_targets: Vec<ObjectId> = ability
        .live_object_targets(state)
        .iter()
        .filter_map(|t| match t {
            TargetRef::Object(id) => Some(*id),
            TargetRef::Player(_) => None,
        })
        .collect();
    if !from_targets.is_empty() {
        return from_targets;
    }

    // CR 702.26b: phasing-in is one of the rare effects that specifically
    // mentions phased-out permanents, so the effect's filter must be applied to
    // the phased-out permanents themselves. `matches_target_filter_including_
    // phased_out` evaluates the filter (controller scope, type, etc.) while
    // bypassing the choke point's phased-out exclusion, so a card such as "phase
    // in each phased-out permanent you control" no longer indiscriminately
    // phases in every phased-out permanent (including an opponent's).
    let ctx = FilterContext::from_ability(ability);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let phased_out = state.objects.get(id).is_some_and(|obj| obj.is_phased_out());
            phased_out && matches_target_filter_including_phased_out(state, *id, target, &ctx)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::combat::AttackTarget;
    use crate::game::scenario::{GameRunner, GameScenario, P0, P1};
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, TypedFilter};
    use crate::types::actions::GameAction;
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{
        PendingContinuation, PersistedGameState, StackEntryKind, WaitingFor,
    };
    use crate::types::identifiers::{CardId, LEGACY_INCARNATION};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::resolution::OptionalEffectFrame;
    use crate::types::zones::Zone;

    fn add_creature(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    #[test]
    fn compound_phase_out_collects_source_and_announced_set_without_fallback() {
        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let chosen = add_creature(&mut state, PlayerId(0), "Chosen");
        let unrelated = add_creature(&mut state, PlayerId(0), "Unrelated");
        let filter = TargetFilter::Or {
            filters: vec![TargetFilter::SelfRef, TargetFilter::ParentTarget],
        };
        for (targets, expected) in [
            (vec![], vec![source]),
            (vec![TargetRef::Object(chosen)], vec![source, chosen]),
            (vec![TargetRef::Object(source)], vec![source]),
        ] {
            let ability = ResolvedAbility::new(
                Effect::PhaseOut {
                    target: filter.clone(),
                },
                targets,
                source,
                PlayerId(0),
            );
            let actual = collect_object_targets(&state, &ability, &filter);
            assert_eq!(actual, expected);
            assert!(!actual.contains(&unrelated));
        }
        let ability = ResolvedAbility::new(
            Effect::PhaseOut {
                target: TargetFilter::SelfRef,
            },
            vec![TargetRef::Object(chosen)],
            source,
            PlayerId(0),
        );
        assert_eq!(
            collect_object_targets(&state, &ability, &TargetFilter::SelfRef),
            vec![chosen],
            "standalone explicit-target precedence is unchanged"
        );
    }

    // Synthetic consumer contract: no printed Oracle producer is known for
    // this ordinary Or(SelfRef, TypedCreature) AST. Unlike Dream Fighter's
    // all-context union, it declares a target and must affect only that choice.
    // CR 115.1 + CR 608.2c: SelfRef is an eligible alternative, not extra work.
    #[test]
    fn ordinary_phase_out_union_phases_only_the_announced_object() {
        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "Source");
        let chosen = add_creature(&mut state, PlayerId(0), "Chosen");
        let bystander = add_creature(&mut state, PlayerId(0), "Bystander");
        let effect = Effect::PhaseOut {
            target: TargetFilter::Or {
                filters: vec![
                    TargetFilter::SelfRef,
                    TargetFilter::Typed(TypedFilter::creature()),
                ],
            },
        };
        assert!(crate::types::ability::effect_has_stack_target_declaration(
            &effect
        ));
        let ability =
            ResolvedAbility::new(effect, vec![TargetRef::Object(chosen)], source, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.objects[&chosen].is_phased_out());
        assert!(state.objects[&source].is_phased_in());
        assert!(state.objects[&bystander].is_phased_in());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved { kind: EffectKind::PhaseOut, source_id, .. }
                if *source_id == source
        )));
    }

    // Existing AST consumer contract, not a verified printed slot-union producer.
    // Nearest printed examples are U.S.S. Defiant, Mirror Migrant ("it and up
    // to one target creature you control phase out") and Dream Fighter ("this
    // creature and that creature phase out"). They produce ParentTarget and
    // TriggeringSource respectively, not ParentTargetSlot. This supplemental
    // resolver test checks the existing slot reference without inventing Oracle.
    #[test]
    fn compound_phase_out_parent_slot_affects_only_its_live_declared_object() {
        for (selected_index, stale_index, inherited_count, expected_index) in [
            (1, None, 3, Some(1)),
            (1, Some(0), 3, Some(1)),
            (1, Some(1), 3, None),
            (3, None, 3, None),
            (1, None, 0, None),
        ] {
            let mut state = GameState::new_two_player(42);
            let source = add_creature(&mut state, P0, "Source");
            let inherited = [
                add_creature(&mut state, P0, "Earlier slot"),
                add_creature(&mut state, P0, "Selected slot"),
                add_creature(&mut state, P0, "Later slot"),
            ];
            let bystander = add_creature(&mut state, P0, "Bystander");
            let mut ability = ResolvedAbility::new(
                Effect::PhaseOut {
                    target: TargetFilter::Or {
                        filters: vec![
                            TargetFilter::SelfRef,
                            TargetFilter::ParentTargetSlot {
                                index: selected_index,
                            },
                        ],
                    },
                },
                inherited
                    .iter()
                    .take(inherited_count)
                    .copied()
                    .map(TargetRef::Object)
                    .collect(),
                source,
                P0,
            );
            ability.set_target_incarnations_recursive(
                inherited
                    .iter()
                    .map(|id| ObjectIncarnationRef::from_object(&state.objects[id]))
                    .collect(),
            );
            if let Some(index) = stale_index {
                let id = inherited[index];
                crate::game::zones::move_to_zone(&mut state, id, Zone::Exile, &mut Vec::new());
                crate::game::zones::move_to_zone(
                    &mut state,
                    id,
                    Zone::Battlefield,
                    &mut Vec::new(),
                );
            }
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            assert!(
                state.objects[&source].is_phased_out(),
                "the independent source leg must still resolve"
            );
            for (index, id) in inherited.iter().enumerate() {
                assert_eq!(
                    state.objects[id].is_phased_out(),
                    expected_index == Some(index),
                    "selected slot {selected_index}, stale slot {stale_index:?}, inherited count {inherited_count}, observed slot {index}"
                );
                assert_eq!(state.objects[id].zone, Zone::Battlefield);
            }
            assert!(state.objects[&bystander].is_phased_in());
            assert!(matches!(
                events.last(),
                Some(GameEvent::EffectResolved {
                    kind: EffectKind::PhaseOut,
                    source_id,
                    ..
                }) if *source_id == source
            ));
        }
    }

    // Supplemental AST consumer boundary: these context leaves have no verified
    // printed phasing-union producer. Defiant and Dream Fighter above justify
    // only the supported reference legs, not arbitrary is_context_ref unions.
    #[test]
    fn unsupported_phase_out_context_unions_keep_announced_only_behavior() {
        for context_leaf in [
            TargetFilter::None,
            TargetFilter::LastCreated,
            TargetFilter::CostPaidObject,
            TargetFilter::EventTarget,
        ] {
            let mut state = GameState::new_two_player(42);
            let source = add_creature(&mut state, P0, "Source");
            let chosen = add_creature(&mut state, P0, "Announced");
            let created = add_creature(&mut state, P0, "Created");
            state.last_created_token_ids = vec![created];
            let ability = ResolvedAbility::new(
                Effect::PhaseOut {
                    target: TargetFilter::Or {
                        filters: vec![TargetFilter::SelfRef, context_leaf],
                    },
                },
                vec![TargetRef::Object(chosen)],
                source,
                P0,
            );
            let mut events = Vec::new();
            resolve(&mut state, &ability, &mut events).unwrap();
            assert!(state.objects[&chosen].is_phased_out());
            assert!(state.objects[&source].is_phased_in());
            assert!(state.objects[&created].is_phased_in());
            assert!(matches!(
                events.last(),
                Some(GameEvent::EffectResolved {
                    kind: EffectKind::PhaseOut,
                    source_id,
                    ..
                }) if *source_id == source
            ));
        }
    }

    #[test]
    fn compound_phase_out_batches_an_attachment_with_its_host_indirectly() {
        let mut state = GameState::new_two_player(42);
        let host = add_creature(&mut state, PlayerId(0), "Host");
        let attachment = add_creature(&mut state, PlayerId(0), "Animated Equipment");
        // CR 702.26g: only Auras, Equipment and Fortifications are cascaded, so
        // the fixture must be an ELIGIBLE attachment or its `Indirectly`
        // assertion cannot follow from the printing. It stays a creature (the
        // host relation is what is under test): an animated Equipment —
        // Creature + Artifact with the Equipment subtype.
        //
        // Raw collector fixture: ordinary creature attachments are detached by
        // SBAs before a public priority boundary. This isolates batch precedence.
        {
            let obj = state.objects.get_mut(&attachment).unwrap();
            obj.card_types.core_types.push(CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.attached_to = Some(host.into());
        }
        state
            .objects
            .get_mut(&host)
            .unwrap()
            .attachments
            .push(attachment);
        let ability = ResolvedAbility::new(
            Effect::PhaseOut {
                target: TargetFilter::Or {
                    filters: vec![TargetFilter::SelfRef, TargetFilter::ParentTarget],
                },
            },
            vec![TargetRef::Object(attachment)],
            host,
            PlayerId(0),
        );
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert_eq!(
            state.objects[&host].phase_status,
            crate::game::game_object::PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Directly
            }
        );
        assert_eq!(
            state.objects[&attachment].phase_status,
            crate::game::game_object::PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Indirectly
            }
        );
    }

    /// CR 702.26b: A mass phase-in effect specifically mentioning phased-out
    /// permanents must still honor the effect's object filter. Reverting
    /// `collect_phase_in_targets` to return every phased-out battlefield object
    /// phases in the opponent's creature and fails this regression.
    #[test]
    fn phase_in_mass_filter_only_returns_matching_phased_out_objects() {
        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "Phase Source");
        let mine = add_creature(&mut state, PlayerId(0), "Mine");
        let theirs = add_creature(&mut state, PlayerId(1), "Theirs");

        let mut events = Vec::new();
        phase_out_object(&mut state, mine, PhaseOutCause::Directly, &mut events);
        phase_out_object(&mut state, theirs, PhaseOutCause::Directly, &mut events);

        let ability = ResolvedAbility::new(
            Effect::PhaseIn {
                target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            Vec::new(),
            source,
            PlayerId(0),
        );

        resolve_phase_in(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.objects[&mine].is_phased_out(),
            "controller's matching phased-out creature must phase in"
        );
        assert!(
            state.objects[&theirs].is_phased_out(),
            "opponent's phased-out creature must remain phased out"
        );
    }

    /// CR 702.26c + CR 101.2 + CR 611.2b: `resolve_phase_in` is the second
    /// lock-enforcement site (alongside the untap-step TBA). An explicit
    /// `Effect::PhaseIn` against a permanent held by an active `CantPhaseIn`
    /// restriction must be a no-op while the lock holds, then succeed once the
    /// lock lapses — mirroring The Pandorica's own delayed-trigger phase-in,
    /// which only resolves after the source untaps or leaves. Removing the
    /// `object_has_active_cant_phase_in` skip in `resolve_phase_in` fails this.
    #[test]
    fn resolve_phase_in_respects_cant_phase_in_lock() {
        use crate::types::ability::{ContinuousModification, Duration, StaticCondition};
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "The Pandorica");
        let target = add_creature(&mut state, PlayerId(0), "Held");
        state.objects.get_mut(&source).unwrap().tapped = true;

        let mut events = Vec::new();
        phase_out_object(&mut state, target, PhaseOutCause::Directly, &mut events);
        assert!(state.objects[&target].is_phased_out());

        // CR 611.2b: the lock lives only while the source is tapped (SourceIsTapped).
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::ForAsLongAs {
                condition: StaticCondition::SourceIsTapped,
            },
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantPhaseIn,
            }],
            None,
        );

        // The delayed-trigger phase-in carries a concrete resolved target
        // (ParentTarget snapshot); model it with an explicit object target.
        let ability = ResolvedAbility::new(
            Effect::PhaseIn {
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );

        // Lock active: the explicit phase-in must NOT override it (CR 101.2).
        events.clear();
        resolve_phase_in(&mut state, &ability, &mut events).unwrap();
        assert!(
            state.objects[&target].is_phased_out(),
            "explicit phase-in must not override an active CantPhaseIn lock"
        );

        // Lock lapses (source untaps): the same explicit phase-in now succeeds.
        state.objects.get_mut(&source).unwrap().tapped = false;
        events.clear();
        resolve_phase_in(&mut state, &ability, &mut events).unwrap();
        assert!(
            state.objects[&target].is_phased_in(),
            "explicit phase-in must succeed once the lock lapses"
        );
    }

    /// CR 603.7b + CR 502.3 + CR 702.26c: The Pandorica's full re-entry
    /// lifecycle through the production runtime path — registration, end-of-turn
    /// cleanup, later-turn untap firing, and phase-in resolution.
    ///
    /// This is the regression that the isolated `resolve_phase_in`/parser tests
    /// could not catch: the re-entry delayed trigger is created on the
    /// activation turn but its qualifying event (the source's untap) occurs on a
    /// LATER turn. Modeling it with a `ThisTurn` lifetime — as the original
    /// `WhenNextEvent` did — prunes it at end-of-turn cleanup, so it never fires
    /// and the permanent only re-enters one untap-cycle late via the phasing TBA.
    /// The `Persistent` lifetime must survive cleanup and fire on the untap.
    #[test]
    fn pandorica_reentry_survives_cleanup_and_fires_on_later_untap() {
        use crate::game::effects::delayed_trigger;
        use crate::game::triggers::check_delayed_triggers;
        use crate::game::turns::execute_cleanup;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, DelayedTriggerCondition,
            DelayedTriggerKind, DelayedTriggerLifetime, Duration, StaticCondition,
            TriggerDefinition,
        };
        use crate::types::game_state::DelayedTrigger;
        use crate::types::statics::StaticMode;
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let source = add_creature(&mut state, PlayerId(0), "The Pandorica");
        let target = add_creature(&mut state, PlayerId(0), "Held");
        // The Pandorica taps to activate; the lock holds "for as long as it
        // remains tapped" (CR 611.2b).
        state.objects.get_mut(&source).unwrap().tapped = true;

        let mut events = Vec::new();
        phase_out_object(&mut state, target, PhaseOutCause::Directly, &mut events);
        assert!(state.objects[&target].is_phased_out());

        // CR 611.2b: the can't-phase-in lock, granted by the activation.
        state.add_transient_continuous_effect(
            source,
            PlayerId(0),
            Duration::ForAsLongAs {
                condition: StaticCondition::SourceIsTapped,
            },
            TargetFilter::SpecificObject { id: target },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::CantPhaseIn,
            }],
            None,
        );

        // CR 603.7c: register the open-ended re-entry trigger through the real
        // `CreateDelayedTrigger` resolver, mirroring the parsed shape
        // (Untaps OR LeavesBattlefield, both scoped to the source; inner PhaseIn
        // bound to the parent target).
        let untaps = TriggerDefinition::new(TriggerMode::Untaps).valid_card(TargetFilter::SelfRef);
        let leaves = TriggerDefinition::new(TriggerMode::LeavesBattlefield)
            .valid_card(TargetFilter::SelfRef);
        let create = ResolvedAbility::new(
            Effect::CreateDelayedTrigger {
                condition: DelayedTriggerCondition::WhenNextEvent {
                    trigger: Box::new(untaps),
                    or_trigger: Some(Box::new(leaves)),
                    lifetime: DelayedTriggerLifetime::Persistent,
                },
                effect: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::PhaseIn {
                        target: TargetFilter::ParentTarget,
                    },
                )),
                uses_tracked_set: false,
            },
            vec![TargetRef::Object(target)],
            source,
            PlayerId(0),
        );
        delayed_trigger::resolve(&mut state, &create, &mut events).unwrap();

        // Control: a `ThisTurn` "when you next cast a spell" delayed trigger that
        // SHOULD be pruned at cleanup, proving the survival below is the lifetime
        // gate rather than a blanket "keep everything".
        state.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::WhenNextEvent {
                trigger: Box::new(TriggerDefinition::new(TriggerMode::SpellCast)),
                or_trigger: None,
                lifetime: DelayedTriggerLifetime::ThisTurn,
            },
            ability: Box::new(crate::game::ability_utils::build_resolved_from_def(
                &AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Draw {
                        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ),
                source,
                PlayerId(0),
            )),
            controller: PlayerId(0),
            source_id: source,
            one_shot: true,
            kind: DelayedTriggerKind::Ordinary,
            provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
        });
        assert_eq!(state.delayed_triggers.len(), 2);

        // Snapshot the re-entry ability before cleanup/firing consumes it.
        let reentry = state
            .delayed_triggers
            .iter()
            .find(|dt| {
                matches!(
                    dt.condition,
                    DelayedTriggerCondition::WhenNextEvent {
                        lifetime: DelayedTriggerLifetime::Persistent,
                        ..
                    }
                )
            })
            .expect("re-entry trigger must be registered")
            .ability
            .clone();
        assert!(
            matches!(reentry.effect, Effect::PhaseIn { .. }),
            "re-entry ability must be a PhaseIn"
        );
        assert_eq!(
            reentry.targets,
            vec![TargetRef::Object(target)],
            "CR 603.7c: re-entry must snapshot the phased-out parent target"
        );

        // CR 513.2 + CR 603.7b: end-of-turn cleanup. The `ThisTurn` control
        // trigger is pruned; the `Persistent` re-entry trigger survives.
        let mut cleanup_events = Vec::new();
        execute_cleanup(&mut state, &mut cleanup_events);
        assert_eq!(
            state.delayed_triggers.len(),
            1,
            "cleanup must prune the ThisTurn trigger but keep the Persistent re-entry"
        );
        assert!(
            state.delayed_triggers.iter().any(|dt| matches!(
                dt.condition,
                DelayedTriggerCondition::WhenNextEvent {
                    lifetime: DelayedTriggerLifetime::Persistent,
                    ..
                }
            )),
            "the open-ended re-entry trigger must survive end-of-turn cleanup"
        );

        // CR 502.3: on a later turn's untap step the source untaps (the lock
        // lapses) and emits `PermanentUntapped`, which fires the re-entry trigger.
        state.objects.get_mut(&source).unwrap().tapped = false;
        let fired = check_delayed_triggers(
            &mut state,
            &[GameEvent::PermanentUntapped { object_id: source }],
        );
        assert!(
            !fired.is_empty(),
            "the source's untap must fire the persistent re-entry trigger"
        );
        assert!(
            state.delayed_triggers.is_empty(),
            "the one-shot re-entry trigger must be consumed once it fires"
        );

        // CR 702.26c: resolving the fired re-entry (stack resolution) phases the
        // permanent back in now that the lock has lapsed.
        let mut phase_in_events = Vec::new();
        resolve_phase_in(&mut state, &reentry, &mut phase_in_events).unwrap();
        assert!(
            state.objects[&target].is_phased_in(),
            "the held permanent must re-enter on the untap that fires the trigger"
        );
    }

    fn queued_combat_phase_state() -> (GameState, ObjectId, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let fighter = scenario
            .add_creature(P1, "Dream Fighter", 1, 1)
            .from_oracle_text("Whenever this creature blocks or becomes blocked by a creature, this creature and that creature phase out. (While they're phased out, they're treated as though they don't exist. Each one phases in before its controller untaps during their next untap step.)")
            .id();
        let partner = scenario.add_creature(P0, "Combat partner", 2, 2).id();
        let mut runner = scenario.build();
        runner.advance_to_combat();
        runner
            .declare_attackers(&[(partner, AttackTarget::Player(P1))])
            .unwrap();
        for _ in 0..32 {
            if matches!(
                runner.state().waiting_for,
                WaitingFor::DeclareBlockers { .. }
            ) {
                break;
            }
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            runner.act(GameAction::PassPriority).unwrap();
        }
        runner.declare_blockers(&[(fighter, partner)]).unwrap();
        (runner.state().clone(), fighter, partner)
    }

    fn restore_phase_state(state: &GameState, trusted: bool) -> Result<GameState, String> {
        let saved = if trusted {
            PersistedGameState::capture(state.clone())
        } else {
            PersistedGameState::Raw(Box::new(state.clone()))
        };
        let bytes = serde_json::to_vec(&saved).map_err(|error| error.to_string())?;
        serde_json::from_slice::<PersistedGameState>(&bytes)
            .map_err(|error| error.to_string())?
            .into_game_state()
            .map_err(|error| error.to_string())
    }

    #[test]
    fn parked_phase_context_cannot_borrow_the_resolving_entrys_pins() {
        let (mut state, fighter, partner) = queued_combat_phase_state();
        let entry = state.stack.pop_back().unwrap();
        let chain = Box::new(entry.ability().unwrap().clone());
        let event = match &entry.kind {
            StackEntryKind::TriggeredAbility { trigger_event, .. } => trigger_event.clone(),
            _ => panic!("actual combat must produce a triggered ability"),
        };
        // Infrastructure: park the actual instruction in the existing typed
        // continuation carrier; the resolving entry retains its independent copy.
        state.resolving_trigger_firing = state.stack_trigger_firings.remove(&entry.id);
        state.current_trigger_events = state
            .stack_trigger_event_batches
            .remove(&entry.id)
            .unwrap_or_default();
        state.current_trigger_event = event.clone();
        state.resolving_stack_entry = Some(entry);
        let pending = PendingContinuation::new(chain, &state);
        state.park_ability_continuation(pending);
        state.current_trigger_event = None;
        state.current_trigger_events.clear();
        let mut unbound = state.clone();
        unbound
            .active_ability_continuation_frame_mut()
            .unwrap()
            .pending
            .chain
            .set_target_incarnations_recursive(Vec::new());
        // Infrastructure: a typed optional decision keeps the continuation
        // legitimately suspended; an exposed ready chain at Priority is not a save.
        for parked in [&mut state, &mut unbound] {
            parked.push_optional_effect_frame(OptionalEffectFrame {
                ability: Box::new(ResolvedAbility::new(Effect::NoOp, Vec::new(), fighter, P1)),
                trigger_event: event.clone(),
                trigger_events: Vec::new(),
                trigger_match_count: None,
                return_result_occurrence: None,
            });
            parked.waiting_for = WaitingFor::OptionalEffectChoice {
                player: P1,
                decision_subject_id: None,
                source_id: fighter,
                description: None,
                may_trigger_key: None,
                same_card_may_trigger_choice_available: false,
            };
        }
        for trusted in [false, true] {
            let restored = restore_phase_state(&state, trusted)
                .expect("the parked chain owns its captured trigger context");
            let mut runner = GameRunner::from_state(restored);
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .unwrap();
            assert!(runner.state().objects[&fighter].is_phased_out());
            assert!(runner.state().objects[&partner].is_phased_out());
            assert!(runner.state().stack.is_empty());
            assert!(runner.state().resolution_stack.is_empty());
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            assert!(
                restore_phase_state(&unbound, trusted).is_err(),
                "the entry's pin cannot repair a different resumed ability copy"
            );
        }
    }

    #[test]
    fn stack_push_repairs_legacy_keys_without_rebinding_known_event_objects() {
        for known_departed_pin in [false, true] {
            let (mut state, fighter, partner) = queued_combat_phase_state();
            let entry = state.stack.back().unwrap();
            let mut ability = entry.ability().unwrap().clone();
            let event = match &entry.kind {
                StackEntryKind::TriggeredAbility { trigger_event, .. } => {
                    trigger_event.clone().unwrap()
                }
                _ => panic!("actual combat must produce a triggered ability"),
            };
            let original = ObjectIncarnationRef::from_object(&state.objects[&partner]);
            let mut pins = vec![ObjectIncarnationRef::of(partner, LEGACY_INCARNATION)];
            if known_departed_pin {
                crate::game::zones::move_to_zone(&mut state, partner, Zone::Exile, &mut Vec::new());
                crate::game::zones::move_to_zone(
                    &mut state,
                    partner,
                    Zone::Battlefield,
                    &mut Vec::new(),
                );
                pins.push(original);
            }
            ability.set_target_incarnations_recursive(pins);
            seed_event_context_pins(&state, &mut ability, &event);
            state.current_trigger_event = Some(event);
            resolve(&mut state, &ability, &mut Vec::new()).unwrap();
            assert!(state.objects[&fighter].is_phased_out());
            assert_eq!(state.objects[&partner].is_phased_out(), !known_departed_pin);
        }
    }
}
