use std::collections::HashSet;

use crate::game::game_object::ObjectIdentityBindingExt;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zone_pipeline::{
    self, ApprovedZoneChange, DeliveryCtx, ExileLinkSpec, ZoneDeliveryResult,
};
use crate::types::ability::{
    Effect, EffectError, EffectKind, FilterProp, ResolvedAbility, TargetFilter, TargetRef,
    TypedFilter,
};
use crate::types::events::GameEvent;
pub(crate) use crate::types::game_state::SimultaneousDeparture;
use crate::types::game_state::{
    GameState, PendingSimultaneousDeparture, QueuedSimultaneousDeparture, ResidentDrainPolicy,
    SimultaneousDepartureConsultStep as ConsultStep, SimultaneousDepartureMember,
    SimultaneousDepartureOwner, SimultaneousDepartureStage, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIdentityBinding};
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 701.8a + CR 614: Apply an accepted Destroy proposed event.
///
/// Routes the inner `ZoneChange(Battlefield → Graveyard)` through the
/// replacement pipeline (CR 614.6) so regeneration (CR 701.8c), redirects
/// (e.g., Rest in Peace → exile), and leaves-the-battlefield replacements
/// all compose on the destruction event.
///
/// Shared by the fresh-destroy path (`resolve`/`resolve_all`) and the
/// post-replacement-choice delivery path (`handle_replacement_choice`).
///
/// Returns `true` on success, `false` if the inner ZoneChange itself
/// needed a replacement choice (caller must not advance).
pub fn apply_destroy_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    match event {
        ProposedEvent::Destroy {
            object_id, source, ..
        } => {
            // CR 701.8a: Destruction resolved — now propose the inner ZoneChange
            // so Moved replacements can intercept the actual zone transfer.
            let zone_proposed =
                ProposedEvent::zone_change(object_id, Zone::Battlefield, Zone::Graveyard, source);
            match replacement::replace_event(state, zone_proposed, events) {
                ReplacementResult::Execute(zone_event) => {
                    // CR 701.8a + CR 614: the inner ZoneChange already cleared the
                    // replacement consult — seal it as a proof token and deliver
                    // through the single pipeline tail so a destruction redirected
                    // to the battlefield (e.g. "would die → return tapped/with
                    // counters") gets the same enter_tapped / enter_with_counters /
                    // ETB-counter-static treatment as any other entry.
                    if !deliver_destruction_zone_change(state, zone_event, source, events) {
                        return false;
                    }
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                    return false;
                }
            }
            events.push(GameEvent::CreatureDestroyed {
                object_id,
                source_id: source,
            });
            true
        }
        ProposedEvent::ZoneChange { .. } => {
            // CR 614.6: the outer Destroy replacement redirected directly to a
            // zone change. Deliver through the pipeline tail so a redirect to the
            // battlefield gets the full delivery treatment, not a bare move.
            deliver_destruction_zone_change(state, event, None, events)
        }
        _ => true,
    }
}

/// CR 701.8a + CR 614.6: Deliver a post-replacement destruction `ZoneChange`
/// through the single zone-pipeline tail (`zone_pipeline::deliver`).
///
/// The event has already cleared the replacement consult, so it is sealed via
/// the `approve_post_replacement` proof token (which preserves its `applied`
/// set and re-validates that it is a ZoneChange). Routing through `deliver`
/// gives a destruction redirected to the battlefield the full enter-tapped /
/// enter-with-counters / ETB-counter-static delivery tail instead of a bare
/// `move_to_zone`. Returns `true` on completion, `false` if the delivery tail
/// itself surfaced a replacement choice (`state.waiting_for` is already set).
fn deliver_destruction_zone_change(
    state: &mut GameState,
    zone_event: ProposedEvent,
    source: Option<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> bool {
    let Ok(approved) = ApprovedZoneChange::approve_post_replacement(zone_event) else {
        // Defensive: the inner proposal is always a ZoneChange.
        return true;
    };
    let ctx = DeliveryCtx {
        source_id: source,
        exile_links: ExileLinkSpec::default(),
        drain: crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
        // Destroy delivers to the graveyard — never a library placement.
        library_placement: None,
    };
    !matches!(
        zone_pipeline::deliver(state, approved, ctx, events),
        ZoneDeliveryResult::NeedsChoice(_)
    )
}

/// CR 614.1a + CR 614.6: a destruction replaced by "instead <effect>" (Dr.
/// McCoy's "remove all +1/+1 counters from it and all damage already dealt to
/// it is healed", Crackling Emergence's "sacrifice this Aura and that land gains
/// indestructible") never happens; the replacing instructions are carried out
/// in its place. The replacement pipeline stashes those instructions as a
/// post-replacement continuation, and a stash that arrives while an earlier
/// one is still waiting is discarded, so every caller that destroys more than
/// one object (a destroy-all spell, several targets, a state-based-action
/// batch) must run each prevented destruction's continuation before consulting
/// the next object. Returns the prompt when the continuation needs a player's
/// choice; the caller surfaces it and stops.
pub(crate) fn run_prevented_destruction_continuation(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    if !state.has_post_replacement_drain() {
        return None;
    }
    crate::game::engine_replacement::apply_pending_post_replacement_effect(
        state, None, None, None, events,
    )
}

/// Outcome of destroying a single object through the guarded path.
///
/// Lets callers (the top-level `Effect::Destroy` loop and the counter-source
/// rider) map a single-object destruction onto their own control flow:
/// `Completed`/`Skipped` continue, `NeedsChoice` requires returning without
/// advancing because the replacement pipeline set `state.waiting_for`.
pub(crate) enum DestroyOutcome {
    /// The object was destroyed (or its destruction was replaced/prevented
    /// inline, e.g. regeneration) — caller may continue.
    Completed,
    /// A guard fired (emblem CR 114.5, not on battlefield, or indestructible
    /// CR 702.12b) so nothing was destroyed — caller may continue.
    Skipped,
    /// A replacement requires a player choice mid-resolution; `state.waiting_for`
    /// is already set. Caller must return without advancing.
    NeedsChoice,
}

/// CR 114.5 / CR 701.8a / CR 702.12b: Destroy a single object through the
/// emblem, zone, and indestructible guards followed by the replacement-aware
/// destruction pipeline.
///
/// Factored out of `resolve`'s per-target loop body so that any caller wanting
/// to destroy one determined object (the top-level Destroy effect, the
/// counter-source rider in `counter.rs`) shares exactly one guarded path — the
/// guards (CR 114.5 emblem, battlefield-zone, CR 702.12b indestructible) live
/// here, *before* `ProposedEvent::Destroy`, so they cannot be bypassed.
pub(crate) fn destroy_single_object(
    state: &mut GameState,
    object_id: ObjectId,
    source: ObjectId,
    cant_regenerate: bool,
    events: &mut Vec<GameEvent>,
) -> DestroyOutcome {
    let Some(obj) = state.objects.get(&object_id) else {
        return DestroyOutcome::Skipped;
    };

    // CR 114.5: Emblems are neither cards nor permanents — cannot be destroyed.
    if obj.is_emblem {
        return DestroyOutcome::Skipped;
    }

    // CR 701.8a: Destroy moves a permanent from the battlefield to its owner's
    // graveyard — only battlefield objects are destroyable.
    if obj.zone != Zone::Battlefield {
        return DestroyOutcome::Skipped;
    }

    // CR 702.12b: A permanent with indestructible can't be destroyed.
    if obj.has_keyword(&crate::types::keywords::Keyword::Indestructible) {
        return DestroyOutcome::Skipped;
    }

    let proposed = ProposedEvent::Destroy {
        object_id,
        source: Some(source),
        cant_regenerate,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if apply_destroy_after_replacement(state, event, events) {
                DestroyOutcome::Completed
            } else {
                DestroyOutcome::NeedsChoice
            }
        }
        ReplacementResult::Prevented => {
            match run_prevented_destruction_continuation(state, events) {
                Some(waiting_for) => {
                    state.waiting_for = waiting_for;
                    DestroyOutcome::NeedsChoice
                }
                None => DestroyOutcome::Completed,
            }
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            DestroyOutcome::NeedsChoice
        }
    }
}

/// CR 701.8a: Destroy moves permanent from battlefield to owner's graveyard.
/// CR 701.8b: Indestructible permanents can't be destroyed.
/// Skips objects with the "indestructible" keyword.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let cant_regenerate = matches!(
        &ability.effect,
        Effect::Destroy {
            cant_regenerate: true,
            ..
        }
    );
    for target in destroyed_targets(state, ability) {
        if let TargetRef::Object(obj_id) = target {
            match destroy_single_object(state, obj_id, ability.source_id, cant_regenerate, events) {
                DestroyOutcome::Completed | DestroyOutcome::Skipped => {}
                DestroyOutcome::NeedsChoice => return Ok(()),
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 701.8a: the objects a single-target `Destroy` node destroys. Shared by
/// `resolve` and `stack_reach`, so a pending node is read with the resolver's
/// own binding.
pub(super) fn destroyed_targets(state: &GameState, ability: &ResolvedAbility) -> Vec<TargetRef> {
    let target_filter = match &ability.effect {
        Effect::Destroy { target, .. } => Some(target),
        _ => None,
    };
    if matches!(target_filter, Some(TargetFilter::SelfRef)) && ability.targets.is_empty() {
        return ability
            .self_ref_binding(state)
            .map(TargetRef::Object)
            .into_iter()
            .collect();
    }
    // CR 400.7 + CR 603.7c: a delayed destroy's pinned referent that became a
    // new object is dropped. The SelfRef fallback above still reads the RAW
    // `ability.targets`, so dropping every element here cannot re-bind the
    // destroy to the source.
    //
    // CR 603.2 + CR 608.2k: an untargeted object anaphor on a triggered ability
    // ("whenever ... is dealt damage, destroy IT") names an object carried by the
    // trigger EVENT, not a target the controller chose, so `ability.targets` is
    // empty and reading it alone destroys nothing. Resolve those referents
    // through the shared `resolved_targets` authority, which handles every pure
    // event-context filter uniformly via `is_pure_event_context_filter`.
    //
    // Deliberately the SHARED predicate rather than a handler-local
    // `matches!(.., TriggeringSource | ..)` list: three such lists already exist
    // (`effect.rs` covers only `TriggeringSource`; `change_zone.rs` and
    // `attach.rs` cover `TriggeringSource | AttachedTo`) and they have diverged,
    // because an omitted member is not a compile error — it degrades silently
    // into "this effect does nothing". Routing through the single authority means
    // a future event-context variant is picked up here for free.
    //
    // Gated on `ability.targets.is_empty()` so a genuinely targeted destroy still
    // affects exactly the chosen targets (CR 608.2b).
    match target_filter {
        Some(filter)
            if ability.targets.is_empty()
                && crate::game::targeting::is_pure_event_context_filter(filter) =>
        {
            crate::game::targeting::resolved_targets(ability, filter, state)
        }
        _ => ability.live_object_targets(state),
    }
}

/// Does this destroy filter carry the "other" qualifier? A spell that creates
/// tokens and then destroys "all other creatures" must spare its own freshly-
/// created tokens. This behavior is the official Martial Coup / White Sun's
/// Twilight Gatherer ruling ("creatures that are not [tokens] created by this
/// copy"); the Comprehensive Rules have no numbered entry for the "other"
/// qualifier itself. The parsed marker for "other" is `FilterProp::Another` on a
/// `Typed` filter. Recurse through the boolean combinators so a compound filter
/// ("all other creatures and all other artifacts" → `And`/`Or`) still trips the
/// exclusion when any branch is "other".
fn filter_excludes_other(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::Another)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_excludes_other)
        }
        TargetFilter::Not { filter } => filter_excludes_other(filter),
        _ => false,
    }
}

/// Destroy all permanents matching the filter.
/// CR 701.8: Routes each destruction through the replacement pipeline
/// so regeneration shields and other replacements can intercept.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (target_filter, cant_regenerate) = match &ability.effect {
        Effect::DestroyAll {
            target,
            cant_regenerate,
        } => (target.clone(), *cant_regenerate),
        _ => (crate::types::ability::TargetFilter::Any, false),
    };

    // Use a creature filter as default if the effect's target is None
    let effective_filter = if matches!(target_filter, crate::types::ability::TargetFilter::None) {
        crate::types::ability::TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            controller: None,
            properties: vec![],
        })
    } else {
        crate::game::effects::resolved_object_filter(state, ability, &target_filter)
    };

    // CR 701.7 (create) + CR 701.8 (destroy), per the Martial Coup / White Sun's
    // Twilight ruling: "destroy all OTHER creatures" on a token-creating spell does
    // NOT destroy the tokens that same spell just created — they are not "other"
    // creatures. `FilterProp::Another` is the parsed "other" marker.
    //
    // The ruling spares only tokens "created by this copy". Each `TokenCreated`
    // event carries the `source_id` of the effect that made the token, so the spared
    // set is exactly the tokens whose creator is THIS resolving ability's source —
    // the same `source_id` shared by the spell's own create and destroy steps. A
    // token an opponent's (or anyone's) replacement effect produces earlier in the
    // same resolution (e.g. Kalitas, Traitor of Ghet turning a creature's death into
    // a Zombie during an earlier "each player sacrifices" instruction) carries that
    // replacement source's id, not this spell's, so it remains an "other" creature
    // and is destroyed. Scanning `events` is resolution-scoped: `resolve_next`
    // resolves exactly one stack object per `apply` (priority resets afterward), so
    // no stale cross-resolution token leaks in.
    //
    // When the filter has no `Another` marker (plain "destroy all creatures") or this
    // spell created no tokens this resolution, the exclusion set is empty and behavior
    // is unchanged.
    let spare_self_created: HashSet<ObjectId> = if filter_excludes_other(&effective_filter) {
        events
            .iter()
            .filter_map(|e| match e {
                GameEvent::TokenCreated {
                    object_id,
                    source_id,
                    ..
                } if *source_id == ability.source_id => Some(*object_id),
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };

    // Collect matching object IDs that are on the battlefield and not indestructible.
    // CR 107.3a + CR 601.2b: ability-context filter evaluation.
    let ctx = crate::game::filter::FilterContext::from_ability(ability);
    let matching: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            let is_indestructible = state
                .objects
                .get(id)
                .map(|obj| obj.has_keyword(&crate::types::keywords::Keyword::Indestructible))
                .unwrap_or(false);
            !is_indestructible
                && !spare_self_created.contains(id)
                && crate::game::filter::matches_target_filter(state, **id, &effective_filter, &ctx)
        })
        .copied()
        .collect();

    // CR 701.8a + CR 608.2c: every matching permanent is destroyed by one
    // simultaneous event. Each member's destruction is consulted while the
    // whole set is present and only then delivered, and a choice on the way
    // parks the rest of the batch instead of abandoning it. The owner's
    // completion stamps the co-departures and publishes `EffectResolved`.
    let members: Vec<_> = matching
        .iter()
        .map(|&object_id| SimultaneousDepartureMember {
            object_id,
            departure: SimultaneousDeparture::Destroy,
        })
        .collect();
    let _ = depart_simultaneously(
        state,
        &members,
        SimultaneousDepartureOwner::DestroyAll {
            source_id: ability.source_id,
            kind: EffectKind::from(&ability.effect),
        },
        Some(ability.source_id),
        cant_regenerate,
        events,
    );

    Ok(())
}

/// How [`depart_simultaneously`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SimultaneousDepartureOutcome {
    /// Every member was consulted and delivered, and the owner's completion
    /// ran.
    Completed,
    /// A consult or a delivery surfaced a choice; `state.waiting_for` is set
    /// and the rest of the batch is parked in a `SimultaneousDeparture` frame,
    /// resumed once that choice's own work has settled.
    Paused,
}

/// Whether consulting one member kept going or parked the batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConsultFlow {
    Continue,
    Paused,
}

/// CR 614.1a + CR 614.6: does a member's departure have a replacement effect
/// whose source is another member? Only then must every death of a
/// state-based-action batch be consulted while all members are still present;
/// otherwise each member's replacements are consulted while their sources are
/// still on the battlefield whichever order the deaths are performed in.
pub(crate) fn co_departing_replacement_applies(
    state: &GameState,
    members: &[SimultaneousDepartureMember],
    source: Option<ObjectId>,
    cant_regenerate: bool,
) -> bool {
    let member_ids: HashSet<ObjectId> = members.iter().map(|member| member.object_id).collect();
    let registry = replacement::replacement_registry();
    members.iter().any(|member| {
        let object_id = member.object_id;
        let graveyard_move =
            ProposedEvent::zone_change(object_id, Zone::Battlefield, Zone::Graveyard, source);
        let destruction = match member.departure {
            SimultaneousDeparture::Destroy => Some(ProposedEvent::Destroy {
                object_id,
                source,
                cant_regenerate,
                applied: HashSet::new(),
            }),
            SimultaneousDeparture::PutIntoGraveyard => None,
        };
        destruction
            .iter()
            .chain(std::iter::once(&graveyard_move))
            .any(|event| {
                replacement::find_applicable_replacements(state, event, registry)
                    .iter()
                    .any(|rid| rid.source != object_id && member_ids.contains(&rid.source))
            })
    })
}

/// CR 614.1a + CR 614.6 + CR 616.1 + CR 701.8a: several permanents leave the
/// battlefield at once (a "destroy all" instruction; CR 704.3 state-based
/// actions), so a replacement effect whose source is itself one of them (Dr.
/// McCoy protecting the other creatures Wrath of God destroys; Kalitas,
/// Traitor of Ghet exiling the opposing creatures that die with it) still
/// applies to the others. Consult every member's destruction and graveyard
/// move while the whole set is present, then deliver the moves. The single
/// authority for both the destroy-all and the state-based-action batch.
///
/// Each member's stashed replacing instructions are detached and held with its
/// queued delivery, then reinstalled just before that delivery, whose tail runs
/// them. A queued member that has meanwhile left the battlefield is skipped
/// with its instructions (CR 400.7).
///
/// A choice is never a reason to abandon the batch. A consult that needs a
/// CR 616.1 choice parks the batch with that member in flight; the answer is
/// recorded into the queue rather than delivered on its own, so the members
/// consulted after it still see every co-departing replacement source
/// ([`resume_parked_departure_consult`]). A delivery whose move or replacing
/// instructions need a choice (Kalitas's Zombie meeting Jinnie Fay's "you may
/// instead create") parks the batch beneath that choice's own work; once it has
/// settled, the batch delivers the rest ([`drain_pending_simultaneous_departure`]).
pub(crate) fn depart_simultaneously(
    state: &mut GameState,
    members: &[SimultaneousDepartureMember],
    owner: SimultaneousDepartureOwner,
    source: Option<ObjectId>,
    cant_regenerate: bool,
    events: &mut Vec<GameEvent>,
) -> SimultaneousDepartureOutcome {
    let pending = PendingSimultaneousDeparture {
        owner,
        source,
        cant_regenerate,
        announced: members.iter().map(|member| member.object_id).collect(),
        unconsulted: members.to_vec(),
        queued: Vec::new(),
        stage: None,
        delivered: Vec::new(),
        outer_rider_pending: state.has_post_replacement_drain(),
        deferred_events: Vec::new(),
    };
    let batch_start = events.len();
    advance_simultaneous_departure(state, pending, batch_start, events)
}

/// CR 614.6 + CR 616.1 + CR 608.2c: resume a parked simultaneous departure
/// once the child that paused it (a delivery's prompt, a replacing
/// instruction's prompt) has settled. The member whose delivery was in flight
/// is recorded, the events held back while the batch was paused are restored
/// ahead of this action's, and the remaining members are consulted and
/// delivered.
pub(crate) fn drain_pending_simultaneous_departure(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return;
    }
    let Some(mut pending) = state
        .take_active_simultaneous_departure()
        .expect("simultaneous-departure resumer may consume only its active frame")
    else {
        return;
    };
    match pending.stage.take() {
        None => {}
        // CR 616.1: the consult's answer was carried out by the answer's own
        // owner rather than recorded here (the batch was not the active frame
        // when the answer arrived), so that member's departure already
        // happened; the rest of the batch continues.
        Some(SimultaneousDepartureStage::Consulting { .. }) => {}
        Some(SimultaneousDepartureStage::Delivering {
            object_id,
            source,
            destroyed,
            moved,
        }) => {
            // CR 614.5 + CR 614.6: a token-choice substitute accepted during
            // that member's delivery (Jinnie Fay's Cats) seeded its applied set
            // so it cannot re-apply to its own tokens. Its work has settled;
            // the next member's replacing instructions create a new event that
            // the same replacement may modify again.
            state.post_replacement_token_choice_applied = None;
            state.post_replacement_token_substitution_count = None;
            record_delivery(&mut pending, object_id, source, destroyed, moved, events);
        }
    }
    // CR 603.10a: one departure is observable as one batch. The events held
    // back while it was paused precede everything this resuming action
    // produced.
    let mut restored = std::mem::take(&mut pending.deferred_events);
    restored.append(events);
    *events = restored;
    let _ = advance_simultaneous_departure(state, pending, 0, events);
}

/// A replacement choice is about to be answered while a simultaneous
/// departure's consult is parked on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ParkedDepartureConsult {
    outer_rider_pending: bool,
}

/// CR 616.1: is the replacement choice about to be answered the parked consult
/// of a simultaneous departure? Read before the answer is applied: applying it
/// can stash the chosen replacement's instructions above the batch's frame.
pub(crate) fn parked_departure_consult(
    state: &GameState,
    answered_object: Option<ObjectId>,
) -> Option<ParkedDepartureConsult> {
    let pending = state.active_simultaneous_departure()?;
    match &pending.stage {
        Some(SimultaneousDepartureStage::Consulting { member, .. })
            if Some(member.object_id) == answered_object =>
        {
            Some(ParkedDepartureConsult {
                outer_rider_pending: pending.outer_rider_pending,
            })
        }
        _ => None,
    }
}

/// CR 616.1 + CR 614.6 + CR 701.8a: record the answered consult of a parked
/// simultaneous departure into its queue instead of carrying the departure out
/// on its own, then leave the batch parked for the shared resumption boundary
/// (or parked again on the member's next choice). Delivering the answered
/// member here would take it off the battlefield while later members are
/// still being consulted, stripping them of a replacement it is the source of
/// (Kalitas exiling the other creatures Wrath of God destroys).
///
/// Returns the answer back when the batch's frame is no longer reachable, so
/// the caller carries it out through its ordinary path.
pub(crate) fn resume_parked_departure_consult(
    state: &mut GameState,
    parked: ParkedDepartureConsult,
    result: ReplacementResult,
    events: &mut Vec<GameEvent>,
) -> Result<(), ReplacementResult> {
    // Applying the answer may have stashed the chosen replacement's
    // instructions in a drain frame above the batch. They belong to this
    // member; lift them off so the batch is reachable, then reinstall them for
    // the consult to queue with the member.
    let rider = if parked.outer_rider_pending {
        None
    } else {
        state.detach_ready_post_replacement_drain()
    };
    let taken = state.take_active_simultaneous_departure();
    if let Some(rider) = rider {
        state.install_post_replacement_drain(rider, ResidentDrainPolicy::KeepResident);
    }
    let Ok(Some(mut pending)) = taken else {
        return Err(result);
    };
    let Some(SimultaneousDepartureStage::Consulting {
        member,
        binding,
        step,
    }) = pending.stage.take()
    else {
        unreachable!("a parked departure consult owns a Consulting stage");
    };
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    let consult_start = events.len();
    let child_stack_start = state.resolution_stack.capture_child_boundary();
    match settle_consult(state, &mut pending, member, binding, step, result, events) {
        ConsultFlow::Paused => {
            park_simultaneous_departure(state, pending, child_stack_start, consult_start, events);
        }
        ConsultFlow::Continue => {
            // The answered member is queued; the shared resumption boundary
            // consults the rest and delivers the batch.
            pending
                .deferred_events
                .extend(events.drain(consult_start..));
            state.resolution_stack.push_simultaneous_departure(pending);
        }
    }
    Ok(())
}

/// Consult the remaining members, then deliver every queued one, parking the
/// batch on the first choice. Runs the owner's completion once nothing is
/// left. `stamp_from` is where this batch's events begin in `events`.
fn advance_simultaneous_departure(
    state: &mut GameState,
    mut pending: PendingSimultaneousDeparture,
    stamp_from: usize,
    events: &mut Vec<GameEvent>,
) -> SimultaneousDepartureOutcome {
    while !pending.unconsulted.is_empty() {
        let member = pending.unconsulted.remove(0);
        // CR 400.7: an earlier member's replacing instructions may have moved
        // this one already; it is no longer on the battlefield to depart.
        let Some(binding) = state
            .objects
            .get(&member.object_id)
            .filter(|obj| obj.zone == Zone::Battlefield && obj.is_phased_in())
            .map(ObjectIdentityBinding::from_object)
        else {
            continue;
        };
        let child_stack_start = state.resolution_stack.capture_child_boundary();
        let (step, proposed) = match member.departure {
            SimultaneousDeparture::Destroy => (
                ConsultStep::Destruction,
                ProposedEvent::Destroy {
                    object_id: member.object_id,
                    source: pending.source,
                    cant_regenerate: pending.cant_regenerate,
                    applied: HashSet::new(),
                },
            ),
            // CR 704.5f: a put-into-graveyard death has no destroying source.
            SimultaneousDeparture::PutIntoGraveyard => (
                ConsultStep::GraveyardMove {
                    source: None,
                    destroyed: false,
                },
                ProposedEvent::zone_change(
                    member.object_id,
                    Zone::Battlefield,
                    Zone::Graveyard,
                    None,
                ),
            ),
        };
        let result = replacement::replace_event(state, proposed, events);
        if settle_consult(state, &mut pending, member, binding, step, result, events)
            == ConsultFlow::Paused
        {
            park_simultaneous_departure(state, pending, child_stack_start, stamp_from, events);
            return SimultaneousDepartureOutcome::Paused;
        }
    }

    while !pending.queued.is_empty() {
        let queued = pending.queued.remove(0);
        // CR 400.7: the consulted object already left (sacrificed by another
        // member's replacing instructions); neither its move nor its replacing
        // instructions happen.
        if !queued.binding.is_current(state) {
            continue;
        }
        let child_stack_start = state.resolution_stack.capture_child_boundary();
        let has_rider = queued.rider.is_some();
        if let Some(rider) = queued.rider {
            state.install_post_replacement_drain(rider, ResidentDrainPolicy::KeepResident);
        }
        let moved = queued.zone_event.is_some();
        let paused = match queued.zone_event {
            // The delivery tail runs the reinstalled rider (CR 614.6).
            Some(zone_event) => {
                !deliver_destruction_zone_change(state, zone_event, queued.source, events)
            }
            None if has_rider => match run_prevented_destruction_continuation(state, events) {
                Some(waiting_for) => {
                    state.waiting_for = waiting_for;
                    true
                }
                None => false,
            },
            None => false,
        };
        if paused {
            // CR 608.2c: the member's delivery finishes through the choice's
            // own work; the batch records it once that work has settled.
            pending.stage = Some(SimultaneousDepartureStage::Delivering {
                object_id: queued.object_id,
                source: queued.source,
                destroyed: queued.destroyed,
                moved,
            });
            park_simultaneous_departure(state, pending, child_stack_start, stamp_from, events);
            return SimultaneousDepartureOutcome::Paused;
        }
        record_delivery(
            &mut pending,
            queued.object_id,
            queued.source,
            queued.destroyed,
            moved,
            events,
        );
    }

    complete_simultaneous_departure(state, &pending, stamp_from, events);
    SimultaneousDepartureOutcome::Completed
}

/// CR 616.1 + CR 614.6: settle one consult step of `member` from its
/// replacement result, queueing its approved move with its own replacing
/// instructions. A destruction that executes goes on to consult its graveyard
/// move. A choice records the step in flight so its answer resumes it.
fn settle_consult(
    state: &mut GameState,
    pending: &mut PendingSimultaneousDeparture,
    member: SimultaneousDepartureMember,
    binding: ObjectIdentityBinding,
    step: ConsultStep,
    result: ReplacementResult,
    events: &mut Vec<GameEvent>,
) -> ConsultFlow {
    match (step, result) {
        (_, ReplacementResult::NeedsChoice(player)) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            pending.stage = Some(SimultaneousDepartureStage::Consulting {
                member,
                binding,
                step,
            });
            ConsultFlow::Paused
        }
        // CR 701.8a: the destruction happens, so its graveyard move is
        // consulted next.
        (
            ConsultStep::Destruction,
            ReplacementResult::Execute(ProposedEvent::Destroy {
                object_id, source, ..
            }),
        ) => {
            let zone_proposed =
                ProposedEvent::zone_change(object_id, Zone::Battlefield, Zone::Graveyard, source);
            let result = replacement::replace_event(state, zone_proposed, events);
            settle_consult(
                state,
                pending,
                member,
                binding,
                ConsultStep::GraveyardMove {
                    source,
                    destroyed: true,
                },
                result,
                events,
            )
        }
        // CR 614.6: an outer redirect of the destruction itself.
        (
            ConsultStep::Destruction,
            ReplacementResult::Execute(redirect @ ProposedEvent::ZoneChange { .. }),
        ) => {
            queue_departure(state, pending, member, binding, None, Some(redirect), false);
            ConsultFlow::Continue
        }
        (ConsultStep::Destruction, ReplacementResult::Execute(_)) => ConsultFlow::Continue,
        // Regenerated, or replaced by an "instead" effect whose replacing
        // instructions run now, before the next member is consulted. Nothing of
        // this member's departure remains, so a prompt they raise parks the
        // batch with no member in flight.
        (ConsultStep::Destruction, ReplacementResult::Prevented) => {
            match run_prevented_destruction_continuation(state, events) {
                Some(waiting_for) => {
                    state.waiting_for = waiting_for;
                    ConsultFlow::Paused
                }
                None => ConsultFlow::Continue,
            }
        }
        (ConsultStep::GraveyardMove { source, destroyed }, ReplacementResult::Execute(event)) => {
            queue_departure(
                state,
                pending,
                member,
                binding,
                source,
                Some(event),
                destroyed,
            );
            ConsultFlow::Continue
        }
        (ConsultStep::GraveyardMove { source, destroyed }, ReplacementResult::Prevented) => {
            queue_departure(state, pending, member, binding, source, None, destroyed);
            ConsultFlow::Continue
        }
    }
}

/// Queue one consulted member, detaching the replacing instructions its consult
/// stashed so a later member's cannot displace them.
fn queue_departure(
    state: &mut GameState,
    pending: &mut PendingSimultaneousDeparture,
    member: SimultaneousDepartureMember,
    binding: ObjectIdentityBinding,
    source: Option<ObjectId>,
    zone_event: Option<ProposedEvent>,
    destroyed: bool,
) {
    let rider = if pending.outer_rider_pending {
        None
    } else {
        state.detach_ready_post_replacement_drain()
    };
    pending.queued.push(QueuedSimultaneousDeparture {
        object_id: member.object_id,
        binding,
        source,
        zone_event,
        destroyed,
        rider,
    });
}

/// Record one member's finished delivery.
fn record_delivery(
    pending: &mut PendingSimultaneousDeparture,
    object_id: ObjectId,
    source: Option<ObjectId>,
    destroyed: bool,
    moved: bool,
    events: &mut Vec<GameEvent>,
) {
    if moved {
        pending.delivered.push(object_id);
    }
    if destroyed {
        events.push(GameEvent::CreatureDestroyed {
            object_id,
            source_id: source,
        });
    }
}

/// Park the batch on the choice just surfaced, holding back the events it has
/// produced so far (from `batch_start`) until it settles (CR 603.10a).
fn park_simultaneous_departure(
    state: &mut GameState,
    mut pending: PendingSimultaneousDeparture,
    child_stack_start: crate::types::resolution::ChildStackDepth,
    batch_start: usize,
    events: &mut Vec<GameEvent>,
) {
    pending.deferred_events.extend(events.drain(batch_start..));
    state.park_simultaneous_departure(pending, child_stack_start);
}

/// The logical owner's completion, run once after every member has departed.
fn complete_simultaneous_departure(
    state: &mut GameState,
    pending: &PendingSimultaneousDeparture,
    stamp_from: usize,
    events: &mut Vec<GameEvent>,
) {
    // CR 603.10a + CR 704.3: every member left the battlefield simultaneously,
    // so co-departing leaves-the-battlefield/dies observers (Blood Artist,
    // Zulaport Cutthroat) observe each other. CR 701.19a/b: a regenerated
    // member (and any other prevented departure) stays on the battlefield, so
    // `departed_subset` excludes it from every co-departed group.
    let departed = crate::game::zones::departed_subset(state, &pending.announced);
    match pending.owner {
        SimultaneousDepartureOwner::DestroyAll { source_id, kind } => {
            crate::game::zones::mark_simultaneous_departures(&mut events[stamp_from..], &departed);
            events.push(GameEvent::EffectResolved {
                kind,
                source_id,
                subject: None,
            });
        }
        SimultaneousDepartureOwner::StateBasedDeaths => {
            crate::game::sba::settle_redirected_state_based_deaths(state, &pending.delivered);
            crate::game::zones::mark_simultaneous_departures(&mut events[stamp_from..], &departed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::effects::resolve_ability_chain;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCondition, PtValue, QuantityExpr, SubAbilityLink, TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::CounterType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;

    #[test]
    fn destroy_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    /// Spawn a battlefield creature for the "destroy all other creatures" tests.
    fn battlefield_creature(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().card_types.core_types = vec![CoreType::Creature];
        id
    }

    /// A `DestroyAll` over creatures carrying the `FilterProp::Another` ("other")
    /// marker — the parsed shape of "destroy all other creatures".
    fn destroy_all_other_creatures() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![FilterProp::Another],
                }),
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    /// Martial Coup / White Sun's Twilight ruling: "destroy all OTHER creatures"
    /// spares the tokens this spell created earlier in the same resolution. Those
    /// tokens are identified by a `TokenCreated` event whose `source_id` matches the
    /// resolving ability's source (`ObjectId(100)`, per `destroy_all_other_creatures`);
    /// a creature with no such event is destroyed.
    #[test]
    fn destroy_all_other_creatures_spares_same_resolution_tokens() {
        let mut state = GameState::new_two_player(42);
        let token = battlefield_creature(&mut state, "Mite");
        let other = battlefield_creature(&mut state, "Grizzly Bears");

        // The event buffer carries the spell's earlier token creation, exactly as a
        // token-then-wrath chain produces it during a single resolution.
        let mut events = vec![GameEvent::TokenCreated {
            object_id: token,
            name: "Mite".to_string(),
            source_id: ObjectId(100),
        }];

        resolve_all(&mut state, &destroy_all_other_creatures(), &mut events).unwrap();

        assert!(
            state.battlefield.contains(&token),
            "a token created this resolution is not 'other' and must survive"
        );
        assert!(
            !state.battlefield.contains(&other),
            "a creature not created this resolution is destroyed"
        );
    }

    /// The "this copy" clause of the Martial Coup ruling: only the tokens THIS
    /// spell created are spared. A token a replacement effect makes earlier in the
    /// same resolution (e.g. Kalitas turning a death into a Zombie) carries a
    /// `TokenCreated` event too, but with that replacement's `source_id` — not this
    /// spell's — so it is still an "other" creature and is destroyed.
    ///
    /// The foreign token is deliberately given the SAME controller as the resolving
    /// spell (`PlayerId(0)`): identity here is the creating SOURCE, not the
    /// controller, so a caster-controlled replacement token is correctly destroyed —
    /// the case a controller-only check would miss.
    #[test]
    fn destroy_all_other_creatures_does_not_spare_foreign_replacement_token() {
        let mut state = GameState::new_two_player(42);
        let own = battlefield_creature(&mut state, "Mite");
        let foreign = battlefield_creature(&mut state, "Zombie");

        let mut events = vec![
            // `own` was created by the resolving spell (source ObjectId(100)).
            GameEvent::TokenCreated {
                object_id: own,
                name: "Mite".to_string(),
                source_id: ObjectId(100),
            },
            // `foreign` was created this resolution by a DIFFERENT source (a
            // replacement effect, ObjectId(200)) — same controller, different creator.
            GameEvent::TokenCreated {
                object_id: foreign,
                name: "Zombie".to_string(),
                source_id: ObjectId(200),
            },
        ];

        resolve_all(&mut state, &destroy_all_other_creatures(), &mut events).unwrap();

        assert!(
            state.battlefield.contains(&own),
            "the spell's own token is not 'other' and survives"
        );
        assert!(
            !state.battlefield.contains(&foreign),
            "a token created by a different source is not 'this copy' and must be destroyed, \
             even under the spell's own controller"
        );
    }

    /// The "other" marker is recognized through boolean filter combinators, so a
    /// compound "destroy all other creatures and all other artifacts" (`And`/`Or` of
    /// `Another`-bearing `Typed` filters) still triggers the self-token exclusion. A
    /// compound filter with no `Another` does not.
    #[test]
    fn filter_excludes_other_recurses_through_compound_filters() {
        let other = TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::Another],
        });
        assert!(filter_excludes_other(&other));
        assert!(filter_excludes_other(&TargetFilter::Or {
            filters: vec![other.clone()],
        }));
        assert!(filter_excludes_other(&TargetFilter::And {
            filters: vec![other.clone()],
        }));
        assert!(filter_excludes_other(&TargetFilter::Not {
            filter: Box::new(other),
        }));

        let plain = TargetFilter::Typed(TypedFilter {
            type_filters: vec![crate::types::ability::TypeFilter::Creature],
            controller: None,
            properties: vec![],
        });
        assert!(!filter_excludes_other(&TargetFilter::And {
            filters: vec![plain],
        }));
    }

    /// Negative control: with no `TokenCreated` event in the buffer (the destroy
    /// did not create anything this resolution), "destroy all other creatures"
    /// destroys every matching creature — the exclusion set is empty.
    #[test]
    fn destroy_all_other_creatures_without_self_tokens_destroys_all() {
        let mut state = GameState::new_two_player(42);
        let a = battlefield_creature(&mut state, "Bear A");
        let b = battlefield_creature(&mut state, "Bear B");

        resolve_all(&mut state, &destroy_all_other_creatures(), &mut Vec::new()).unwrap();

        assert!(!state.battlefield.contains(&a));
        assert!(!state.battlefield.contains(&b));
    }

    /// Negative control: a plain "destroy all creatures" (no `FilterProp::Another`
    /// marker) destroys even a same-resolution token — the exclusion is gated on
    /// the "other" qualifier, so it must not fire here.
    #[test]
    fn destroy_all_creatures_without_other_marker_destroys_self_token() {
        let mut state = GameState::new_two_player(42);
        let token = battlefield_creature(&mut state, "Mite");

        let mut events = vec![GameEvent::TokenCreated {
            object_id: token,
            name: "Mite".to_string(),
            source_id: ObjectId(100),
        }];

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![crate::types::ability::TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.battlefield.contains(&token),
            "without the 'other' marker, a same-resolution token is destroyed too"
        );
    }

    /// CR 122.1c: a permanent with shield counters is not destroyed by a
    /// destruction effect; one shield counter is removed instead, per use.
    #[test]
    fn shield_counter_prevents_destruction_and_is_consumed_per_use() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shielded Bear".to_string(),
            Zone::Battlefield,
        );
        // CR 122.1c: one or more shield counters share a single replacement.
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .counters
            .insert(CounterType::Shield, 2);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );

        // First destruction: prevented, one shield counter removed (2 -> 1).
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert!(
            state.battlefield.contains(&obj_id),
            "shield counter must prevent destruction"
        );
        assert_eq!(
            state.objects[&obj_id].counters.get(&CounterType::Shield),
            Some(&1)
        );

        // Second destruction: removes the last counter (1 -> 0); still alive.
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert!(state.battlefield.contains(&obj_id));
        assert_eq!(
            state.objects[&obj_id].counters.get(&CounterType::Shield),
            None
        );

        // Third destruction: no shield left -> destroyed.
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();
        assert!(
            !state.battlefield.contains(&obj_id),
            "with no shield counter, the permanent is destroyed"
        );
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    #[test]
    fn destroy_self_ref_moves_source_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Experimental Frenzy".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::SelfRef,
                cant_regenerate: false,
            },
            Vec::new(),
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    #[test]
    fn destroy_skips_indestructible() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "God".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Indestructible);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.battlefield.contains(&obj_id));
    }

    fn make_if_you_do_token_rider(source_id: ObjectId) -> ResolvedAbility {
        let mut rider = ResolvedAbility::new(
            Effect::Token {
                name: "Destroy Rider Token".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string()],
                colors: Vec::new(),
                keywords: Vec::new(),
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                attack_destination:
                    crate::types::ability::TokenAttackDestination::ControllerChooses,
                supertypes: Vec::new(),
                static_abilities: Vec::new(),
                enter_with_counters: Vec::new(),
            },
            Vec::new(),
            source_id,
            PlayerId(0),
        )
        .condition(AbilityCondition::effect_performed());
        rider.sub_link = SubAbilityLink::SequentialSibling;
        rider
    }

    fn destroy_with_if_you_do_rider(target: ObjectId) -> ResolvedAbility {
        let mut ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        );
        ability.sub_ability = Some(Box::new(make_if_you_do_token_rider(ObjectId(100))));
        ability
    }

    /// CR 608.2c + CR 701.8a: a mandatory destroy instruction that actually
    /// moves the target satisfies its following "if you do" rider.
    #[test]
    fn mandatory_destroy_if_you_do_rider_fires_when_destroyed() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Mortal Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = destroy_with_if_you_do_rider(obj_id);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state
            .objects
            .values()
            .any(|obj| obj.is_token && obj.name == "Destroy Rider Token"));
    }

    /// CR 608.2c + CR 702.12b: a skipped destroy instruction did not happen,
    /// so it must not satisfy a following "if you do" rider.
    #[test]
    fn mandatory_destroy_if_you_do_rider_skips_when_indestructible() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Indestructible Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Indestructible);
        let ability = destroy_with_if_you_do_rider(obj_id);
        let mut events = Vec::new();

        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state
            .objects
            .values()
            .any(|obj| obj.is_token && obj.name == "Destroy Rider Token"));
    }

    /// CR 701.8a: the destruction names the object whose instruction destroyed it.
    #[test]
    fn destroy_emits_creature_destroyed_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::CreatureDestroyed { object_id, source_id }
                if *object_id == obj_id && *source_id == Some(ObjectId(100))
        )));
    }

    #[test]
    fn destroy_all_creatures() {
        let mut state = GameState::new_two_player(42);
        let bear1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let bear2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opp Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Non-creature should survive
        let _land = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&bear1));
        assert!(!state.battlefield.contains(&bear2));
        // Land survives
        assert_eq!(state.battlefield.len(), 1);
    }

    /// CR 122.1c: a shield counter replaces destruction from a mass-destruction
    /// effect (board wipe), not just single-target destruction. The shielded
    /// creature survives (one counter removed); an unshielded creature dies.
    #[test]
    fn shield_counter_prevents_destroy_all_and_is_consumed() {
        let mut state = GameState::new_two_player(42);

        let shielded = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shielded Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&shielded).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Shield, 1);
        }

        let plain = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Plain Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&plain)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        resolve_all(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(
            state.battlefield.contains(&shielded),
            "shield counter must save the creature from a board wipe"
        );
        assert_eq!(
            state.objects[&shielded].counters.get(&CounterType::Shield),
            None,
            "the shield counter is consumed"
        );
        assert!(
            !state.battlefield.contains(&plain),
            "unshielded creature is destroyed by the board wipe"
        );
    }

    #[test]
    fn destroy_all_shield_counter_and_umbra_prompt_for_order() {
        let mut state = GameState::new_two_player(42);

        let shielded = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shielded Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&shielded).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.counters.insert(CounterType::Shield, 1);
        }

        let umbra = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Hyena Umbra".to_string(),
            Zone::Battlefield,
        );
        {
            let aura = state.objects.get_mut(&umbra).unwrap();
            aura.card_types.core_types.push(CoreType::Enchantment);
            aura.card_types.subtypes.push("Aura".to_string());
            aura.keywords.push(Keyword::TotemArmor);
            aura.attached_to = Some(shielded.into());
        }
        state
            .objects
            .get_mut(&shielded)
            .unwrap()
            .attachments
            .push(umbra);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        let WaitingFor::ReplacementChoice {
            player,
            candidate_count,
            candidates,
            kind,
            ..
        } = &state.waiting_for
        else {
            panic!(
                "shield counter plus umbra armor under DestroyAll must prompt for CR 616 \
                 order, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*player, PlayerId(0));
        assert_eq!(*candidate_count, 2);
        // CR 616.1e: two distinct competing replacements — an ordering decision,
        // not an optional accept/decline. The frontend keys its presentation off
        // this discriminator, so pin it here.
        assert_eq!(
            *kind,
            crate::types::game_state::ReplacementChoiceKind::Order
        );
        let descriptions: Vec<&str> = candidates.iter().map(|c| c.description.as_str()).collect();
        assert_eq!(
            descriptions.as_slice(),
            &[
                "Remove a shield counter",
                "Umbra armor: destroy Hyena Umbra instead",
            ]
        );
        assert_eq!(
            state.objects[&shielded].counters.get(&CounterType::Shield),
            Some(&1),
            "the shield counter must not be consumed before the replacement choice"
        );
        assert!(
            state.battlefield.contains(&umbra),
            "the Umbra must not be destroyed before the replacement choice"
        );
    }

    #[test]
    fn destroy_all_or_filter_destroys_every_matching_permanent() {
        fn permanent(
            state: &mut GameState,
            card_id: u64,
            owner: PlayerId,
            name: &str,
            core_type: CoreType,
        ) -> ObjectId {
            let id = create_object(
                state,
                CardId(card_id),
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
                .push(core_type);
            id
        }

        let mut state = GameState::new_two_player(42);
        let p0_artifact = permanent(
            &mut state,
            1,
            PlayerId(0),
            "Player Artifact",
            CoreType::Artifact,
        );
        let p0_creature = permanent(
            &mut state,
            2,
            PlayerId(0),
            "Player Creature",
            CoreType::Creature,
        );
        let p0_land = permanent(&mut state, 3, PlayerId(0), "Player Land", CoreType::Land);
        let p1_artifact = permanent(
            &mut state,
            4,
            PlayerId(1),
            "Opponent Artifact",
            CoreType::Artifact,
        );
        let p1_creature = permanent(
            &mut state,
            5,
            PlayerId(1),
            "Opponent Creature",
            CoreType::Creature,
        );
        let p1_land = permanent(&mut state, 6, PlayerId(1), "Opponent Land", CoreType::Land);
        let enchantment = permanent(
            &mut state,
            7,
            PlayerId(1),
            "Opponent Enchantment",
            CoreType::Enchantment,
        );

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::Or {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Artifact)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)),
                        TargetFilter::Typed(TypedFilter::new(TypeFilter::Land)),
                    ],
                },
                cant_regenerate: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        for destroyed in [
            p0_artifact,
            p0_creature,
            p0_land,
            p1_artifact,
            p1_creature,
            p1_land,
        ] {
            assert_eq!(state.objects[&destroyed].zone, Zone::Graveyard);
        }
        assert_eq!(state.objects[&enchantment].zone, Zone::Battlefield);
    }

    #[test]
    fn destroy_prevented_by_regen_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        // Add regeneration shield
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(bear_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature survived
        assert!(
            state.battlefield.contains(&bear_id),
            "Creature with regen shield should survive Destroy"
        );
        // No CreatureDestroyed event
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CreatureDestroyed { .. })));
        // Regenerated event emitted
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::Regenerated { .. })));
    }

    #[test]
    fn destroy_all_prevented_by_regen_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);

        // Protected creature
        let protected_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Shielded".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&protected_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&protected_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        // Unprotected creature
        let unprotected_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Unshielded".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&unprotected_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        // Protected creature survives
        assert!(
            state.battlefield.contains(&protected_id),
            "Creature with regen shield should survive DestroyAll"
        );
        // Unprotected creature destroyed
        assert!(
            !state.battlefield.contains(&unprotected_id),
            "Unshielded creature should be destroyed by DestroyAll"
        );
    }

    #[test]
    fn destroy_all_cant_regenerate_bypasses_shield() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let bear_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&bear_id)
            .unwrap()
            .replacement_definitions
            .push(shield);

        let ability = ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::None,
                cant_regenerate: true,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        assert!(
            !state.battlefield.contains(&bear_id),
            "cant_regenerate should bypass regen shield in DestroyAll"
        );
    }

    // ---------------------------------------------------------------------
    // "Destroyed this way" tracked-set regression tests (Fumigate class).
    // CR 603.7 + CR 701.8a: DestroyAll must record the actually-destroyed
    // objects into a tracked set so downstream sub-abilities referencing
    // `QuantityRef::TrackedSetSize` resolve against the correct count.
    // ---------------------------------------------------------------------

    use crate::types::ability::{QuantityRef, TypeFilter, TypedFilter};

    /// Builds the Fumigate-shape chain: `DestroyAll(creatures)` followed by
    /// `GainLife(amount = TrackedSetSize, player = Controller)`.
    fn fumigate_chain(source_id: ObjectId, controller: PlayerId) -> ResolvedAbility {
        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        );
        ResolvedAbility::new(
            Effect::DestroyAll {
                target: TargetFilter::Typed(TypedFilter {
                    type_filters: vec![TypeFilter::Creature],
                    controller: None,
                    properties: vec![],
                }),
                cant_regenerate: false,
            },
            vec![],
            source_id,
            controller,
        )
        .sub_ability(gain_life)
    }

    fn spawn_creature(
        state: &mut GameState,
        card_id: CardId,
        owner: PlayerId,
        name: &str,
    ) -> ObjectId {
        let id = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
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
    fn fumigate_gains_zero_life_when_no_creatures_on_battlefield() {
        let mut state = GameState::new_two_player(42);
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life);
        // A tracked set must still be recorded (even if empty) so stale prior
        // sets are not reused by TrackedSetSize.
        assert_eq!(state.tracked_object_sets.len(), 1);
        assert!(state
            .tracked_object_sets
            .values()
            .next()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn fumigate_gains_one_life_for_one_destroyed_creature() {
        let mut state = GameState::new_two_player(42);
        let _bear = spawn_creature(&mut state, CardId(1), PlayerId(0), "Bear");
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life + 1);
    }

    #[test]
    fn fumigate_gains_n_life_for_n_destroyed_creatures() {
        let mut state = GameState::new_two_player(42);
        for i in 0u64..5 {
            spawn_creature(
                &mut state,
                CardId(i + 1),
                PlayerId((i % 2) as u8),
                "Creature",
            );
        }
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life + 5);
    }

    #[test]
    fn fumigate_excludes_regenerated_creature_from_life_gained() {
        use crate::types::ability::ReplacementDefinition;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let shielded = spawn_creature(&mut state, CardId(1), PlayerId(0), "Shielded");
        // CR 701.8c: regeneration shield replaces the destruction.
        let shield = ReplacementDefinition::new(ReplacementEvent::Destroy)
            .valid_card(TargetFilter::SelfRef)
            .description("Regenerate".to_string())
            .regeneration_shield();
        state
            .objects
            .get_mut(&shielded)
            .unwrap()
            .replacement_definitions
            .push(shield);

        // Two unshielded creatures + one shielded = 2 should actually die.
        spawn_creature(&mut state, CardId(2), PlayerId(0), "UnshieldedA");
        spawn_creature(&mut state, CardId(3), PlayerId(1), "UnshieldedB");
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Life gained must equal *actually destroyed* count (2), not filter-matched (3).
        assert_eq!(state.players[0].life, starting_life + 2);
        // Regenerated creature survives.
        assert!(state.battlefield.contains(&shielded));
    }

    #[test]
    fn fumigate_excludes_indestructible_creature_from_life_gained() {
        let mut state = GameState::new_two_player(42);
        let god = spawn_creature(&mut state, CardId(1), PlayerId(0), "God");
        state
            .objects
            .get_mut(&god)
            .unwrap()
            .keywords
            .push(crate::types::keywords::Keyword::Indestructible);
        spawn_creature(&mut state, CardId(2), PlayerId(1), "Bear");
        let starting_life = state.players[0].life;

        let ability = fumigate_chain(ObjectId(100), PlayerId(0));
        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        // Only the non-indestructible creature was destroyed.
        assert_eq!(state.players[0].life, starting_life + 1);
        assert!(state.battlefield.contains(&god));
    }

    #[test]
    fn destroy_single_target_records_tracked_set_for_downstream_gain_life() {
        // Single-target `Destroy` variant (not DestroyAll) — the class fix must
        // cover both resolve() and resolve_all() paths.
        let mut state = GameState::new_two_player(42);
        let bear = spawn_creature(&mut state, CardId(1), PlayerId(1), "Bear");
        let starting_life = state.players[0].life;

        let gain_life = ResolvedAbility::new(
            Effect::GainLife {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::TrackedSetSize,
                },
                player: TargetFilter::Controller,
            },
            vec![],
            ObjectId(200),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
            vec![TargetRef::Object(bear)],
            ObjectId(200),
            PlayerId(0),
        )
        .sub_ability(gain_life);

        let mut events = Vec::new();
        resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert_eq!(state.players[0].life, starting_life + 1);
    }
}
