use crate::game::ability_utils::{resolve_multi_target_bounds, MultiTargetBounds};
use crate::game::engine::{PriorityAnnouncementFacadeAccess, PriorityPrincipal};
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::game_object::AttachTarget;
use crate::game::targeting::resolved_object_ids_for_filter;
use crate::types::ability::{
    AbilityCondition, Effect, EffectError, EffectKind, FilterProp, MultiTargetSpec, QuantityExpr,
    ResolvedAbility, TargetFilter, TargetRef, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::resolution::ChildStackDepth;
use crate::types::resolved_commands::{
    ResolvedAttachmentCommand, ResolvedAttachmentReplayInvariantError,
};
use crate::types::zones::Zone;

/// An engine-authored Equip announcement for the Priority preflight. The
/// Equipment identity remains provider-owned until the Priority facade rebuilds
/// the ordinary reducer primer.
pub(in crate::game) struct PriorityEquipAnnouncement {
    equipment_id: ObjectId,
}

impl PriorityEquipAnnouncement {
    fn new(equipment_id: ObjectId) -> Self {
        Self { equipment_id }
    }

    pub(in crate::game) fn equipment_id(
        &self,
        _access: &PriorityAnnouncementFacadeAccess,
    ) -> ObjectId {
        self.equipment_id
    }
}

/// Enumerates the attachment authority's finite Equip primers for the current
/// Priority holder. The ordinary reducer remains responsible for sorcery-speed
/// timing and target selection when the announcement is replayed.
pub(in crate::game) fn priority_equip_announcements(
    state: &GameState,
    principal: &PriorityPrincipal,
) -> Vec<PriorityEquipAnnouncement> {
    let controller = principal.semantic_holder();
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&equipment_id| {
            state.objects.get(&equipment_id).is_some_and(|equipment| {
                equipment.controller == controller
                    && equipment
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype == "Equipment")
                    && state.battlefield.iter().copied().any(|candidate_id| {
                        state.objects.get(&candidate_id).is_some_and(|candidate| {
                            candidate.controller == controller
                                && candidate
                                    .card_types
                                    .core_types
                                    .contains(&CoreType::Creature)
                        })
                    })
            })
        })
        .map(PriorityEquipAnnouncement::new)
        .collect()
}

/// The terminal state of one resolved Attach operation after any required
/// resolution-time selection has been made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachResolutionOutcome {
    Completed,
    Paused,
}

/// The prompt phase either hands a fully bound operation to the executor,
/// completes as a no-op, or parks another resolution-time choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttachPromptOutcome {
    Execute,
    Completed,
    Paused,
}

/// CR 701.3a + CR 701.3b: Attach — to place an Aura, Equipment, or Fortification on another object or player.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    resolve_with_prompt(state, ability, events).map(|_| ())
}

/// Prompt or park an Attach operation until both attachment roles are bound,
/// then execute it. Callers that already hold a bound attachment role must use
/// [`resolve_bound_attachment_operation`] directly so they cannot reopen the
/// same resolution-time prompt.
fn resolve_with_prompt(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<AttachResolutionOutcome, EffectError> {
    let attachment_filter = match &ability.effect {
        Effect::Attach { attachment, .. } => attachment,
        _ => &TargetFilter::SelfRef,
    };

    match prompt_resolution_attachment_choice(state, ability, attachment_filter, events)? {
        AttachPromptOutcome::Execute => resolve_bound_attachment_operation(state, ability, events),
        AttachPromptOutcome::Completed => Ok(AttachResolutionOutcome::Completed),
        AttachPromptOutcome::Paused => Ok(AttachResolutionOutcome::Paused),
    }
}

/// Execute an Attach operation whose resolution-time attachment roles are
/// already bound. This deliberately excludes prompt logic: re-entering it for
/// an Equipment answer can recreate the same `EffectZoneChoice` indefinitely.
fn resolve_bound_attachment_operation(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<AttachResolutionOutcome, EffectError> {
    let source_id = ability.source_id;
    let (attachment_filter, target_filter) = match &ability.effect {
        Effect::Attach { attachment, target } => (attachment, target),
        _ => (&TargetFilter::SelfRef, &TargetFilter::Any),
    };

    // CR 400.7 + CR 603.7c: a delayed attach whose pinned referent became a new
    // object attaches nothing. The trigger fired and resolved (CR 603.7b); it
    // affected nothing.
    //
    // PLACEMENT IS LOAD-BEARING — this MUST sit above the two `ok_or_else(..)?`
    // conversions below. Letting the substitution in `resolve_object_filter`
    // empty the list instead would surface `EffectError::MissingParam` and emit
    // NO EffectResolved. Mirrors the CR 303.4j no-op guard further down, which
    // is this function's proof that an events sink is in scope here.
    //
    // Reached by `gift of immortality` and `next of kin`: their ROOT is
    // `ChangeZone{SelfRef}`, but `ability_pins_object_anaphor` walks the whole
    // chain, so the `Attach` sub-ability's `ParentTarget` earns the pins.
    if ability.pinned_object_targets_all_stale(state) {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id,
            subject: None,
        });
        return Ok(AttachResolutionOutcome::Completed);
    }

    // CR 608.2h + CR 608.2k: Typed attachment operands resolve from the
    // battlefield/LKI unless they are explicit player-chosen targets.
    // Typed/scan-based attachment filters (e.g. "Equipment attached to ~") resolve
    // from the battlefield/LKI, not from explicit target slots. Consuming
    // `ability.targets` here would steal the ParentTarget bearer (Zack Fair).
    // `Any`/`Any` pairs share one iterator so [equipment, host] slots stay ordered.
    let mut target_slots = ability.targets.iter();
    let attachment_ids = if !ability.attach_attachment_targets().is_empty() {
        // CR 608.2d: This event-scoped Attach may resolve only the explicitly
        // selected member of its forwarded candidate set. Do not fall back to
        // a trigger event or a general ParentTarget projection.
        resolve_bound_attachment_targets(state, ability, attachment_filter)
    } else if matches!(attachment_filter, TargetFilter::ParentTarget) {
        resolve_parent_target_attachment_from_trigger(state)
            .or_else(|| resolve_bound_attachment_target(state, ability, attachment_filter))
            .or_else(|| resolve_object_filter(state, ability, attachment_filter, &mut target_slots))
            .into_iter()
            .collect()
    } else if attachment_filter_uses_explicit_target_slot(attachment_filter) {
        resolve_bound_attachment_target(state, ability, attachment_filter)
            .or_else(|| resolve_object_filter(state, ability, attachment_filter, &mut target_slots))
            .into_iter()
            .collect()
    } else {
        resolve_object_filter(state, ability, attachment_filter, &mut std::iter::empty())
            .into_iter()
            .collect()
    };
    if attachment_ids.is_empty() {
        return Err(EffectError::MissingParam(
            "No attachment for Attach".to_string(),
        ));
    }
    let target_id = resolve_attach_target(state, ability, target_filter, &mut target_slots)
        .ok_or_else(|| EffectError::MissingParam("No target for Attach".to_string()))?;

    for (index, attachment_id) in attachment_ids.iter().copied().enumerate() {
        // CR 303.4j: If an effect attempts to attach an Aura on the battlefield to an
        // object it can't legally enchant, the Aura doesn't move. Delegate to the single
        // COMPLETE legality authority (sba::is_valid_attachment_target) — attachment_illegality
        // (protection/prohibition) + the Aura's Enchant filter + the zone gate. A bespoke
        // Enchant-only check would silently miss zone/protection mismatches. Scoped to Aura
        // attachments so Equipment/Fortification resolution is unchanged.
        let attacher_is_aura = state
            .objects
            .get(&attachment_id)
            .is_some_and(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"));
        if attacher_is_aura
            && !crate::game::sba::is_valid_attachment_target(state, attachment_id, target_id)
        {
            // CR 303.4j: the aura doesn't move.
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id,
                subject: None,
            });
            continue;
        }

        // CR 701.3a + CR 614.1a: Route the attach through the replacement pipeline
        // so an "as it becomes attached, choose …" definition on the attachment
        // (`ReplacementEvent::Attached`, Psychic Paper) can bind its choice as the
        // attachment resolves — the attach-time analogue of "as ~ enters, choose".
        let child_stack_start = state.resolution_stack.capture_child_boundary();
        let proposed = crate::types::proposed_event::ProposedEvent::Attach {
            attachment_id,
            target_id,
            applied: Default::default(),
        };
        match crate::game::replacement::replace_event(state, proposed, events) {
            crate::game::replacement::ReplacementResult::Execute(_) => {
                if let Some(waiting_for) =
                    deliver_attach(state, attachment_id, target_id, source_id, events)
                {
                    if !ability.attach_attachment_targets().is_empty() {
                        defer_remaining_selected_attachments(
                            state,
                            ability,
                            &attachment_ids[index + 1..],
                            child_stack_start,
                        );
                    }
                    state.waiting_for = waiting_for;
                    return Ok(AttachResolutionOutcome::Paused);
                }
            }
            crate::game::replacement::ReplacementResult::Prevented => {
                events.push(GameEvent::EffectResolved {
                    kind: EffectKind::from(&ability.effect),
                    source_id,
                    subject: None,
                });
            }
            crate::game::replacement::ReplacementResult::NeedsChoice(player) => {
                // CR 616.1: multiple "becomes attached" replacements apply — park
                // for the ordering choice. `handle_replacement_choice`'s
                // `ProposedEvent::Attach` arm resumes via `deliver_attach` once
                // the player orders them.
                if !ability.attach_attachment_targets().is_empty() {
                    defer_remaining_selected_attachments(
                        state,
                        ability,
                        &attachment_ids[index + 1..],
                        child_stack_start,
                    );
                }
                crate::game::replacement::park_waiting_for(state, player);
                return Ok(AttachResolutionOutcome::Paused);
            }
        }
    }

    Ok(AttachResolutionOutcome::Completed)
}

/// CR 608.2c + CR 616.1: A selected attachment can open an attached-event
/// replacement prompt. Keep
/// only the unprocessed selected attachments in the active child so answering
/// that prompt resumes the next selection rather than replaying the first one
/// or skipping the remaining selections.
fn defer_remaining_selected_attachments(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining: &[ObjectId],
    child_stack_start: ChildStackDepth,
) {
    if remaining.is_empty() {
        return;
    }

    let mut remainder = ability.clone();
    let tail = remainder.sub_ability.take();
    remainder.set_attach_attachment_targets(
        ability
            .attach_attachment_targets()
            .iter()
            .copied()
            .filter(|target| remaining.contains(&target.object_id))
            .collect(),
    );

    // CR 608.2c + CR 616.1: an already selected multi-attachment operation
    // may pause after the current member raises a replacement child. Keep the
    // remaining bound members and the printed tail as separate ordinary
    // continuations, outside that exact child stack. `attachment_choice` is
    // reserved for a real EffectZoneChoice owner, never a synthetic marker.
    let mut remainder =
        crate::types::game_state::PendingContinuation::new(Box::new(remainder), state);
    remainder.attachment_remainder = Some(crate::types::game_state::PendingAttachmentRemainder {
        producer: Box::new(ability.clone()),
    });
    let tail = tail.map(|tail| crate::types::game_state::PendingContinuation::new(tail, state));
    match state
        .resolution_stack
        .capture_child_boundary()
        .cmp(&child_stack_start)
    {
        std::cmp::Ordering::Less => {
            panic!(
                "Attach delivery removed a parent before its remaining attachments could be parked"
            )
        }
        std::cmp::Ordering::Equal => {
            if let Some(tail) = tail {
                state.park_ability_continuation(tail);
            }
            state.park_ability_continuation(remainder);
        }
        std::cmp::Ordering::Greater => {
            state
                .insert_ability_continuation_parent_at_child_boundary(remainder, child_stack_start)
                .expect(
                    "remaining Attach members must be inserted outside their replacement child",
                );
            if let Some(tail) = tail {
                state
                    .insert_ability_continuation_parent_at_child_boundary(tail, child_stack_start)
                    .expect("Attach tail must be inserted outside its replacement child");
            }
        }
    }
}

/// CR 701.3a + CR 614.1a: Perform the attach mutation and, if the attachment
/// carries an "as it becomes attached, choose …" replacement, drain its
/// stashed continuation (`ReplacementEvent::Attached`). Single authority for
/// completing an attach after the replacement pipeline consult — used both by
/// the immediate `resolve()` path and by `handle_replacement_choice`'s
/// post-ordering-choice resume, so the two paths cannot drift apart.
pub(crate) fn deliver_attach(
    state: &mut GameState,
    attachment_id: ObjectId,
    target_id: ObjectId,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Option<WaitingFor> {
    if let Some(old_target) = attach_to(state, attachment_id, target_id) {
        events.push(GameEvent::Unattached {
            attachment_id,
            old_target,
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Attach,
        source_id,
        subject: None,
    });

    crate::game::engine_replacement::apply_pending_post_replacement_effect(
        state,
        Some(attachment_id),
        None,
        Some(crate::types::replacements::ReplacementEvent::Attached),
        events,
    )
}

/// CR 701.3d: Unattach each matching Equipment from the matched host, leaving
/// it on the battlefield but no longer attached.
pub fn resolve_unattach_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (attachment_filter, target_filter) = match &ability.effect {
        Effect::UnattachAll { attachment, target } => (attachment, target),
        _ => (&TargetFilter::Any, &TargetFilter::Any),
    };

    let target_ids = resolved_object_ids_for_filter(state, ability, target_filter);

    let ctx = FilterContext::from_ability(ability);
    // CR 608.2c + CR 701.3d: A context-ref attachment anaphor ("unattach it" →
    // `ParentTarget`/`ParentTargetSlot`) designates the snapshot object recorded
    // when the effect was created, not a live filter match — `matches_target_filter`
    // returns false for positive parent-refs (filter.rs: "resolve at resolution
    // time"). Resolve it here against the ability's target snapshot, mirroring how
    // the sibling `resolve_attach` routes `ParentTarget` through resolution rather
    // than object matching. Typed host filters and `SelfRef` stay on the
    // `matches_target_filter` path.
    let explicit_attachment_ids: Option<Vec<ObjectId>> = matches!(
        attachment_filter,
        TargetFilter::ParentTarget | TargetFilter::ParentTargetSlot { .. }
    )
    .then(|| {
        // CR 400.7 + CR 603.7c: substitution only. Population inside the class
        // is 0 (zero `UnattachAll` nodes), and the one card that reaches this
        // (`stolen uniform`) is denied a pin by
        // `condition_names_referent_zone_change`, so `live_object_targets` is
        // the identity here today. Applied for uniformity and defence in depth;
        // deliberately NO early return, since a population of 0 admits no
        // non-vacuous test.
        //
        // Slot carve-out DOES apply: `attachment_filter` may be
        // `ParentTargetSlot`, and `effect_object_targets` indexes it
        // positionally. This is the second call site of the standing
        // 22-call-site constraint.
        let live_targets = ability.live_object_targets(state);
        let pool: &[TargetRef] =
            if matches!(attachment_filter, TargetFilter::ParentTargetSlot { .. }) {
                &ability.targets
            } else {
                &live_targets
            };
        super::effect_object_targets(attachment_filter, pool)
    });
    for target_id in target_ids {
        let attachments = state
            .objects
            .get(&target_id)
            .map(|target| target.attachments.clone())
            .unwrap_or_default();
        for attachment_id in attachments {
            let keep = match &explicit_attachment_ids {
                Some(ids) => ids.contains(&attachment_id),
                None => matches_target_filter(state, attachment_id, attachment_filter, &ctx),
            };
            if !keep {
                continue;
            }
            if let Some(old_target) = unattach(state, attachment_id) {
                events.push(GameEvent::Unattached {
                    attachment_id,
                    old_target,
                });
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

pub(crate) fn target_ref_from_attach_target(target: AttachTarget) -> TargetRef {
    match target {
        AttachTarget::Object(id) => TargetRef::Object(id),
        AttachTarget::Player(id) => TargetRef::Player(id),
    }
}

fn attachment_tree_contains(state: &GameState, root_id: ObjectId, candidate_id: ObjectId) -> bool {
    let mut remaining = vec![root_id];
    let mut visited = Vec::new();

    while let Some(id) = remaining.pop() {
        if visited.contains(&id) {
            continue;
        }
        if id == candidate_id {
            return true;
        }
        visited.push(id);
        if let Some(object) = state.objects.get(&id) {
            remaining.extend(object.attachments.iter().copied());
        }
    }

    false
}

/// CR 608.2d + CR 601.2c: Optional or activation-deferred attach sub-instructions
/// (Nahiri, the Lithomancer +2) choose the Equipment only after the controller
/// accepts at resolution. When multiple Equipment match, prompt instead of
/// auto-attaching the first battlefield scan result.
fn prompt_resolution_attachment_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    attachment_filter: &TargetFilter,
    events: &mut Vec<GameEvent>,
) -> Result<AttachPromptOutcome, EffectError> {
    if !ability.attach_attachment_candidates().is_empty() {
        return prompt_forwarded_attachment_choice(state, ability, events);
    }
    let candidates = attachment_candidates_from_zone_change(
        state,
        ability,
        state.last_zone_changed_ids.as_slice(),
    );
    if !candidates.is_empty() {
        let mut bound = ability.clone();
        bound.bind_attach_attachment_candidates(candidates);
        return prompt_forwarded_attachment_choice(state, &bound, events);
    }
    if !attachment_filter_uses_explicit_target_slot(attachment_filter) {
        return Ok(AttachPromptOutcome::Execute);
    }
    if explicit_attachment_target_chosen(state, ability, attachment_filter) {
        return Ok(AttachPromptOutcome::Execute);
    }

    let ctx = FilterContext::from_ability(ability);
    let effective = crate::game::effects::resolved_object_filter(ability, attachment_filter);
    let eligible: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| matches_target_filter(state, *id, &effective, &ctx))
        .collect();

    let bounds = attachment_choice_bounds(state, ability, eligible.len())?;

    match (eligible.len(), bounds.min, bounds.max) {
        // CR 608.2d: a player can't choose an illegal or impossible option. With
        // no eligible attachment there is nothing to choose, so the "attach an
        // Equipment you control" selection can't be made and the effect does
        // nothing. An empty candidate list paired with `min_count >= 1` would
        // otherwise build an unsatisfiable `EffectZoneChoice` and deadlock the
        // game — Nahiri, the Lithomancer's "You may attach an Equipment you
        // control to it" when the controller has no Equipment: the "may" gate is
        // accepted, clearing the optional flag, so `attachment_choice_bounds`
        // defaults to {min:1, max:1} against zero candidates. Subsumes the prior
        // `(0, 0, _)` no-op arm.
        (0, _, _) | (_, 0, 0) => Ok(AttachPromptOutcome::Completed),
        (1, 1, 1) => Ok(AttachPromptOutcome::Execute),
        _ => {
            park_resolution_attachment_choice(state, ability, eligible, bounds);
            Ok(AttachPromptOutcome::Paused)
        }
    }
}

/// CR 400.7 + CR 608.2c + CR 701.3b: A `ZoneChangedThisWay` Attach whose attachment
/// operand is `ParentTarget` means "one of them", not merely the most recent
/// trigger event. Preserve the exact event-scoped battlefield objects as typed
/// attachment candidates so the prompt never includes preexisting Equipment.
pub(crate) fn attachment_candidates_from_zone_change(
    state: &GameState,
    ability: &ResolvedAbility,
    object_ids: &[ObjectId],
) -> Vec<ObjectIncarnationRef> {
    let Effect::Attach {
        attachment: TargetFilter::ParentTarget,
        ..
    } = &ability.effect
    else {
        return Vec::new();
    };
    let Some(AbilityCondition::ZoneChangedThisWay {
        filter,
        destination: Some(Zone::Battlefield),
    }) = ability.condition.as_ref()
    else {
        return Vec::new();
    };

    let context = FilterContext::from_ability(ability);
    object_ids
        .iter()
        .filter_map(|&object_id| {
            let object = state.objects.get(&object_id)?;
            (object.zone == Zone::Battlefield
                && matches_target_filter(state, object_id, filter, &context))
            .then(|| ObjectIncarnationRef::from_object(object))
        })
        .collect()
}

/// CR 115.10a: An Attach whose attachment operand is the moved set from a
/// `forward_result` parent must choose its host first, then choose
/// from that parent-owned set. Neither choice is targeting, and the attachment
/// pool must never be recomputed from the battlefield after the event window.
fn prompt_forwarded_attachment_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<AttachPromptOutcome, EffectError> {
    let candidates: Vec<ObjectId> = ability
        .attach_attachment_candidates()
        .iter()
        .filter(|candidate| candidate.is_current(state))
        .map(|candidate| candidate.object_id)
        .collect();
    if candidates.is_empty() {
        return Ok(AttachPromptOutcome::Completed);
    }

    let Effect::Attach { target, .. } = &ability.effect else {
        unreachable!("forwarded attachment choices only resolve Attach effects");
    };

    if ability.attach_host_target().is_none() {
        let hosts = resolved_object_ids_for_filter(state, ability, target);
        match hosts.len() {
            0 => return Ok(AttachPromptOutcome::Completed),
            1 => {
                let mut bound = ability.clone();
                let host = state
                    .objects
                    .get(&hosts[0])
                    .map(ObjectIncarnationRef::from_object)
                    .expect("resolved attachment host must exist");
                bound.bind_attach_host_target(host);
                return prompt_forwarded_attachment_choice(state, &bound, events);
            }
            _ => {
                park_resolution_attachment_choice(
                    state,
                    ability,
                    hosts,
                    MultiTargetBounds { min: 1, max: 1 },
                );
                return Ok(AttachPromptOutcome::Paused);
            }
        }
    }

    if !ability.attach_attachment_targets().is_empty() {
        return Ok(AttachPromptOutcome::Execute);
    }

    let bounds = attachment_choice_bounds(state, ability, candidates.len())?;
    match (candidates.len(), bounds.min, bounds.max) {
        (0, _, _) | (_, 0, 0) => Ok(AttachPromptOutcome::Completed),
        (1, 1, 1) => {
            let mut bound = ability.clone();
            let attachment = state
                .objects
                .get(&candidates[0])
                .map(ObjectIncarnationRef::from_object)
                .expect("resolved attachment candidate must exist");
            bound.bind_attach_attachment_target(attachment);
            match resolve_bound_attachment_operation(state, &bound, events)? {
                AttachResolutionOutcome::Completed => Ok(AttachPromptOutcome::Completed),
                AttachResolutionOutcome::Paused => Ok(AttachPromptOutcome::Paused),
            }
        }
        _ => {
            park_resolution_attachment_choice(state, ability, candidates, bounds);
            Ok(AttachPromptOutcome::Paused)
        }
    }
}

/// CR 608.2d: Park the exact Attach instruction that owns a resolution-time
/// choice. The child frame retains only that Attach operation; its parent owns
/// the printed chain tail, so resolving either a host or attachment choice
/// cannot discard or duplicate later instructions.
fn park_resolution_attachment_choice(
    state: &mut GameState,
    ability: &ResolvedAbility,
    cards: Vec<ObjectId>,
    bounds: MultiTargetBounds,
) {
    let mut operation = ability.clone();
    let tail = operation.sub_ability.take();
    let waiting_for = WaitingFor::EffectZoneChoice {
        player: ability.controller,
        cards,
        count: bounds.max,
        min_count: bounds.min,
        up_to: bounds.min != bounds.max,
        source_id: ability.source_id,
        effect_kind: EffectKind::Attach,
        zone: Zone::Battlefield,
        destination: None,
        enter_tapped: crate::types::zones::EtbTapState::Unspecified,
        enter_transformed: false,
        enters_under_player: None,
        enters_attacking: false,
        owner_library: false,
        track_exiled_by_source: false,
        face_down_profile: None,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        count_param: 0,
        library_position: None,
        mass_library_order: None,
        is_cost_payment: false,
        enters_modified_if: None,
        duration: None,
    };

    if let Some(active) = state.active_ability_continuation_frame() {
        if active.pending.attachment_choice.is_some() {
            debug_assert!(
                tail.is_none(),
                "a re-parked attachment operation must already have split off its tail"
            );
            let mut child = active.clone();
            child.pending.chain = Box::new(operation.clone());
            child.pending.attachment_choice =
                Some(crate::types::game_state::PendingAttachmentChoice {
                    operation: Box::new(operation),
                });
            state
                .resolve_and_apply_frame_transition(
                    crate::types::resolved_commands::ResolvedFrameTransition::ReplaceActive {
                        frame: crate::types::resolution::ResolutionFrame::AbilityContinuation(
                            child,
                        ),
                    },
                )
                .expect("attachment choice child frame must re-park atomically");
            state.waiting_for = waiting_for;
            return;
        }
    }

    if let Some(tail) = tail {
        if let Some(frame) = state.active_ability_continuation_frame_mut() {
            // The active frame is the continuation this Attach was resolving.
            // Preserve every frame-owned context while replacing only its
            // executable chain with the exact following tail.
            frame.pending.chain = tail;
            frame.pending.attachment_choice = None;
        } else {
            state.park_ability_continuation(crate::types::game_state::PendingContinuation::new(
                tail, state,
            ));
        }
    }

    let mut child =
        crate::types::game_state::PendingContinuation::new(Box::new(operation.clone()), state);
    child.attachment_choice = Some(crate::types::game_state::PendingAttachmentChoice {
        operation: Box::new(operation),
    });
    state
        .resolve_and_apply_frame_transition(
            crate::types::resolved_commands::ResolvedFrameTransition::Push {
                frame: crate::types::resolution::ResolutionFrame::AbilityContinuation(
                    crate::types::resolution::AbilityContinuationFrame {
                        pending: child,
                        choose_zone_trigger_context: None,
                    },
                ),
            },
        )
        .expect("attachment choice child frame must push atomically");
    state.waiting_for = waiting_for;
}

fn attachment_choice_bounds(
    state: &GameState,
    ability: &ResolvedAbility,
    eligible_count: usize,
) -> Result<MultiTargetBounds, EffectError> {
    if let Some(spec) = &ability.multi_target {
        return resolve_multi_target_bounds(state, ability, spec, eligible_count)
            .map_err(|error| EffectError::InvalidParam(error.to_string()));
    }
    if ability.targeting_is_optional() {
        let spec = MultiTargetSpec::up_to(QuantityExpr::Fixed { value: 1 });
        return resolve_multi_target_bounds(state, ability, &spec, eligible_count)
            .map_err(|error| EffectError::InvalidParam(error.to_string()));
    }
    Ok(MultiTargetBounds { min: 1, max: 1 })
}

/// Resume an attach sub-instruction paused on `EffectZoneChoice`.
#[cfg(test)]
pub(crate) fn complete_resolution_attachment_choice(
    state: &mut GameState,
    ability: ResolvedAbility,
    attachment_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let choice_ability = bind_resolution_attachment_choice(state, ability, attachment_ids)?;
    resolve_bound_attachment_operation(state, &choice_ability, events).map(|_| ())
}

/// Bind the answer to a resolution-time attachment choice before the child
/// operation is resumed. The binding records exact object incarnations, so a
/// later re-park cannot attach a new object that reused the selected id.
pub(crate) fn bind_resolution_attachment_choice(
    state: &GameState,
    ability: ResolvedAbility,
    attachment_ids: &[ObjectId],
) -> Result<ResolvedAbility, EffectError> {
    let selecting_host = !ability.attach_attachment_candidates().is_empty()
        && ability.attach_host_target().is_none();
    let mut choice_ability = ability;
    choice_ability.sub_ability = None;
    for &selected_id in attachment_ids {
        choice_ability.targets.push(TargetRef::Object(selected_id));
        let selected = state
            .objects
            .get(&selected_id)
            .map(ObjectIncarnationRef::from_object)
            .ok_or_else(|| {
                EffectError::MissingParam("Selected attachment choice left play".to_string())
            })?;
        if selecting_host {
            choice_ability.bind_attach_host_target(selected);
        } else {
            choice_ability.bind_attach_attachment_target(selected);
        }
    }
    Ok(choice_ability)
}

/// CR 608.2c + CR 608.2d: Bind and resolve an Attach `EffectZoneChoice`
/// against the exact child frame that owns the prompt. The resolver must read
/// the stored, bound operation: resolving a detached clone leaves the child
/// carrying its pre-choice operands, so its eventual continuation cannot
/// deliver the selected attachment.
///
/// Returns `true` only after the bound operation completed to priority and its
/// typed child was retired. A host answer that re-parks an Equipment answer, or
/// an Attached replacement that parks a post-replacement child, leaves its
/// current owner in place and returns `false`.
pub(crate) fn resolve_selected_attachment_choice(
    state: &mut GameState,
    attachment_ids: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<bool, EffectError> {
    let operation = state
        .active_ability_continuation_frame()
        .and_then(|frame| frame.pending.attachment_choice.as_ref())
        .map(|choice| choice.operation.as_ref().clone())
        .ok_or_else(|| {
            EffectError::MissingParam(
                "Attach EffectZoneChoice missing typed attachment operation".to_string(),
            )
        })?;
    let selecting_host = !operation.attach_attachment_candidates().is_empty()
        && operation.attach_host_target().is_none();
    let bound_operation = bind_resolution_attachment_choice(state, operation, attachment_ids)?;
    let operation = {
        let frame = state
            .active_ability_continuation_frame_mut()
            .expect("Attach EffectZoneChoice owner must remain active while binding");
        let choice = frame
            .pending
            .attachment_choice
            .as_mut()
            .expect("Attach EffectZoneChoice owner must retain its typed operation");
        *choice.operation = bound_operation;
        let operation = choice.operation.as_ref().clone();
        *frame.pending.chain = operation.clone();
        operation
    };

    // A host answer must re-enter the prompt phase to choose from the
    // forwarded Equipment set. An already-bound Equipment answer instead
    // executes directly, avoiding a recursive replay of its own prompt.
    let outcome = if selecting_host {
        resolve_with_prompt(state, &operation, events)?
    } else {
        resolve_bound_attachment_operation(state, &operation, events)?
    };

    match outcome {
        AttachResolutionOutcome::Paused => Ok(false),
        AttachResolutionOutcome::Completed => {
            let _ = state
                .take_active_attachment_choice_continuation()
                .expect("completed attach choice must retain its active child")
                .expect("completed attach choice must own its active child");
            Ok(true)
        }
    }
}

/// Resolve an explicitly chosen attachment through its role binding before the
/// generic target projection. Resolution-time attachment choices append to
/// `targets` after their event-context host, so position alone cannot identify
/// the Equipment without this binding.
fn resolve_bound_attachment_target(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<ObjectId> {
    resolve_bound_attachment_targets(state, ability, filter)
        .into_iter()
        .next()
}

fn resolve_bound_attachment_targets(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Vec<ObjectId> {
    if !ability.attach_attachment_candidates().is_empty() {
        return ability
            .attach_attachment_targets()
            .iter()
            .filter_map(|target| {
                (target.is_current(state)
                    && ability
                        .attach_attachment_candidates()
                        .iter()
                        .any(|candidate| candidate == target))
                .then_some(target.object_id)
            })
            .collect();
    }
    let ctx = FilterContext::from_ability(ability);
    let effective = crate::game::effects::resolved_object_filter(ability, filter);
    ability
        .attach_attachment_targets()
        .iter()
        .filter_map(|target| {
            (target.is_current(state)
                && matches_target_filter(state, target.object_id, &effective, &ctx))
            .then_some(target.object_id)
        })
        .collect()
}

/// CR 608.2d + CR 400.7: Resolve the host role of an attachment instruction. A
/// role-bound host wins; otherwise preserve the legacy parent-target anaphor
/// while excluding attachment-role objects, then use the captured battlefield
/// event as the final event-context fallback. This keeps an event host and a
/// resolution-time selected Equipment from swapping roles.
fn resolve_attach_target<'a>(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    target_slots: &mut impl Iterator<Item = &'a TargetRef>,
) -> Option<ObjectId> {
    if let Some(host) = ability.attach_host_target() {
        if !host.is_current(state) {
            return None;
        }
        if matches!(filter, TargetFilter::ParentTarget) {
            return Some(host.object_id);
        }
        let ctx = FilterContext::from_ability(ability);
        let effective = crate::game::effects::resolved_object_filter(ability, filter);
        return matches_target_filter(state, host.object_id, &effective, &ctx)
            .then_some(host.object_id);
    }

    match filter {
        TargetFilter::ParentTarget => {
            let attachment_ids = current_attachment_target_ids(state, ability);
            ability
                .live_object_targets(state)
                .into_iter()
                .find_map(|target| match target {
                    TargetRef::Object(id) if !attachment_ids.contains(&id) => Some(id),
                    _ => None,
                })
                .or_else(|| resolve_parent_target_host_from_trigger(state))
        }
        TargetFilter::LastCreated | TargetFilter::LastRevealed | TargetFilter::LastZoneChanged => {
            resolve_dynamic_attach_host_target(state, ability, filter, target_slots)
        }
        _ => resolve_object_filter(state, ability, filter, target_slots),
    }
}

/// CR 400.7: Return attachment role ids only while their pinned incarnations
/// remain current. A later object with the same id is a new object after a zone
/// change, so this dynamic-host exclusion must not exclude it.
fn current_attachment_target_ids(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    ability
        .attach_attachment_targets()
        .iter()
        .filter(|target| target.is_current(state))
        .map(|target| target.object_id)
        .collect()
}

/// Resolve a resolution-local host referent without allowing a just-selected
/// attachment to fill both roles. Explicit `LastCreated` targets retain the
/// established parent-chain precedence; the other two dynamic filters require
/// that an explicit target still belongs to their respective ledgers.
fn resolve_dynamic_attach_host_target<'a>(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    target_slots: &mut impl Iterator<Item = &'a TargetRef>,
) -> Option<ObjectId> {
    let attachment_ids = current_attachment_target_ids(state, ability);
    let dynamic_ids = match filter {
        TargetFilter::LastCreated => &state.last_created_token_ids,
        TargetFilter::LastRevealed => &state.last_revealed_ids,
        TargetFilter::LastZoneChanged => &state.last_zone_changed_ids,
        _ => unreachable!("dynamic attachment-host resolution only receives ledger filters"),
    };
    let explicit_host = target_slots.find_map(|target| match target {
        TargetRef::Object(id)
            if !attachment_ids.contains(id)
                && (matches!(filter, TargetFilter::LastCreated) || dynamic_ids.contains(id)) =>
        {
            Some(*id)
        }
        TargetRef::Object(_) | TargetRef::Player(_) => None,
    });
    explicit_host.or_else(|| {
        dynamic_ids
            .iter()
            .copied()
            .find(|id| !attachment_ids.contains(id))
    })
}

fn resolve_parent_target_host_from_trigger(state: &GameState) -> Option<ObjectId> {
    match state.current_trigger_event.as_ref()? {
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            ..
        } => Some(*object_id),
        _ => None,
    }
}

fn explicit_attachment_target_chosen(
    state: &GameState,
    ability: &ResolvedAbility,
    attachment_filter: &TargetFilter,
) -> bool {
    let ctx = FilterContext::from_ability(ability);
    let effective = crate::game::effects::resolved_object_filter(ability, attachment_filter);
    ability.targets.iter().any(|target| {
        matches!(
            target,
            TargetRef::Object(id) if matches_target_filter(state, *id, &effective, &ctx)
        )
    })
}

/// CR 603.6a + CR 608.2c + CR 301.5b: In "When this/that Equipment enters,
/// attach it/that Equipment to target creature" triggers, the attachment operand
/// is the object that caused the ETB trigger, while the creature is the chosen
/// target. Resolve only real attachment subtypes from the trigger event so
/// general ParentTarget target inheritance remains unchanged.
fn resolve_parent_target_attachment_from_trigger(state: &GameState) -> Option<ObjectId> {
    let object_id = match state.current_trigger_event.as_ref()? {
        GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            ..
        } => *object_id,
        _ => return None,
    };
    state.objects.get(&object_id).and_then(|obj| {
        if obj
            .card_types
            .subtypes
            .iter()
            .any(|subtype| matches!(subtype.as_str(), "Aura" | "Equipment" | "Fortification"))
        {
            Some(object_id)
        } else {
            None
        }
    })
}

/// Only explicit attachment choices consume player-chosen target slots.
/// Scan-based filters (e.g. "Equipment that was attached to ~") resolve from
/// the battlefield or LKI and must not steal `ParentTarget` slots.
fn attachment_filter_uses_explicit_target_slot(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(tf) => !tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::AttachedToSource)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => filters
            .iter()
            .any(attachment_filter_uses_explicit_target_slot),
        TargetFilter::Not { filter } => attachment_filter_uses_explicit_target_slot(filter),
        _ => false,
    }
}

fn resolve_object_filter<'a>(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
    target_slots: &mut impl Iterator<Item = &'a TargetRef>,
) -> Option<ObjectId> {
    match filter {
        TargetFilter::SelfRef => Some(ability.source_id),
        TargetFilter::LastCreated => target_slots
            .find_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                TargetRef::Player(_) => None,
            })
            .or_else(|| state.last_created_token_ids.first().copied()),
        TargetFilter::TriggeringSource | TargetFilter::AttachedTo => {
            crate::game::targeting::resolve_event_context_target(state, filter, ability.source_id)
                .and_then(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                })
        }
        // CR 400.7 + CR 603.7c: a pinned referent that became a new object is
        // dropped. The all-stale case is caught by the early return at the top
        // of `resolve` (which can emit `EffectResolved`); this substitution
        // handles the PARTIAL-stale case.
        TargetFilter::ParentTarget => {
            ability
                .live_object_targets(state)
                .into_iter()
                .find_map(|target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                })
        }
        // CR 608.2c: a precise slot anaphor ("Attach it to the chosen creature"
        // → attachment slot 1, target slot 0) resolves against the whole
        // resolving chain's accumulated targets. The per-clause `ability.targets`
        // may carry only this clause's nearest target, so route through
        // `resolved_targets`, whose `ParentTargetSlot` arm walks the ROOT chain
        // (CR 608.2c) — the same authority the GainControl handler falls back to.
        // The shared `target_slots` iterator is intentionally not consumed here.
        TargetFilter::ParentTargetSlot { index } => {
            crate::game::targeting::resolve_parent_slot_from_root(state, ability, *index).and_then(
                |target| match target {
                    TargetRef::Object(id) => Some(id),
                    TargetRef::Player(_) => None,
                },
            )
        }
        _ => {
            let ctx = FilterContext::from_ability(ability);
            target_slots
                .find_map(|target| match target {
                    TargetRef::Object(id) if matches_target_filter(state, *id, filter, &ctx) => {
                        Some(*id)
                    }
                    _ => None,
                })
                .or_else(|| {
                    resolved_object_ids_for_filter(state, ability, filter)
                        .into_iter()
                        .next()
                })
                .or_else(|| resolve_attached_to_source_lki_attachment(state, ability, filter))
        }
    }
}

fn filter_has_attached_to_source(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(tf) => tf
            .properties
            .iter()
            .any(|p| matches!(p, FilterProp::AttachedToSource)),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            filters.iter().any(filter_has_attached_to_source)
        }
        TargetFilter::Not { filter } => filter_has_attached_to_source(filter),
        _ => false,
    }
}

fn strip_attached_to_source_prop(filter: &TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed(tf) => TargetFilter::Typed(TypedFilter {
            properties: tf
                .properties
                .iter()
                .filter(|p| !matches!(p, FilterProp::AttachedToSource))
                .cloned()
                .collect(),
            ..tf.clone()
        }),
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.iter().map(strip_attached_to_source_prop).collect(),
        },
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.iter().map(strip_attached_to_source_prop).collect(),
        },
        TargetFilter::Not { filter } => TargetFilter::Not {
            filter: Box::new(strip_attached_to_source_prop(filter)),
        },
        other => other.clone(),
    }
}

/// CR 400.7j + CR 608.2h: Zack Fair — after self-sacrifice the Equipment operand
/// is no longer `AttachedToSource` on the battlefield; resolve it from the
/// source's departure attachment snapshot instead of the global filter predicate.
fn resolve_attached_to_source_lki_attachment(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<ObjectId> {
    if !filter_has_attached_to_source(filter) {
        return None;
    }
    let source_id = ability.source_id;
    let ctx = FilterContext::from_ability(ability);
    let without_attached = strip_attached_to_source_prop(filter);
    state
        .zone_changes_this_turn
        .iter()
        .rev()
        .find(|record| record.object_id == source_id)
        .and_then(|record| {
            record.attachments.iter().find_map(|snap| {
                if state.objects.contains_key(&snap.object_id)
                    && matches_target_filter(state, snap.object_id, &without_attached, &ctx)
                {
                    Some(snap.object_id)
                } else {
                    None
                }
            })
        })
}

/// CR 614.12 + CR 701.3a/b: whose characteristics the ATTACHMENT side of an
/// attachment-legality gate is read from.
///
/// CR 701.3b makes an attach attempt at an illegal host a silent no-op, so the
/// gate below is the ONLY thing standing between a decided attachment and a
/// permanent left dangling for the CR 704.5m sweep. A seam that decided the host
/// was legal must therefore be able to make the gate read the SAME object it
/// decided against: an entrant whose CR 707.9 copy exceptions have not yet been
/// stamped onto the object stored under its id is a different object for
/// protection (CR 702.16c) and attachment-restriction (CR 301.5) purposes than
/// the one the decision saw.
///
/// Borrowed rather than owned: the projection is owned by the deciding seam (or
/// by the state slot that parks it across a player-choice pause), and the gate
/// only reads it.
#[derive(Debug, Clone, Copy)]
pub(crate) enum AttachmentAuthority<'a> {
    /// The object stored under the attachment's id already IS the attachment.
    /// Read it live, so a change to the permanent between a decision and this
    /// gate is never masked by a stale snapshot.
    Stored,
    /// CR 614.12: the attachment as it will exist on the battlefield, supplied
    /// by the seam that holds it because the stored object does not match it yet.
    Projected(&'a crate::game::game_object::GameObject),
}

/// CR 303.4 + CR 301.5: is the attachment an Aura, per the supplied authority?
///
/// Player hosts are Auras-only (see [`attach_to_player`]); a copy exception that
/// adds or removes the `Aura` subtype (CR 205.1a) changes the answer, so this
/// reads the authority rather than the stored object.
pub(crate) fn authority_is_aura(
    state: &GameState,
    attachment_id: ObjectId,
    authority: AttachmentAuthority<'_>,
) -> bool {
    let attachment = match authority {
        AttachmentAuthority::Stored => state.objects.get(&attachment_id),
        AttachmentAuthority::Projected(projection) => Some(projection),
    };
    attachment.is_some_and(|obj| obj.card_types.subtypes.iter().any(|s| s == "Aura"))
}

/// [`can_attach_to_object`] against an explicit [`AttachmentAuthority`].
pub(crate) fn can_attach_to_object_with_authority(
    state: &GameState,
    attachment_id: ObjectId,
    target_id: ObjectId,
    authority: AttachmentAuthority<'_>,
) -> bool {
    match authority {
        AttachmentAuthority::Stored => can_attach_to_object(state, attachment_id, target_id),
        AttachmentAuthority::Projected(projection) => {
            can_attach_to_object_projected(state, attachment_id, Some(projection), target_id)
        }
    }
}

/// [`can_attach_to_player`] against an explicit [`AttachmentAuthority`].
pub(crate) fn can_attach_to_player_with_authority(
    state: &GameState,
    attachment_id: ObjectId,
    target_player: PlayerId,
    authority: AttachmentAuthority<'_>,
) -> bool {
    match authority {
        AttachmentAuthority::Stored => can_attach_to_player(state, attachment_id, target_player),
        AttachmentAuthority::Projected(projection) => {
            can_attach_to_player_projected(state, Some(projection), target_player)
        }
    }
}

/// CR 701.3c: Attaching to a different object gives the attachment a new timestamp.
/// Core attachment logic: attach `attachment_id` to `target_id`.
/// Handles detaching from a previous target if already attached.
pub fn attach_to(
    state: &mut GameState,
    attachment_id: ObjectId,
    target_id: ObjectId,
) -> Option<TargetRef> {
    attach_to_with_authority(state, attachment_id, target_id, AttachmentAuthority::Stored)
}

/// CR 614.12 + CR 701.3a: [`attach_to`] whose CR 701.3b legality gate reads the
/// supplied [`AttachmentAuthority`] instead of the stored object.
///
/// Only the GATE is projected. The edit itself — `attached_to`, the host's
/// `attachments` list, the CR 613.7e timestamp, the resolved-commands journal
/// row — is
/// applied to the object actually stored under `attachment_id`, because that is
/// the object that will carry the attachment.
pub(crate) fn attach_to_with_authority(
    state: &mut GameState,
    attachment_id: ObjectId,
    target_id: ObjectId,
    authority: AttachmentAuthority<'_>,
) -> Option<TargetRef> {
    let legal = can_attach_to_object_with_authority(state, attachment_id, target_id, authority);
    if !legal {
        return None;
    }

    // CR 613.7e + CR 701.3b/c: read the UNFILTERED prior host once. The timestamp
    // bump (below) must distinguish first-attach (None) from a same-host re-attach
    // (Some(host)); `old_target` collapses both to `None` and cannot. `old_target`
    // and the CR 733 command's `expected_old_host` are both views of this one read
    // rather than repeat queries of the attachment's host.
    let attachment_object = state.objects.get(&attachment_id);
    let reference = attachment_object.map(ObjectIncarnationRef::from_object);
    let expected_old_host = attachment_object.and_then(|obj| obj.attached_to);
    let prior_host = expected_old_host.map(target_ref_from_attach_target);
    // Discriminate the timestamp bump (below) before `filter` consumes `prior_host`:
    // `==` borrows, `Option::filter` moves. True on first-attach (None) or a move to a
    // different host; false only on a same-host re-attach (CR 701.3b keeps the stamp).
    let moving_to_new_host = prior_host != Some(TargetRef::Object(target_id));
    let old_target = prior_host.filter(|target| *target != TargetRef::Object(target_id));

    // CR 701.3a: Attaching moves attachment onto target.
    // If already attached to something, detach first. We only need to clear an
    // Object host's `attachments` list — a Player host has no such list.
    if let Some(old_target_id) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
        .and_then(|t| t.as_object())
    {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }

    // Set attached_to on the attachment. `From<ObjectId> for AttachTarget`
    // selects the `Object` variant; player attachment has its own entry point
    // (`attach_to_player`).
    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = Some(target_id.into());
    }

    // CR 613.7e + CR 701.3c: first attach (None) or a move to a different host
    // bumps the timestamp; a same-host re-attach (CR 701.3b) does not.
    let resulting_timestamp = moving_to_new_host.then(|| {
        let ts = state.next_timestamp();
        if let Some(attachment) = state.objects.get_mut(&attachment_id) {
            attachment.timestamp = ts;
        }
        ts
    });

    // Add to target's attachments list
    if let Some(target) = state.objects.get_mut(&target_id) {
        if !target.attachments.contains(&attachment_id) {
            target.attachments.push(attachment_id);
        }
    }

    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);

    // CR 733: journal the settled edit through its owning family. A same-host
    // re-attach (CR 701.3b) leaves every field above unchanged, so only a real
    // host transition is recorded.
    if let Some(reference) = reference.filter(|_| moving_to_new_host) {
        record_attachment_edit(
            state,
            reference,
            expected_old_host,
            Some(AttachTarget::Object(target_id)),
            resulting_timestamp,
        );
    }
    old_target
}

/// Stamps one settled attachment-graph edit into the CR 733 resolved-command
/// journal on behalf of the three attachment authorities.
fn record_attachment_edit(
    state: &mut GameState,
    attachment: ObjectIncarnationRef,
    expected_old_host: Option<AttachTarget>,
    resulting_host: Option<AttachTarget>,
    resulting_timestamp: Option<u64>,
) {
    let cause = state.current_or_begin_rules_execution_node();
    state
        .resolved_rules_journal
        .record_attachment(ResolvedAttachmentCommand {
            attachment,
            expected_old_host,
            resulting_host,
            resulting_timestamp,
            cause,
        })
        .expect("resolved attachment must have a live journal cause");
}

/// Why a host forbids an attachment, independent of the Enchant filter and zone.
/// This is the single authority for protection ("E" of DEBT) plus
/// can't-be-attached legality, shared by attach gates and SBA re-checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AttachIllegality {
    /// CR 702.16c: A protected permanent or player can't be enchanted by an
    /// Aura of the protected quality.
    /// CR 702.16d: A protected permanent can't be equipped or fortified by an
    /// attachment of the protected quality.
    Protection,
    /// CR 303.4c: Other applicable effects can make an Aura's host illegal.
    /// CR 701.3a: An attachment can't be attached to something it couldn't
    /// enchant, equip, or fortify.
    Prohibited,
}

/// Returns `Some(reason)` when an object host forbids `attachment` via
/// protection or a can't-be-attached static, else `None`.
///
/// Does NOT evaluate the Enchant filter or zone — that legality is applied by
/// the caller (`is_valid_attachment_target` checks it for the SBA path). The
/// checks here are purely additive prohibitions that apply equally at attach
/// time and continuously thereafter, so routing both paths through this resolver
/// keeps them from drifting.
pub(crate) fn attachment_illegality(
    state: &GameState,
    attachment_id: ObjectId,
    host_id: ObjectId,
) -> Option<AttachIllegality> {
    attachment_illegality_projected(
        state,
        attachment_id,
        state.objects.get(&attachment_id),
        host_id,
    )
}

/// CR 614.12 + CR 701.3a: [`attachment_illegality`] against an explicitly
/// supplied projection of the ATTACHMENT.
///
/// Every attachment-side half of this resolver — is the attacher an Aura or an
/// Equipment (CR 303.4 / CR 301.5), its own `AttachmentRestriction` statics
/// (CR 301.5b / CR 303.4j), and the CR 702.16c/d protection quality match — reads
/// the attachment's characteristics. For an entrant that is not yet the object
/// stored under its id, `state.objects` holds the WRONG characteristics: a meld
/// entrant's id still holds the exiled front-face component card, which has a
/// different typeline, colors and controller than the permanent that is entering.
/// CR 614.12 requires "the characteristics of the permanent as it would exist on
/// the battlefield", so the pre-entry CR 303.4f/g host consult passes the
/// entrant projection here and this resolver reads it instead.
///
/// `attachment` is `None` only when no object and no projection exists under the
/// id, in which case the attachment-side halves are skipped exactly as before.
///
/// `attachment_id` is still used for the two id-keyed structural reads that are
/// not characteristic lookups: the CR 301.5c self-attach guard and the CR 701.3b
/// attachment-graph cycle guard.
pub(crate) fn attachment_illegality_projected(
    state: &GameState,
    attachment_id: ObjectId,
    attachment: Option<&crate::game::game_object::GameObject>,
    host_id: ObjectId,
) -> Option<AttachIllegality> {
    // CR 301.5c: "An Equipment can't equip itself." (And no permanent can be
    // attached to itself.) Single-authority self-attach guard protecting both
    // `can_attach_to_object` and `attach_to`.
    if attachment_id == host_id {
        return Some(AttachIllegality::Prohibited);
    }
    // CR 701.3b + CR 301.5c + CR 303.4d: an illegal attachment attempt does
    // nothing. A host already attached, directly or indirectly, to the proposed
    // attachment would make the attachment graph cyclic, the same invalid shape
    // as self-attach.
    if attachment_tree_contains(state, attachment_id, host_id) {
        return Some(AttachIllegality::Prohibited);
    }
    // CR 701.3a: `CantBeAttached` blocks any attachment from being attached to
    // the host.
    if crate::game::static_abilities::object_has_static_other(state, host_id, "CantBeAttached") {
        return Some(AttachIllegality::Prohibited);
    }
    let (attacher_is_aura, attacher_is_equipment) = attachment.map_or((false, false), |obj| {
        (
            obj.card_types.subtypes.iter().any(|s| s == "Aura"),
            obj.card_types.subtypes.iter().any(|s| s == "Equipment"),
        )
    });
    // CR 303.4c: Other applicable effects can make an Aura's host illegal.
    if attacher_is_aura
        && crate::game::static_abilities::object_has_static_other(state, host_id, "CantBeEnchanted")
    {
        return Some(AttachIllegality::Prohibited);
    }
    // CR 701.3a: Equipment can't be attached to an object it can't equip.
    if attacher_is_equipment
        && crate::game::static_abilities::object_has_static_other(state, host_id, "CantBeEquipped")
    {
        return Some(AttachIllegality::Prohibited);
    }

    // CR 301.5 + CR 303.4 + CR 701.3a: Positive attachment restriction. An
    // Aura/Equipment that "can be attached only to {filter}" may only attach to a
    // host matching that filter. Unlike the `CantBe*` host prohibitions above
    // (read from the HOST's statics), this restriction is carried by the
    // ATTACHMENT itself, so a candidate host failing the filter makes the attach
    // illegal (CR 301.5b / CR 303.4j: the attachment doesn't move).
    if !attachment_satisfies_restrictions(state, attachment_id, attachment, host_id) {
        return Some(AttachIllegality::Prohibited);
    }

    // CR 702.16c: Protection from a quality prevents Auras of that quality from
    // being attached to the protected permanent.
    // CR 702.16d: Protection from a quality prevents Equipment or Fortifications
    // of that quality from being attached to the protected permanent.
    // CR 702.16n / CR 702.16p: A protection grant that says "this effect doesn't
    // remove …" does not make matching attachments illegal via *that* instance
    // (Flickering Ward / Ward cycle / Benevolent Blessing). Other instances of
    // protection from the same quality still apply normally.
    if let (Some(host), Some(attachment)) = (state.objects.get(&host_id), attachment) {
        if protection_blocks_attachment(state, host_id, attachment_id, host, attachment) {
            return Some(AttachIllegality::Protection);
        }
    }

    None
}

/// CR 702.16c/d + CR 702.16n/p: True when some protection instance on `host`
/// matches `attachment` and is not exempted for that attachment.
fn protection_blocks_attachment(
    state: &GameState,
    host_id: ObjectId,
    attachment_id: ObjectId,
    host: &crate::game::game_object::GameObject,
    attachment: &crate::game::game_object::GameObject,
) -> bool {
    use crate::types::ability::ContinuousModification;
    use crate::types::keywords::Keyword;
    use crate::types::statics::StaticMode;

    // CR 702.16: Printed / base protection on the host has no 702.16n rider —
    // it always blocks matching attachments.
    for kw in &host.base_keywords {
        if let Keyword::Protection(ref pt) = kw {
            if crate::game::keywords::source_matches_protection_target(pt, host, attachment) {
                return true;
            }
        }
    }

    // Continuous grants: each matching protection instance blocks unless its
    // StaticDefinition/TCE carries a CR 702.16n/p exemption covering this
    // attachment.
    let mut any_matching_grant = false;
    for (source_obj, def) in crate::game::functioning_abilities::battlefield_active_statics(state) {
        if !matches!(def.mode, StaticMode::Continuous) {
            continue;
        }
        let source_id = source_obj.id;
        let def_index = live_static_def_index(source_obj, def);
        let affected = def.affected.clone().unwrap_or(TargetFilter::Any);
        let ctx = FilterContext::from_source(state, source_id);
        if !matches_target_filter(state, host_id, &affected, &ctx) {
            continue;
        }
        for (mod_index, modification) in def.modifications.iter().enumerate() {
            let ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(pt),
            } = modification
            else {
                continue;
            };
            let resolved = resolve_protection_target_for_grant(state, source_id, pt);
            let Some(resolved) = resolved else {
                continue;
            };
            if !crate::game::keywords::source_matches_protection_target(&resolved, host, attachment)
            {
                continue;
            }
            any_matching_grant = true;
            if !protection_grant_exempts_attachment(
                state,
                attachment_id,
                source_id,
                (def_index, mod_index, host_id),
                &resolved,
                def.protection_does_not_remove.as_ref(),
            ) {
                return true;
            }
        }
    }

    // Transient continuous protection grants (e.g. Mother of Runes) — no
    // StaticDefinition rider today; treat as always-blocking when they match.
    for tce in &state.transient_continuous_effects {
        let ctx = FilterContext::from_source(state, tce.source_id);
        if !matches_target_filter(state, host_id, &tce.affected, &ctx) {
            continue;
        }
        for modification in &tce.modifications {
            let ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(pt),
            } = modification
            else {
                continue;
            };
            let resolved = resolve_protection_target_for_grant(state, tce.source_id, pt);
            let Some(resolved) = resolved else {
                continue;
            };
            if crate::game::keywords::source_matches_protection_target(&resolved, host, attachment)
            {
                // Transients currently carry no 702.16n rider field.
                return true;
            }
        }
    }

    // If host.keywords still match (granted protection present) but we found no
    // continuous grant — fall back to the pre-exemption query so we never open
    // a hole when grant discovery misses a path.
    if !any_matching_grant && crate::game::keywords::protection_prevents_from(host, attachment) {
        return true;
    }

    false
}

/// CR 702.16 + CR 105.4: Resolve `ChosenColor` / `ChosenCardType` against the
/// granting source before matching the attachment (mirrors layer bake-in).
fn resolve_protection_target_for_grant(
    state: &GameState,
    source_id: ObjectId,
    pt: &crate::types::keywords::ProtectionTarget,
) -> Option<crate::types::keywords::ProtectionTarget> {
    use crate::types::keywords::ProtectionTarget;
    match pt {
        ProtectionTarget::ChosenColor => state
            .objects
            .get(&source_id)
            .and_then(|src| src.chosen_color())
            .map(ProtectionTarget::Color),
        ProtectionTarget::ChosenCardType => state
            .objects
            .get(&source_id)
            .and_then(|src| src.chosen_card_type())
            .and_then(|ct| ct.protection_quality_str())
            .map(|quality| ProtectionTarget::CardType(quality.to_string())),
        other => Some(other.clone()),
    }
}

/// Composite key for one protection modification on a host:
/// `(static_definitions index, modifications index, host object id)`.
use crate::game::game_object::ProtectionEffectHostKey;

/// Live `static_definitions` index for an active static returned by
/// `battlefield_active_statics`.
fn live_static_def_index(
    source: &crate::game::game_object::GameObject,
    def: &crate::types::ability::StaticDefinition,
) -> usize {
    source
        .static_definitions
        .iter_all()
        .position(|d| std::ptr::eq(d, def))
        .expect("active static definition must index live static_definitions")
}

/// CR 702.16p: Capture attachment IDs matching `resolved_pt` that are already
/// on `host_id` and controlled by `grant_controller` at protection-start time.
fn capture_protection_start_attachment_snapshot(
    state: &GameState,
    host_id: ObjectId,
    grant_controller: PlayerId,
    resolved_pt: &crate::types::keywords::ProtectionTarget,
) -> Vec<ObjectId> {
    let Some(host) = state.objects.get(&host_id) else {
        return Vec::new();
    };
    host.attachments
        .iter()
        .filter_map(|&attachment_id| {
            let attachment = state.objects.get(&attachment_id)?;
            let is_aura_or_equipment = attachment
                .card_types
                .subtypes
                .iter()
                .any(|s| s.eq_ignore_ascii_case("Aura") || s.eq_ignore_ascii_case("Equipment"));
            if !is_aura_or_equipment || attachment.controller != grant_controller {
                return None;
            }
            if !crate::game::keywords::source_matches_protection_target(
                resolved_pt,
                host,
                attachment,
            ) {
                return None;
            }
            Some(attachment_id)
        })
        .collect()
}

/// CR 702.16p: When a continuous protection grant with the already-attached
/// rider starts applying to a host, snapshot the matching controlled
/// attachments once; consult that per-grant map in
/// [`protection_grant_exempts_attachment`]. Prune entries when the grant stops
/// applying to a host or the source leaves the battlefield.
pub(crate) fn refresh_protection_start_attachment_snapshots(state: &mut GameState) {
    use crate::types::ability::ContinuousModification;
    use crate::types::ability::ProtectionDoesNotRemove;
    use crate::types::keywords::Keyword;
    use crate::types::statics::StaticMode;
    use std::collections::{HashMap, HashSet};

    struct ActiveGrant {
        def_index: usize,
        mod_index: usize,
        host_id: ObjectId,
        resolved_pt: crate::types::keywords::ProtectionTarget,
        controller: PlayerId,
    }

    let mut active_by_source: HashMap<ObjectId, Vec<ActiveGrant>> = HashMap::new();

    for (source_obj, def) in crate::game::functioning_abilities::battlefield_active_statics(state) {
        if !matches!(def.mode, StaticMode::Continuous) {
            continue;
        }
        if def.protection_does_not_remove
            != Some(ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached)
        {
            continue;
        }
        let source_id = source_obj.id;
        let def_index = live_static_def_index(source_obj, def);
        let affected = def.affected.clone().unwrap_or(TargetFilter::Any);
        let ctx = FilterContext::from_source(state, source_id);
        for (mod_index, modification) in def.modifications.iter().enumerate() {
            let ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(pt),
            } = modification
            else {
                continue;
            };
            let Some(resolved_pt) = resolve_protection_target_for_grant(state, source_id, pt)
            else {
                continue;
            };
            for &host_id in &state.battlefield {
                if !matches_target_filter(state, host_id, &affected, &ctx) {
                    continue;
                }
                active_by_source
                    .entry(source_id)
                    .or_default()
                    .push(ActiveGrant {
                        def_index,
                        mod_index,
                        host_id,
                        resolved_pt: resolved_pt.clone(),
                        controller: source_obj.controller,
                    });
            }
        }
    }

    let active_sources: HashSet<ObjectId> = active_by_source.keys().copied().collect();

    for &source_id in &state.battlefield {
        let Some(source) = state.objects.get(&source_id) else {
            continue;
        };
        if source.protection_start_exempt_attachments.is_empty() {
            continue;
        }
        let active_keys = active_by_source
            .get(&source_id)
            .map(|grants| {
                grants
                    .iter()
                    .map(|g| (g.def_index, g.mod_index, g.host_id))
                    .collect::<HashSet<ProtectionEffectHostKey>>()
            })
            .unwrap_or_default();
        if active_sources.contains(&source_id) {
            state
                .objects
                .get_mut(&source_id)
                .expect("battlefield object")
                .protection_start_exempt_attachments
                .retain(|key, _| active_keys.contains(key));
        } else {
            state
                .objects
                .get_mut(&source_id)
                .expect("battlefield object")
                .protection_start_exempt_attachments
                .clear();
        }
    }

    let mut to_capture = Vec::new();
    for (source_id, grants) in active_by_source {
        for grant in grants {
            let key = (grant.def_index, grant.mod_index, grant.host_id);
            let already_snapshotted = state.objects.get(&source_id).is_some_and(|source| {
                source
                    .protection_start_exempt_attachments
                    .get(&key)
                    .is_some_and(|entry| entry.resolved_quality == grant.resolved_pt)
            });
            if already_snapshotted {
                continue;
            }
            to_capture.push((source_id, key, grant.resolved_pt, grant.controller));
        }
    }

    for (source_id, key, resolved_pt, controller) in to_capture {
        let snapshot =
            capture_protection_start_attachment_snapshot(state, key.2, controller, &resolved_pt);
        state
            .objects
            .get_mut(&source_id)
            .expect("grant source must exist")
            .protection_start_exempt_attachments
            .insert(
                key,
                crate::game::game_object::ProtectionStartSnapshot {
                    resolved_quality: resolved_pt,
                    attachment_ids: snapshot,
                },
            );
    }
}

/// CR 702.16n / CR 702.16p: Does this protection grant's exemption rider cover
/// `attachment_id` on `host_id`?
fn protection_grant_exempts_attachment(
    state: &GameState,
    attachment_id: ObjectId,
    grant_source_id: ObjectId,
    grant_key: ProtectionEffectHostKey,
    resolved_pt: &crate::types::keywords::ProtectionTarget,
    exemption: Option<&crate::types::ability::ProtectionDoesNotRemove>,
) -> bool {
    use crate::types::ability::ProtectionDoesNotRemove;

    let (grant_def_index, grant_mod_index, host_id) = grant_key;

    let Some(exemption) = exemption else {
        return false;
    };
    let Some(attachment) = state.objects.get(&attachment_id) else {
        return false;
    };
    match exemption {
        // CR 702.16n: "this effect doesn't remove this Aura"
        ProtectionDoesNotRemove::Source => attachment_id == grant_source_id,
        // CR 702.16n: "this effect doesn't remove Auras"
        ProtectionDoesNotRemove::Auras => attachment
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Aura")),
        // CR 702.16p: only attachments snapshotted for this specific protection
        // modification (`def_index`, `mod_index`) and host when it started applying.
        ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached => state
            .objects
            .get(&grant_source_id)
            .and_then(|source| {
                source.protection_start_exempt_attachments.get(&(
                    grant_def_index,
                    grant_mod_index,
                    host_id,
                ))
            })
            .is_some_and(|entry| {
                entry.resolved_quality == *resolved_pt
                    && entry.attachment_ids.contains(&attachment_id)
            }),
    }
}

/// CR 301.5 + CR 303.4 + CR 701.3a: True unless `host_id` is forbidden by a
/// positive "can be attached only to {filter}" restriction on `attachment_id`.
///
/// The restriction is a `StaticMode::AttachmentRestriction { filter }` carried by
/// the attachment's own `static_definitions`. By analogy to CR 702.5c (an Aura
/// with multiple enchant instances must satisfy ALL of them), every active
/// restriction must match: the host is legal only if it matches the `filter` of
/// every `AttachmentRestriction` the attachment has. An attachment with no such
/// restriction is unconstrained here and returns `true`.
fn attachment_satisfies_restrictions(
    state: &GameState,
    attachment_id: ObjectId,
    attachment: Option<&crate::game::game_object::GameObject>,
    host_id: ObjectId,
) -> bool {
    let Some(attachment) = attachment else {
        return true;
    };
    // CR 614.12: source-relative predicates inside the restriction filter bind to
    // the ENTRANT's controller. Identical to `FilterContext::from_source` for an
    // attachment that is already the object stored under this id (that
    // constructor reads exactly `state.objects[id].controller`); it differs only
    // for a pre-entry projection, which is the case this parameter exists for.
    let ctx = FilterContext::from_source_with_controller(attachment_id, attachment.controller);
    crate::game::functioning_abilities::active_static_definitions(state, attachment).all(|def| {
        match &def.mode {
            crate::types::statics::StaticMode::AttachmentRestriction { filter } => {
                matches_target_filter(state, host_id, filter, &ctx)
            }
            // Any other static imposes no positive attachment constraint.
            _ => true,
        }
    })
}

/// Returns `Some(reason)` when a player host forbids `attachment` via
/// player-scoped protection, else `None`.
/// The attachment is supplied as a projection rather than looked up by id:
/// CR 614.12 requires an ENTRANT to be read as it will exist on the battlefield,
/// and its id may still hold the pre-entry object. Object-host sibling of
/// [`attachment_illegality_projected`], which documents the same reasoning.
pub(crate) fn player_attachment_illegality(
    state: &GameState,
    attachment: Option<&crate::game::game_object::GameObject>,
    host: PlayerId,
) -> Option<AttachIllegality> {
    // CR 702.16c: A player with protection can't be enchanted by an Aura of the
    // protected quality.
    if crate::game::static_abilities::player_protection_from_object(state, host, attachment) {
        return Some(AttachIllegality::Protection);
    }
    None
}

pub(crate) fn can_attach_to_object(
    state: &GameState,
    attachment_id: ObjectId,
    target_id: ObjectId,
) -> bool {
    can_attach_to_object_projected(
        state,
        attachment_id,
        state.objects.get(&attachment_id),
        target_id,
    )
}

/// CR 614.12: [`can_attach_to_object`] against an explicitly supplied projection
/// of the attachment. See [`attachment_illegality_projected`].
pub(crate) fn can_attach_to_object_projected(
    state: &GameState,
    attachment_id: ObjectId,
    attachment: Option<&crate::game::game_object::GameObject>,
    target_id: ObjectId,
) -> bool {
    // CR 701.3a: A blocked attachment is not a legal host for an attach effect.
    attachment_illegality_projected(state, attachment_id, attachment, target_id).is_none()
}

pub(crate) fn can_attach_to_player(
    state: &GameState,
    attachment_id: ObjectId,
    target_player: PlayerId,
) -> bool {
    can_attach_to_player_projected(state, state.objects.get(&attachment_id), target_player)
}

/// CR 614.12: [`can_attach_to_player`] against an explicitly supplied projection
/// of the attachment. See [`attachment_illegality_projected`].
pub(crate) fn can_attach_to_player_projected(
    state: &GameState,
    attachment: Option<&crate::game::game_object::GameObject>,
    target_player: PlayerId,
) -> bool {
    // CR 303.4c: A player who has left the game is an illegal Aura host.
    if !state
        .players
        .get(target_player.0 as usize)
        .is_some_and(|p| !p.is_eliminated)
    {
        return false;
    }
    // CR 702.16c: Protection from a quality prevents Auras of that quality from
    // being attached to the protected player.
    player_attachment_illegality(state, attachment, target_player).is_none()
}

/// CR 303.4: Attach an Aura to a player (Curse cycle, Faith's Fetters-class).
/// Mirrors `attach_to`'s "detach from previous host" cleanup for Object hosts,
/// but no host-side `attachments` list is touched (a player is not a
/// `GameObject` and has no such field).
///
/// CR 303.4i: An Aura can't enter attached to a player it can't legally
/// enchant.
/// CR 301.5: Equipment can't legally be attached to a player.
/// Mirroring `attach_to`'s silent-no-op gating pattern, an illegal
/// Aura/Equipment pairing here is a no-op rather than an error: a caller that
/// has already validated the source sees no change in state, and a buggy caller
/// that hasn't validated cannot drive the engine into an illegal state.
pub fn attach_to_player(
    state: &mut GameState,
    attachment_id: ObjectId,
    target_player: PlayerId,
) -> Option<TargetRef> {
    attach_to_player_with_authority(
        state,
        attachment_id,
        target_player,
        AttachmentAuthority::Stored,
    )
}

/// CR 614.12 + CR 303.4i: [`attach_to_player`] whose CR 701.3b legality gate
/// reads the supplied [`AttachmentAuthority`] instead of the stored object.
/// Player-host sibling of [`attach_to_with_authority`]; the same "gate is
/// projected, edit is not" split applies.
pub(crate) fn attach_to_player_with_authority(
    state: &mut GameState,
    attachment_id: ObjectId,
    target_player: PlayerId,
    authority: AttachmentAuthority<'_>,
) -> Option<TargetRef> {
    // CR 301.5: Equipment or Fortification cannot attach to a player.
    // CR 303.4: Only Auras may have a player host. Any non-Aura attachment is
    // silently rejected here so the only paths into a `Player` `attached_to`
    // value are legitimate Aura attachments. The Equipment/Fortification check
    // is redundant given the Aura whitelist but is named explicitly so future
    // attachment subtypes cannot slip through by
    // accident — the contract is "Auras only", not "anything that isn't
    // currently equipment".
    if !authority_is_aura(state, attachment_id, authority) {
        return None;
    }
    if !can_attach_to_player_with_authority(state, attachment_id, target_player, authority) {
        return None;
    }

    // CR 613.7e + CR 701.3b/c: read the UNFILTERED prior host once so a first-attach
    // (None) and a same-player re-attach (Some(player)) are distinguishable —
    // `old_target` collapses both to `None`. The filtered view and the CR 733
    // command's `expected_old_host` both derive from this single read rather than
    // querying the attachment's host again.
    let attachment_object = state.objects.get(&attachment_id);
    let reference = attachment_object.map(ObjectIncarnationRef::from_object);
    let expected_old_host = attachment_object.and_then(|obj| obj.attached_to);
    let prior_host = expected_old_host.map(target_ref_from_attach_target);
    // Discriminate the timestamp bump (below) before `filter` consumes `prior_host`:
    // `==` borrows, `Option::filter` moves. True on first-attach (None) or a move to a
    // different player; false only on a same-player re-attach (CR 701.3b keeps the stamp).
    let moving_to_new_host = prior_host != Some(TargetRef::Player(target_player));
    let old_target = prior_host.filter(|target| *target != TargetRef::Player(target_player));

    // CR 701.3a: If already attached to an object, detach from that object's
    // `attachments` list. Re-attaching to a player has no symmetric cleanup —
    // the previous Player host has no list to clear.
    if let Some(AttachTarget::Object(old_target_id)) = state
        .objects
        .get(&attachment_id)
        .and_then(|obj| obj.attached_to)
    {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }

    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = Some(AttachTarget::Player(target_player));
    }

    // CR 613.7e + CR 701.3c: first attach (None) or a move to a different player
    // host bumps the timestamp; a same-player re-attach (CR 701.3b) does not.
    let resulting_timestamp = moving_to_new_host.then(|| {
        let ts = state.next_timestamp();
        if let Some(attachment) = state.objects.get_mut(&attachment_id) {
            attachment.timestamp = ts;
        }
        ts
    });

    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);

    // CR 733: journal the settled edit through its owning family, on the same
    // terms as the object-host authority — a same-player re-attach (CR 701.3b)
    // changed nothing and is not recorded.
    if let Some(reference) = reference.filter(|_| moving_to_new_host) {
        record_attachment_edit(
            state,
            reference,
            expected_old_host,
            Some(AttachTarget::Player(target_player)),
            resulting_timestamp,
        );
    }
    old_target
}

/// CR 701.3d: Move an attachment away from the object or player it was attached
/// to while it remains on the battlefield. This is the single graph update
/// primitive for explicit unattach costs and effects.
pub(crate) fn unattach(state: &mut GameState, attachment_id: ObjectId) -> Option<TargetRef> {
    // One read serves the returned host, the old host's list cleanup, and the
    // CR 733 command's `expected_old_host`. The `?` means an already-unattached
    // attachment returns before any mutation, so every reachable path below is a
    // real host transition.
    let attachment_object = state.objects.get(&attachment_id)?;
    let reference = ObjectIncarnationRef::from_object(attachment_object);
    let expected_old_host = attachment_object.attached_to?;
    let old_target = target_ref_from_attach_target(expected_old_host);

    if let Some(old_target_id) = expected_old_host.as_object() {
        if let Some(old_target) = state.objects.get_mut(&old_target_id) {
            old_target.attachments.retain(|&id| id != attachment_id);
        }
    }
    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = None;
    }
    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);

    // CR 733 + CR 701.3d: an unattach installs no host and, unlike an attach,
    // draws no new timestamp (CR 613.7e applies to attaching to a new host).
    record_attachment_edit(state, reference, Some(expected_old_host), None, None);
    Some(old_target)
}

/// Installs one already-resolved CR 701.3 attachment edit verbatim.
///
/// Deliberately re-runs NONE of the CR 301.5 / CR 303.4 / CR 702.16 attach
/// legality guards: those decided at resolve time whether this edit happened at
/// all, and re-evaluating them against a replayed board could reject an edit the
/// game already made. The applier only verifies that the state it is installing
/// into is the state the command was resolved from, then performs the structural
/// graph move and installs the recorded timestamp.
pub fn apply_resolved_attachment(
    state: &mut GameState,
    command: &ResolvedAttachmentCommand,
) -> Result<(), ResolvedAttachmentReplayInvariantError> {
    let attachment_id = command.attachment.object_id;
    let attachment = state.objects.get(&attachment_id).ok_or(
        ResolvedAttachmentReplayInvariantError::UnknownAttachment(attachment_id),
    )?;
    let found = ObjectIncarnationRef::from_object(attachment);
    if found != command.attachment {
        return Err(ResolvedAttachmentReplayInvariantError::StaleAttachment {
            expected: command.attachment,
            found,
        });
    }
    if attachment.attached_to != command.expected_old_host {
        return Err(
            ResolvedAttachmentReplayInvariantError::HostPreconditionMismatch {
                expected: command.expected_old_host,
                found: attachment.attached_to,
            },
        );
    }
    // CR 303.4f: the recorded host must still be an object the graph can point
    // at, checked before any mutation so a rejected command leaves no partial edit.
    if let Some(new_host_id) = command.resulting_host.and_then(|host| host.as_object()) {
        if !state.objects.contains_key(&new_host_id) {
            return Err(ResolvedAttachmentReplayInvariantError::UnknownHost(
                new_host_id,
            ));
        }
    }

    if let Some(old_host_id) = command.expected_old_host.and_then(|host| host.as_object()) {
        if let Some(old_host) = state.objects.get_mut(&old_host_id) {
            old_host.attachments.retain(|&id| id != attachment_id);
        }
    }
    if let Some(attachment) = state.objects.get_mut(&attachment_id) {
        attachment.attached_to = command.resulting_host;
        // CR 613.7e + CR 701.3c: install the timestamp the authority drew rather
        // than re-drawing one, which would reorder the layer system.
        if let Some(timestamp) = command.resulting_timestamp {
            attachment.timestamp = timestamp;
        }
    }
    // CR 613.7e: an unattach and a same-host re-attach draw no timestamp, so
    // only a recorded draw advances the allocator past the value it installed.
    if let Some(timestamp) = command.resulting_timestamp {
        state.adopt_replayed_timestamp(timestamp);
    }
    if let Some(new_host_id) = command.resulting_host.and_then(|host| host.as_object()) {
        if let Some(new_host) = state.objects.get_mut(&new_host_id) {
            if !new_host.attachments.contains(&attachment_id) {
                new_host.attachments.push(attachment_id);
            }
        }
    }

    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AttachmentKind, ControllerRef, FilterProp, StaticDefinition, TargetFilter, TargetRef,
        TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::{AttachmentSnapshot, ZoneChangeRecord};
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
        GameState::new_two_player(42)
    }

    /// Build Equipment on the battlefield (Artifact + Equipment subtype).
    fn spawn_equipment(state: &mut GameState, name: &str, card_id: u64) -> ObjectId {
        let id = create_object(
            state,
            CardId(card_id),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        id
    }

    /// Build a permanent with the given subtype on the battlefield.
    fn spawn_with_subtype(state: &mut GameState, name: &str, subtype: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.subtypes.push(subtype.to_string());
        id
    }

    fn spawn_creature(state: &mut GameState, name: &str) -> ObjectId {
        spawn_creature_for(state, name, PlayerId(0))
    }

    fn spawn_creature_for(state: &mut GameState, name: &str, owner: PlayerId) -> ObjectId {
        let id = create_object(state, CardId(2), owner, name.to_string(), Zone::Battlefield);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        id
    }

    fn apply_static(state: &mut GameState, id: ObjectId, mode_name: &str) {
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::Other(mode_name.to_string()))
                .affected(TargetFilter::SelfRef),
        );
    }

    #[test]
    fn attachment_illegality_protection_blocks_aura() {
        // CR 702.16c: a host with protection from white forbids a white Aura.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Pacifism", "Aura");
        {
            let obj = state.objects.get_mut(&aura).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.color.push(crate::types::mana::ManaColor::White);
        }
        let creature = spawn_creature(&mut state, "Bear");
        state.objects.get_mut(&creature).unwrap().keywords.push(
            crate::types::keywords::Keyword::Protection(
                crate::types::keywords::ProtectionTarget::Color(
                    crate::types::mana::ManaColor::White,
                ),
            ),
        );

        assert_eq!(
            attachment_illegality(&state, aura, creature),
            Some(AttachIllegality::Protection)
        );
        assert!(!can_attach_to_object(&state, aura, creature));
    }

    #[test]
    fn attachment_illegality_cant_be_enchanted_blocks_aura() {
        // CR 303.4c: other applicable effects can make an Aura's host illegal.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let creature = spawn_creature(&mut state, "Bear");
        apply_static(&mut state, creature, "CantBeEnchanted");

        assert_eq!(
            attachment_illegality(&state, aura, creature),
            Some(AttachIllegality::Prohibited)
        );
        assert!(!can_attach_to_object(&state, aura, creature));
    }

    #[test]
    fn player_attachment_illegality_protection_blocks_aura() {
        // CR 702.16c: a player with protection from everything can't be
        // enchanted by an Aura.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Curse", "Aura");
        state.add_transient_continuous_effect(
            aura,
            PlayerId(0),
            crate::types::ability::Duration::UntilEndOfTurn,
            TargetFilter::SpecificPlayer { id: PlayerId(1) },
            vec![crate::types::ability::ContinuousModification::AddKeyword {
                keyword: crate::types::keywords::Keyword::Protection(
                    crate::types::keywords::ProtectionTarget::Everything,
                ),
            }],
            None,
        );

        assert_eq!(
            player_attachment_illegality(&state, state.objects.get(&aura), PlayerId(1)),
            Some(AttachIllegality::Protection)
        );
        assert!(!can_attach_to_player(&state, aura, PlayerId(1)));
    }

    #[test]
    fn attachment_illegality_none_for_legal_host() {
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let creature = spawn_creature(&mut state, "Bear");

        assert_eq!(attachment_illegality(&state, aura, creature), None);
        assert!(can_attach_to_object(&state, aura, creature));
    }

    #[test]
    fn test_attach_sets_attached_to_and_attachments() {
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .subtypes
            .push("Equipment".to_string());

        let creature_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        attach_to(&mut state, equipment_id, creature_id);

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_id))
        );
        assert!(state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn multiple_distinct_equipment_can_attach_to_same_creature() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let creature = spawn_creature(&mut state, "Bear");

        attach_to(&mut state, sword, creature);
        attach_to(&mut state, shield, creature);

        assert_eq!(
            state.objects.get(&sword).unwrap().attached_to,
            Some(AttachTarget::Object(creature))
        );
        assert_eq!(
            state.objects.get(&shield).unwrap().attached_to,
            Some(AttachTarget::Object(creature))
        );
        assert_eq!(
            state.objects.get(&creature).unwrap().attachments,
            vec![sword, shield]
        );
    }

    /// E1: re-attaching Equipment to a DIFFERENT host issues a new timestamp on
    /// each attach — CR 613.7e (first attach) then CR 701.3c (move to a
    /// different host). Reverting Step 2 leaves the timestamp at 0 throughout,
    /// so both strict-increase asserts fail.
    #[test]
    fn reattach_to_different_host_bumps_timestamp() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let bear_a = spawn_creature(&mut state, "Bear A");
        let bear_b = spawn_creature(&mut state, "Bear B");

        let ts_initial = state.objects[&sword].timestamp;

        attach_to(&mut state, sword, bear_a);
        let ts_after_a = state.objects[&sword].timestamp;
        assert!(
            ts_after_a > ts_initial,
            "first attach must issue a new timestamp (CR 613.7e)"
        );

        attach_to(&mut state, sword, bear_b);
        let ts_after_b = state.objects[&sword].timestamp;
        assert!(
            ts_after_b > ts_after_a,
            "moving to a different host must issue a new timestamp (CR 701.3c)"
        );
    }

    /// E2: re-attaching to the SAME host issues no new timestamp — CR 701.3b
    /// (the effect does nothing). This row discriminates the LOW-1 fix: the gate
    /// reads the UNFILTERED prior host, so a same-host re-attach is recognized as
    /// a no-op. Reverting to the filtered `old_target` local collapses the
    /// same-host case to `None`, which compares unequal and would bump.
    #[test]
    fn reattach_to_same_host_does_not_bump_timestamp() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let bear = spawn_creature(&mut state, "Bear");

        attach_to(&mut state, sword, bear);
        let ts_after_first = state.objects[&sword].timestamp;

        attach_to(&mut state, sword, bear);
        let ts_after_second = state.objects[&sword].timestamp;
        assert_eq!(
            ts_after_first, ts_after_second,
            "re-attaching to the same host must not issue a new timestamp (CR 701.3b)"
        );
    }

    #[test]
    fn test_attach_re_equip_moves_equipment() {
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );

        let creature_a = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_a)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let creature_b = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Bear B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_b)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Attach to creature A
        attach_to(&mut state, equipment_id, creature_a);
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_a))
        );

        // Re-equip to creature B
        let old_target = attach_to(&mut state, equipment_id, creature_b);
        assert_eq!(old_target, Some(TargetRef::Object(creature_a)));

        // Should be attached to B now
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_b))
        );
        assert!(state
            .objects
            .get(&creature_b)
            .unwrap()
            .attachments
            .contains(&equipment_id));

        // Should no longer be on A's attachments
        assert!(!state
            .objects
            .get(&creature_a)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn reattach_to_same_creature_returns_no_unattach_target() {
        let mut state = setup();
        let equipment_id = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let creature_id = spawn_creature(&mut state, "Bear");

        assert_eq!(attach_to(&mut state, equipment_id, creature_id), None);
        assert_eq!(attach_to(&mut state, equipment_id, creature_id), None);
    }

    #[test]
    fn unattach_returns_previous_host() {
        let mut state = setup();
        let equipment_id = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let creature_id = spawn_creature(&mut state, "Bear");
        attach_to(&mut state, equipment_id, creature_id);

        let old_target = unattach(&mut state, equipment_id);

        assert_eq!(old_target, Some(TargetRef::Object(creature_id)));
        assert_eq!(state.objects.get(&equipment_id).unwrap().attached_to, None);
        assert!(!state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn unattach_all_removes_matching_equipment_from_target() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let aura = spawn_with_subtype(&mut state, "Pacifism", "Aura");
        let creature = spawn_creature(&mut state, "Bear");
        attach_to(&mut state, sword, creature);
        attach_to(&mut state, shield, creature);
        attach_to(&mut state, aura, creature);

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(creature)],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects.get(&sword).unwrap().attached_to, None);
        assert_eq!(state.objects.get(&shield).unwrap().attached_to, None);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(AttachTarget::Object(creature))
        );
        assert_eq!(
            state.objects.get(&creature).unwrap().attachments,
            vec![aura]
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::Unattached { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn unattach_all_parent_target_removes_equipment_from_each_parent_host() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let bear = spawn_creature(&mut state, "Bear");
        let elf = spawn_creature(&mut state, "Elf");
        attach_to(&mut state, sword, bear);
        attach_to(&mut state, shield, elf);

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(bear), TargetRef::Object(elf)],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects.get(&sword).unwrap().attached_to, None);
        assert_eq!(state.objects.get(&shield).unwrap().attached_to, None);
        assert!(state.objects.get(&bear).unwrap().attachments.is_empty());
        assert!(state.objects.get(&elf).unwrap().attachments.is_empty());
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::Unattached { .. }))
                .count(),
            2
        );
    }

    #[test]
    fn unattach_all_empty_target_set_resolves_noop() {
        let mut state = setup();
        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature()
                        .controller(ControllerRef::Opponent),
                ),
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert!(matches!(
            events.as_slice(),
            [GameEvent::EffectResolved {
                kind: EffectKind::UnattachAll,
                source_id: ObjectId(999),
                ..
            }]
        ));
    }

    #[test]
    fn unattach_all_filters_explicit_object_targets() {
        let mut state = setup();
        let sword = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let shield = spawn_with_subtype(&mut state, "Shield", "Equipment");
        let own_creature = spawn_creature(&mut state, "Bear");
        let opponent_creature = spawn_creature_for(&mut state, "Elf", PlayerId(1));
        attach_to(&mut state, sword, own_creature);
        attach_to(&mut state, shield, opponent_creature);

        let ability = crate::types::ability::ResolvedAbility::new(
            Effect::UnattachAll {
                attachment: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::default().subtype("Equipment".to_string()),
                ),
                target: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature().controller(ControllerRef::You),
                ),
            },
            vec![
                TargetRef::Object(own_creature),
                TargetRef::Object(opponent_creature),
            ],
            ObjectId(999),
            PlayerId(0),
        );
        let mut events = vec![];

        resolve_unattach_all(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects.get(&sword).unwrap().attached_to, None);
        assert_eq!(
            state.objects.get(&shield).unwrap().attached_to,
            Some(AttachTarget::Object(opponent_creature))
        );
        assert!(state
            .objects
            .get(&opponent_creature)
            .unwrap()
            .attachments
            .contains(&shield));
    }

    #[test]
    fn cant_be_attached_blocks_any_attachment() {
        // CR 701.3: "Can't be attached" blocks any attachment (Aura/Equipment).
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let victim = spawn_creature(&mut state, "Victim");
        apply_static(&mut state, victim, "CantBeAttached");

        attach_to(&mut state, aura, victim);

        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);
        assert!(state.objects.get(&victim).unwrap().attachments.is_empty());
    }

    #[test]
    fn cant_be_enchanted_blocks_aura() {
        // CR 702.5: "Can't be enchanted" blocks Auras specifically.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Pacifism", "Aura");
        let victim = spawn_creature(&mut state, "Kira");
        apply_static(&mut state, victim, "CantBeEnchanted");

        attach_to(&mut state, aura, victim);

        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);
    }

    #[test]
    fn cant_be_equipped_blocks_equipment() {
        // CR 702.6: "Can't be equipped" blocks Equipment specifically.
        let mut state = setup();
        let equipment = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let victim = spawn_creature(&mut state, "Skittering Surveyor");
        apply_static(&mut state, victim, "CantBeEquipped");

        attach_to(&mut state, equipment, victim);

        assert_eq!(state.objects.get(&equipment).unwrap().attached_to, None);
    }

    #[test]
    fn deferred_attach_prompts_when_multiple_equipment_match() {
        use crate::types::ability::TypeFilter;
        use crate::types::game_state::WaitingFor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Kor Soldier");
        state.last_created_token_ids = vec![host];
        let first = spawn_equipment(&mut state, "Rod A", 10);
        let second = spawn_equipment(&mut state, "Rod B", 11);

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastCreated,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        let WaitingFor::EffectZoneChoice {
            cards, effect_kind, ..
        } = &state.waiting_for
        else {
            panic!(
                "expected EffectZoneChoice for multiple Equipment, got {:?}",
                state.waiting_for
            );
        };
        assert_eq!(*effect_kind, EffectKind::Attach);
        assert_eq!(cards.len(), 2);
        assert!(cards.contains(&first));
        assert!(cards.contains(&second));
        assert!(state.active_ability_continuation().is_some());

        let frame = state
            .take_active_ability_continuation()
            .expect("attachment fixture cannot consume a buried continuation")
            .unwrap();
        complete_resolution_attachment_choice(
            &mut state,
            *frame.pending.chain,
            &[second],
            &mut events,
        )
        .unwrap();

        assert_eq!(
            state.objects.get(&second).unwrap().attached_to,
            Some(AttachTarget::Object(host))
        );
        assert!(state.objects.get(&first).unwrap().attached_to.is_none());
    }

    #[test]
    fn optional_multi_attach_prompts_with_zero_minimum() {
        use crate::types::ability::TypeFilter;
        use crate::types::game_state::WaitingFor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Mockingbird");
        state.last_created_token_ids = vec![host];
        let first = spawn_equipment(&mut state, "Spy Kit", 10);
        let second = spawn_equipment(&mut state, "Energy Daggers", 11);

        let mut ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastCreated,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::unlimited(0));

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                cards,
                count,
                min_count,
                up_to,
                effect_kind,
                ..
            } => {
                assert_eq!(*effect_kind, EffectKind::Attach);
                assert_eq!(*count, 2);
                assert_eq!(*min_count, 0);
                assert!(*up_to);
                assert!(cards.contains(&first));
                assert!(cards.contains(&second));
            }
            other => panic!("expected optional Attach EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn complete_resolution_attachment_choice_attaches_multiple_equipment() {
        let mut state = setup();
        let host = spawn_creature(&mut state, "Mockingbird");
        state.last_created_token_ids = vec![host];
        let first = spawn_equipment(&mut state, "Spy Kit", 10);
        let second = spawn_equipment(&mut state, "Energy Daggers", 11);

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastCreated,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        complete_resolution_attachment_choice(&mut state, ability, &[first, second], &mut events)
            .unwrap();

        assert_eq!(
            state.objects.get(&first).unwrap().attached_to,
            Some(AttachTarget::Object(host))
        );
        assert_eq!(
            state.objects.get(&second).unwrap().attached_to,
            Some(AttachTarget::Object(host))
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Attach,
                        ..
                    }
                ))
                .count(),
            2,
            "each selected Equipment must resolve its own attachment"
        );
    }

    #[test]
    fn complete_resolution_attachment_choice_attaches_to_source_host() {
        let mut state = setup();
        let cloud = spawn_creature(&mut state, "Cloud, Ex-SOLDIER");
        let equipment = spawn_equipment(&mut state, "Buster Sword", 12);

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::SelfRef,
            },
            vec![],
            cloud,
            PlayerId(0),
        );

        let mut events = vec![];
        complete_resolution_attachment_choice(&mut state, ability, &[equipment], &mut events)
            .unwrap();

        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(cloud))
        );
        assert!(state
            .objects
            .get(&cloud)
            .unwrap()
            .attachments
            .contains(&equipment));
    }

    #[test]
    fn cloud_etb_attach_selection_equips_selected_equipment_to_cloud() {
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;

        let mut state = setup();
        let cloud = spawn_creature(&mut state, "Cloud, Ex-SOLDIER");
        let other_host = spawn_creature(&mut state, "Other Soldier");
        let first_equipment = spawn_equipment(&mut state, "Iron Sword", 12);
        let selected_equipment = spawn_equipment(&mut state, "Buster Sword", 13);

        let trigger = crate::parser::oracle_trigger::parse_trigger_line(
            "When ~ enters, attach up to one target Equipment you control to it.",
            "Cloud, Ex-SOLDIER",
        );
        let execute = trigger.execute.as_deref().expect("execute must be Some");
        let mut ability = crate::types::ability::ResolvedAbility::new(
            (*execute.effect).clone(),
            vec![],
            cloud,
            PlayerId(0),
        );
        ability.multi_target = execute.multi_target.clone();

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                cards,
                effect_kind,
                min_count,
                count,
                up_to,
                ..
            } => {
                assert_eq!(*effect_kind, EffectKind::Attach);
                assert_eq!(*min_count, 0);
                assert_eq!(*count, 1);
                assert!(*up_to);
                assert!(cards.contains(&first_equipment));
                assert!(cards.contains(&selected_equipment));
            }
            other => panic!("expected Cloud attach EffectZoneChoice, got {other:?}"),
        }

        crate::game::engine::apply_as_current(
            &mut state,
            GameAction::SelectCards {
                cards: vec![selected_equipment],
            },
        )
        .unwrap();

        assert_eq!(
            state.objects.get(&selected_equipment).unwrap().attached_to,
            Some(AttachTarget::Object(cloud))
        );
        assert!(state
            .objects
            .get(&cloud)
            .unwrap()
            .attachments
            .contains(&selected_equipment));
        assert!(state
            .objects
            .get(&first_equipment)
            .unwrap()
            .attached_to
            .is_none());
        assert!(state
            .objects
            .get(&other_host)
            .unwrap()
            .attachments
            .is_empty());
    }

    #[test]
    fn optional_attach_with_no_eligible_equipment_is_noop_without_attach_event() {
        use crate::types::ability::TypeFilter;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Mockingbird");
        state.last_created_token_ids = vec![host];
        let mut ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastCreated,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );
        ability.multi_target = Some(MultiTargetSpec::unlimited(0));

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            events.iter().all(|event| !matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Attach,
                    ..
                }
            )),
            "no-op optional Attach must not emit an attachment event"
        );
        assert!(state.objects.get(&host).unwrap().attachments.is_empty());
    }

    /// Regression: Nahiri, the Lithomancer +2 ("Create a Kor Soldier token. You
    /// may attach an Equipment you control to it") when the controller has NO
    /// Equipment. The "may" gate is accepted upstream, clearing `optional` and
    /// leaving no `multi_target`, so `attachment_choice_bounds` defaults to
    /// {min:1, max:1}. With zero eligible Equipment the pre-fix match catch-all
    /// built an `EffectZoneChoice { cards: [], min_count: 1 }` — an unsatisfiable
    /// prompt with no legal actions that froze the game (turn-26 stuck report).
    /// CR 608.2d: with no eligible Equipment the choice can't be made, so the
    /// effect does nothing.
    #[test]
    fn mandatory_attach_with_no_eligible_equipment_does_not_deadlock() {
        use crate::types::ability::TypeFilter;
        use crate::types::game_state::WaitingFor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Kor Soldier");
        state.last_created_token_ids = vec![host];

        // Post-"may"-gate Nahiri shape: optional flag already consumed, no
        // multi_target, so bounds resolve to {min:1, max:1}. No Equipment exists.
        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastCreated,
            },
            vec![],
            ObjectId(999),
            PlayerId(2),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
            "empty-eligible attach must not build an unsatisfiable choice, got {:?}",
            state.waiting_for
        );
        assert!(
            state.active_ability_continuation().is_none(),
            "no continuation should be stashed for a no-op attach"
        );
        assert!(state.objects.get(&host).unwrap().attachments.is_empty());
    }

    #[test]
    fn complete_resolution_attachment_choice_uses_nahiri_subtype_filter() {
        let mut state = setup();
        let host = spawn_creature(&mut state, "Kor Soldier");
        state.last_created_token_ids = vec![host];
        let equipment = spawn_equipment(&mut state, "Skullclamp", 12);

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastCreated,
            },
            vec![],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        complete_resolution_attachment_choice(&mut state, ability, &[equipment], &mut events)
            .unwrap();

        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(host))
        );
    }

    #[test]
    fn resolution_attachment_choice_excludes_equipment_from_dynamic_host_ledgers() {
        let mut state = setup();
        let created_host = spawn_creature(&mut state, "Created Kor");
        let revealed_host = spawn_creature(&mut state, "Revealed Kor");
        let changed_host = spawn_creature(&mut state, "Changed Kor");
        state.last_created_token_ids = vec![created_host];
        state.last_revealed_ids = vec![revealed_host];
        let first_equipment = spawn_equipment(&mut state, "First Blade", 10);
        let second_equipment = spawn_equipment(&mut state, "Second Blade", 11);
        let revealed_equipment = spawn_equipment(&mut state, "Revealed Blade", 12);
        let changed_equipment = spawn_equipment(&mut state, "Changed Blade", 13);
        state.last_zone_changed_ids = vec![changed_equipment, changed_host];

        let attachment_filter = TargetFilter::Typed(
            TypedFilter::default()
                .subtype("Equipment".to_string())
                .controller(ControllerRef::You),
        );
        let ability_for = |target| {
            crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::Attach {
                    attachment: attachment_filter.clone(),
                    target,
                },
                vec![],
                ObjectId(999),
                PlayerId(0),
            )
        };
        let mut events = vec![];

        complete_resolution_attachment_choice(
            &mut state,
            ability_for(TargetFilter::LastCreated),
            &[first_equipment, second_equipment],
            &mut events,
        )
        .expect("multiple selected Equipment attach to LastCreated host");
        complete_resolution_attachment_choice(
            &mut state,
            ability_for(TargetFilter::LastRevealed),
            &[revealed_equipment],
            &mut events,
        )
        .expect("selected Equipment attaches to LastRevealed host");
        complete_resolution_attachment_choice(
            &mut state,
            ability_for(TargetFilter::LastZoneChanged),
            &[changed_equipment],
            &mut events,
        )
        .expect("selected Equipment attaches to LastZoneChanged host");

        for equipment in [first_equipment, second_equipment] {
            assert_eq!(
                state.objects[&equipment].attached_to,
                Some(AttachTarget::Object(created_host)),
                "a selected Equipment cannot displace the LastCreated host"
            );
        }
        assert_eq!(
            state.objects[&revealed_equipment].attached_to,
            Some(AttachTarget::Object(revealed_host))
        );
        assert_eq!(
            state.objects[&changed_equipment].attached_to,
            Some(AttachTarget::Object(changed_host))
        );
    }

    #[test]
    fn complete_resolution_attachment_choice_skips_stale_parent_propagated_targets() {
        let mut state = setup();
        let host = spawn_creature(&mut state, "Kor Soldier");
        state.last_created_token_ids = vec![host];
        let first = spawn_equipment(&mut state, "Rod A", 10);
        let chosen = spawn_equipment(&mut state, "Rod B", 11);

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastCreated,
            },
            // Parent chain walkers can propagate the LastCreated bearer into
            // `targets` before the Equipment pick is appended at resolution.
            vec![crate::types::ability::TargetRef::Object(host)],
            ObjectId(999),
            PlayerId(0),
        );

        let mut events = vec![];
        complete_resolution_attachment_choice(&mut state, ability, &[chosen], &mut events).unwrap();

        assert_eq!(
            state.objects.get(&chosen).unwrap().attached_to,
            Some(AttachTarget::Object(host))
        );
        assert!(state.objects.get(&first).unwrap().attached_to.is_none());
    }

    #[test]
    fn attachment_role_binding_rejects_a_reincarnated_selected_equipment() {
        let mut state = setup();
        let equipment = spawn_equipment(&mut state, "Rod", 10);
        let filter = TargetFilter::Typed(
            TypedFilter::new(TypeFilter::Artifact)
                .subtype("Equipment".to_string())
                .controller(ControllerRef::You),
        );
        let mut ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: filter.clone(),
                target: TargetFilter::LastCreated,
            },
            Vec::new(),
            ObjectId(999),
            PlayerId(0),
        );
        ability.bind_attach_attachment_target(ObjectIncarnationRef::from_object(
            state
                .objects
                .get(&equipment)
                .expect("selected Equipment exists"),
        ));
        assert_eq!(
            resolve_bound_attachment_target(&state, &ability, &filter),
            Some(equipment),
            "the bound role pin must resolve before the incarnation changes"
        );
        state
            .objects
            .get_mut(&equipment)
            .expect("selected Equipment remains addressable")
            .incarnation += 1;

        assert_eq!(
            resolve_bound_attachment_target(&state, &ability, &filter),
            None,
            "a selected role pin cannot attach a later incarnation sharing its object id"
        );
    }

    #[test]
    fn stale_attachment_binding_does_not_exclude_new_incarnation_as_host() {
        let mut state = setup();
        let former_attachment = spawn_equipment(&mut state, "Returned Blade", 10);
        let selected_attachment = spawn_equipment(&mut state, "Selected Blade", 11);
        let stale_binding = ObjectIncarnationRef::from_object(
            state
                .objects
                .get(&former_attachment)
                .expect("former attachment exists"),
        );
        let returned_host = state
            .objects
            .get_mut(&former_attachment)
            .expect("former attachment remains addressable");
        returned_host.incarnation += 1;
        returned_host.card_types.core_types = vec![CoreType::Creature];
        returned_host.card_types.subtypes.clear();
        returned_host.base_card_types = returned_host.card_types.clone();
        state.last_zone_changed_ids = vec![selected_attachment, former_attachment];

        let mut ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .subtype("Equipment".to_string())
                        .controller(ControllerRef::You),
                ),
                target: TargetFilter::LastZoneChanged,
            },
            Vec::new(),
            ObjectId(999),
            PlayerId(0),
        );
        ability.bind_attach_attachment_target(stale_binding);

        let mut events = vec![];
        complete_resolution_attachment_choice(
            &mut state,
            ability,
            &[selected_attachment],
            &mut events,
        )
        .expect("a stale attachment binding must not exclude the new host incarnation");

        assert_eq!(
            state.objects[&selected_attachment].attached_to,
            Some(AttachTarget::Object(former_attachment)),
            "the selected Equipment attaches to the new incarnation, not to itself"
        );
    }

    #[test]
    fn attach_resolves_last_created_target() {
        // CR 702.182a: Attach sub-ability with TargetFilter::LastCreated resolves
        // target from state.last_created_token_ids (Job select pattern).
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rod".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&equipment_id)
            .unwrap()
            .card_types
            .subtypes
            .push("Equipment".to_string());

        let token_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Hero".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&token_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.last_created_token_ids = vec![token_id];

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::LastCreated,
            },
            vec![], // No explicit targets — should fall back to LastCreated
            equipment_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(token_id))
        );
        assert!(state
            .objects
            .get(&token_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
    }

    #[test]
    fn attach_with_explicit_targets_ignores_last_created() {
        // Regression: when explicit targets exist, LastCreated on the effect
        // should NOT be used — explicit targets take precedence.
        let mut state = setup();
        let equipment_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sword".to_string(),
            Zone::Battlefield,
        );
        let creature_a = spawn_creature(&mut state, "Bear A");
        let creature_b = spawn_creature(&mut state, "Bear B");
        state.last_created_token_ids = vec![creature_b];

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::SelfRef,
                target: TargetFilter::LastCreated,
            },
            vec![crate::types::ability::TargetRef::Object(creature_a)],
            equipment_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Should attach to creature_a (explicit target), not creature_b (LastCreated)
        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_a))
        );
    }

    #[test]
    fn attach_resolves_non_source_attachment_from_target_slot() {
        let mut state = setup();
        let source_id = spawn_creature(&mut state, "Windwalker");
        let equipment_id = spawn_with_subtype(&mut state, "Sword", "Equipment");
        let creature_id = spawn_creature(&mut state, "Bear");

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Any,
                target: TargetFilter::Any,
            },
            vec![
                crate::types::ability::TargetRef::Object(equipment_id),
                crate::types::ability::TargetRef::Object(creature_id),
            ],
            source_id,
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment_id).unwrap().attached_to,
            Some(AttachTarget::Object(creature_id))
        );
        assert!(state
            .objects
            .get(&creature_id)
            .unwrap()
            .attachments
            .contains(&equipment_id));
        assert_eq!(state.objects.get(&source_id).unwrap().attached_to, None);
    }

    #[test]
    fn attach_prohibitions_distinguish_aura_vs_equipment() {
        // CantBeEnchanted allows Equipment; CantBeEquipped allows Auras;
        // CantBeAttached blocks both.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Aura", "Aura");
        let equipment = spawn_with_subtype(&mut state, "Sword", "Equipment");

        // Case A: CantBeEnchanted creature accepts Equipment.
        let cant_be_enchanted = spawn_creature(&mut state, "Kira");
        apply_static(&mut state, cant_be_enchanted, "CantBeEnchanted");
        attach_to(&mut state, equipment, cant_be_enchanted);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(cant_be_enchanted)),
            "Equipment should attach to a creature with CantBeEnchanted"
        );
        // Aura is rejected
        attach_to(&mut state, aura, cant_be_enchanted);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            None,
            "Aura should be blocked by CantBeEnchanted"
        );

        // Case B: CantBeEquipped creature accepts Auras.
        let cant_be_equipped = spawn_creature(&mut state, "Citanul Druid");
        apply_static(&mut state, cant_be_equipped, "CantBeEquipped");
        attach_to(&mut state, aura, cant_be_equipped);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(AttachTarget::Object(cant_be_equipped)),
            "Aura should attach to a creature with CantBeEquipped"
        );

        // Case C: CantBeAttached creature rejects both.
        // Detach the aura and equipment first by removing the static from earlier cases.
        let cant_be_attached = spawn_creature(&mut state, "Warded Keep");
        apply_static(&mut state, cant_be_attached, "CantBeAttached");
        let aura2 = spawn_with_subtype(&mut state, "Aura2", "Aura");
        let equipment2 = spawn_with_subtype(&mut state, "Sword2", "Equipment");
        attach_to(&mut state, aura2, cant_be_attached);
        attach_to(&mut state, equipment2, cant_be_attached);
        assert_eq!(state.objects.get(&aura2).unwrap().attached_to, None);
        assert_eq!(state.objects.get(&equipment2).unwrap().attached_to, None);
    }

    /// Add a positive `AttachmentRestriction` static to an attachment, carrying
    /// the given legal-host `TargetFilter`.
    fn apply_attach_restriction(state: &mut GameState, id: ObjectId, filter: TargetFilter) {
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::AttachmentRestriction { filter })
                .affected(TargetFilter::SelfRef),
        );
    }

    /// Build a creature with the given power on the battlefield.
    fn spawn_creature_with_power(state: &mut GameState, name: &str, power: i32) -> ObjectId {
        let id = spawn_creature(state, name);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.power = Some(power);
        obj.toughness = Some(power.max(1));
        crate::game::layers::mark_layers_full(state);
        id
    }

    #[test]
    fn attachment_restriction_power_ge_blocks_weak_host_allows_strong_host() {
        // CR 301.5b + CR 701.3a: Strata Scythe class — Equipment that "can be
        // attached only to a creature with power 3 or greater" may not attach to a
        // power-2 creature, but may attach to a power-3 creature. The restriction
        // lives on the ATTACHMENT, not the host (contrast CantBeEquipped).
        let mut state = setup();
        let equipment = spawn_with_subtype(&mut state, "Strata Scythe", "Equipment");
        let power_filter = TargetFilter::Typed(
            crate::types::ability::TypedFilter::creature().properties(vec![
                crate::types::ability::FilterProp::PtComparison {
                    stat: crate::types::ability::PtStat::Power,
                    scope: crate::types::ability::PtValueScope::Current,
                    comparator: crate::types::ability::Comparator::GE,
                    value: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                },
            ]),
        );
        apply_attach_restriction(&mut state, equipment, power_filter);

        let weak = spawn_creature_with_power(&mut state, "Grizzly Bears", 2);
        let strong = spawn_creature_with_power(&mut state, "Hill Giant", 3);

        // Non-matching host (power 2) is an illegal attach target — attach is a no-op.
        assert_eq!(
            attachment_illegality(&state, equipment, weak),
            Some(AttachIllegality::Prohibited),
            "power-2 host must fail the power>=3 attachment restriction"
        );
        assert!(!can_attach_to_object(&state, equipment, weak));
        attach_to(&mut state, equipment, weak);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            None,
            "Equipment must not move onto a non-matching host (CR 701.3b)"
        );

        // Matching host (power 3) is a legal attach target.
        assert_eq!(attachment_illegality(&state, equipment, strong), None);
        assert!(can_attach_to_object(&state, equipment, strong));
        attach_to(&mut state, equipment, strong);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(strong)),
            "Equipment must attach onto a host matching the restriction filter"
        );
        assert!(state
            .objects
            .get(&strong)
            .unwrap()
            .attachments
            .contains(&equipment));
    }

    #[test]
    fn attachment_restriction_legendary_gates_aura_host() {
        // CR 303.4j + CR 701.3a: Konda's Banner class — an attachment restricted
        // to "a legendary creature" may not attach to a nonlegendary host.
        let mut state = setup();
        let aura = spawn_with_subtype(&mut state, "Konda's Banner", "Aura");
        let legendary_filter = TargetFilter::Typed(
            crate::types::ability::TypedFilter::creature().properties(vec![
                crate::types::ability::FilterProp::HasSupertype {
                    value: crate::types::card_type::Supertype::Legendary,
                },
            ]),
        );
        apply_attach_restriction(&mut state, aura, legendary_filter);

        let nonlegendary = spawn_creature(&mut state, "Bear");
        let legendary = spawn_creature(&mut state, "Konda, Lord of Eiganjo");
        state
            .objects
            .get_mut(&legendary)
            .unwrap()
            .card_types
            .supertypes
            .push(crate::types::card_type::Supertype::Legendary);
        crate::game::layers::mark_layers_full(&mut state);

        assert!(!can_attach_to_object(&state, aura, nonlegendary));
        attach_to(&mut state, aura, nonlegendary);
        assert_eq!(state.objects.get(&aura).unwrap().attached_to, None);

        assert!(can_attach_to_object(&state, aura, legendary));
        attach_to(&mut state, aura, legendary);
        assert_eq!(
            state.objects.get(&aura).unwrap().attached_to,
            Some(AttachTarget::Object(legendary))
        );
    }

    /// CR 301.5c: "An Equipment can't equip itself." The single-authority
    /// `attachment_illegality` self-attach guard makes `can_attach_to_object`
    /// reject a self target and `attach_to(id, id)` a no-op.
    #[test]
    fn self_attach_is_prohibited_and_no_op() {
        let mut state = setup();
        // A reconfigure Equipment is itself a creature while unattached.
        let equip = spawn_with_subtype(&mut state, "Self-Equip", "Equipment");
        state
            .objects
            .get_mut(&equip)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert_eq!(
            attachment_illegality(&state, equip, equip),
            Some(AttachIllegality::Prohibited),
            "self-attach is illegal (CR 301.5c)"
        );
        assert!(
            !can_attach_to_object(&state, equip, equip),
            "can_attach_to_object rejects self target"
        );

        assert_eq!(
            attach_to(&mut state, equip, equip),
            None,
            "attach_to(id, id) is a no-op returning None"
        );
        assert_eq!(
            state.objects.get(&equip).unwrap().attached_to,
            None,
            "self-attach leaves attached_to unset"
        );
        assert!(
            state.objects.get(&equip).unwrap().attachments.is_empty(),
            "self-attach adds nothing to attachments"
        );
    }

    #[test]
    fn attachment_cycle_is_prohibited_and_no_op() {
        let mut state = setup();
        let creature = spawn_creature(&mut state, "Bearer");
        let equipment = spawn_equipment(&mut state, "Assassin Gauntlet", 10);

        attach_to(&mut state, equipment, creature);

        assert_eq!(
            attachment_illegality(&state, creature, equipment),
            Some(AttachIllegality::Prohibited),
            "attaching a host to its own attachment would create a cycle"
        );
        assert!(!can_attach_to_object(&state, creature, equipment));
        assert_eq!(attach_to(&mut state, creature, equipment), None);
        assert_eq!(state.objects.get(&creature).unwrap().attached_to, None);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(creature))
        );
        assert_eq!(
            state.objects.get(&creature).unwrap().attachments,
            vec![equipment]
        );
        assert!(state
            .objects
            .get(&equipment)
            .unwrap()
            .attachments
            .is_empty());
    }

    #[test]
    fn attach_equipment_was_attached_to_sacrificed_source() {
        use crate::game::sacrifice::sacrifice_permanent;

        let mut state = setup();
        let zack = spawn_creature(&mut state, "Zack Fair");
        let bearer = spawn_creature(&mut state, "Bearer");
        let equipment = spawn_with_subtype(&mut state, "Hero's Sword", "Equipment");
        attach_to(&mut state, equipment, zack);

        let mut events = Vec::new();
        sacrifice_permanent(&mut state, zack, PlayerId(0), &mut events).unwrap();

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .properties(vec![FilterProp::AttachedToSource]),
                ),
                target: TargetFilter::ParentTarget,
            },
            vec![crate::types::ability::TargetRef::Object(bearer)],
            zack,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(bearer))
        );
    }

    #[test]
    fn attach_equipment_was_attached_uses_latest_departure_snapshot() {
        let mut state = setup();
        let zack = spawn_creature(&mut state, "Zack Fair");
        let bearer = spawn_creature(&mut state, "Bearer");
        let old_equipment = spawn_with_subtype(&mut state, "Old Sword", "Equipment");
        let new_equipment = spawn_with_subtype(&mut state, "New Sword", "Equipment");

        state.zone_changes_this_turn.push_back(ZoneChangeRecord {
            attachments: vec![AttachmentSnapshot {
                object_id: old_equipment,
                identity: None,
                controller: PlayerId(0),
                kind: AttachmentKind::Equipment,
            }],
            ..ZoneChangeRecord::test_minimal(zack, Some(Zone::Battlefield), Zone::Graveyard)
        });
        state.zone_changes_this_turn.push_back(ZoneChangeRecord {
            attachments: vec![AttachmentSnapshot {
                object_id: new_equipment,
                identity: None,
                controller: PlayerId(0),
                kind: AttachmentKind::Equipment,
            }],
            ..ZoneChangeRecord::test_minimal(zack, Some(Zone::Battlefield), Zone::Graveyard)
        });

        let ability = crate::types::ability::ResolvedAbility::new(
            crate::types::ability::Effect::Attach {
                attachment: TargetFilter::Typed(
                    TypedFilter::default()
                        .subtype("Equipment".to_string())
                        .properties(vec![FilterProp::AttachedToSource]),
                ),
                target: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(bearer)],
            zack,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&new_equipment).unwrap().attached_to,
            Some(AttachTarget::Object(bearer))
        );
        assert_eq!(state.objects.get(&old_equipment).unwrap().attached_to, None);
    }

    fn spawn_grant_source(state: &mut GameState, name: &str, card_id: u64) -> ObjectId {
        create_object(
            state,
            CardId(card_id),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        )
    }

    fn apply_protection_grant(
        state: &mut GameState,
        source_id: ObjectId,
        host_id: ObjectId,
        pt: crate::types::keywords::ProtectionTarget,
        exemption: Option<crate::types::ability::ProtectionDoesNotRemove>,
    ) {
        use crate::types::ability::ContinuousModification;
        use crate::types::keywords::Keyword;

        let mut def = StaticDefinition::continuous()
            .affected(TargetFilter::SpecificObject { id: host_id })
            .modifications(vec![ContinuousModification::AddKeyword {
                keyword: Keyword::Protection(pt),
            }]);
        if let Some(exemption) = exemption {
            def = def.protection_does_not_remove(exemption);
        }
        state
            .objects
            .get_mut(&source_id)
            .unwrap()
            .static_definitions
            .push(def);
    }

    fn evaluate_protection_layers(state: &mut GameState) {
        crate::game::layers::mark_layers_full(state);
        crate::game::layers::evaluate_layers(state);
    }

    fn protection_snapshot_ids(
        state: &GameState,
        source_id: ObjectId,
        def_index: usize,
        mod_index: usize,
        host_id: ObjectId,
    ) -> Vec<ObjectId> {
        state
            .objects
            .get(&source_id)
            .and_then(|source| {
                source
                    .protection_start_exempt_attachments
                    .get(&(def_index, mod_index, host_id))
            })
            .map(|entry| entry.attachment_ids.clone())
            .unwrap_or_default()
    }

    fn replace_source_protection_statics(state: &mut GameState, source_id: ObjectId) {
        use std::sync::Arc;
        let obj = state.objects.get_mut(&source_id).unwrap();
        obj.static_definitions.clear();
        obj.base_static_definitions = Arc::new(Vec::new());
        obj.base_characteristics_initialized = false;
    }

    #[test]
    fn cr_702_16p_exempts_matching_controlled_attachment_at_grant_start() {
        use crate::types::ability::ProtectionDoesNotRemove;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Bear");
        let equipment = spawn_equipment(&mut state, "Sword", 10);
        {
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.color.push(ManaColor::White);
        }
        attach_to(&mut state, equipment, host);

        let grant_source = spawn_grant_source(&mut state, "Blessing", 11);
        apply_protection_grant(
            &mut state,
            grant_source,
            host,
            ProtectionTarget::Color(ManaColor::White),
            Some(ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached),
        );
        evaluate_protection_layers(&mut state);

        let snapshot = protection_snapshot_ids(&state, grant_source, 0, 0, host);
        assert!(
            snapshot.contains(&equipment),
            "CR 702.16p: matching controlled attachment at grant start must be snapshotted"
        );
        assert_eq!(
            attachment_illegality(&state, equipment, host),
            None,
            "snapshotted attachment must remain legal"
        );

        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            Some(AttachTarget::Object(host)),
            "CR 702.16p: exempt Equipment must stay attached through SBA"
        );
    }

    #[test]
    fn cr_702_16p_does_not_exempt_attachment_that_becomes_matching_after_grant_start() {
        use crate::types::ability::ProtectionDoesNotRemove;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Bear");
        let equipment = spawn_equipment(&mut state, "Sword", 20);
        attach_to(&mut state, equipment, host);

        let grant_source = spawn_grant_source(&mut state, "Blessing", 21);
        apply_protection_grant(
            &mut state,
            grant_source,
            host,
            ProtectionTarget::Color(ManaColor::White),
            Some(ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached),
        );
        evaluate_protection_layers(&mut state);

        assert!(
            !protection_snapshot_ids(&state, grant_source, 0, 0, host).contains(&equipment),
            "colorless Equipment must not enter the start-time snapshot"
        );

        state
            .objects
            .get_mut(&equipment)
            .unwrap()
            .base_color
            .push(ManaColor::White);
        evaluate_protection_layers(&mut state);

        assert_eq!(
            attachment_illegality(&state, equipment, host),
            Some(AttachIllegality::Protection),
            "attachment that becomes matching only after grant start must be illegal (live-check bug)"
        );

        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            None,
            "CR 704.5n: Equipment that was not in the 702.16p snapshot must unattach"
        );
        assert!(
            state.battlefield.contains(&equipment),
            "Equipment stays on the battlefield after illegal attachment SBA"
        );
    }

    #[test]
    fn cr_702_16p_second_protection_grant_without_rider_still_blocks() {
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Bear");
        let equipment = spawn_equipment(&mut state, "Sword", 30);
        {
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.color.push(ManaColor::White);
        }
        attach_to(&mut state, equipment, host);

        let rider_source = spawn_grant_source(&mut state, "Blessing", 31);
        apply_protection_grant(
            &mut state,
            rider_source,
            host,
            ProtectionTarget::Color(ManaColor::White),
            Some(crate::types::ability::ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached),
        );
        evaluate_protection_layers(&mut state);

        let plain_source = spawn_grant_source(&mut state, "Mother of Runes", 32);
        apply_protection_grant(
            &mut state,
            plain_source,
            host,
            ProtectionTarget::Color(ManaColor::White),
            None,
        );
        evaluate_protection_layers(&mut state);

        assert_eq!(
            attachment_illegality(&state, equipment, host),
            Some(AttachIllegality::Protection),
            "a second protection instance without a 702.16n/p rider must still block"
        );

        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            None,
            "second un-ridered grant must remove despite the first grant's snapshot"
        );
    }

    #[test]
    fn cr_702_16p_same_source_second_effect_does_not_inherit_first_snapshot() {
        use crate::types::ability::ProtectionDoesNotRemove;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Bear");
        let equipment = spawn_equipment(&mut state, "Sword", 40);
        {
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.color.push(ManaColor::Blue);
        }
        attach_to(&mut state, equipment, host);

        let grant_source = spawn_grant_source(&mut state, "Dual Blessing", 41);
        apply_protection_grant(
            &mut state,
            grant_source,
            host,
            ProtectionTarget::Color(ManaColor::Blue),
            Some(ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached),
        );
        evaluate_protection_layers(&mut state);
        assert!(
            protection_snapshot_ids(&state, grant_source, 0, 0, host).contains(&equipment),
            "blue rider must snapshot the blue Equipment at effect start"
        );

        replace_source_protection_statics(&mut state, grant_source);
        apply_protection_grant(
            &mut state,
            grant_source,
            host,
            ProtectionTarget::Color(ManaColor::White),
            Some(ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached),
        );
        evaluate_protection_layers(&mut state);
        assert!(
            !protection_snapshot_ids(&state, grant_source, 0, 0, host).contains(&equipment),
            "white rider must not inherit the prior blue snapshot at the same def_index"
        );

        state
            .objects
            .get_mut(&equipment)
            .unwrap()
            .base_color
            .push(ManaColor::White);
        evaluate_protection_layers(&mut state);

        assert_eq!(
            attachment_illegality(&state, equipment, host),
            Some(AttachIllegality::Protection),
            "white Equipment must be illegal once it matches the white rider"
        );

        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            None,
            "Equipment that became white after the white rider started must unattach"
        );
    }

    #[test]
    fn cr_702_16p_opponent_controlled_matching_attachment_not_exempt() {
        use crate::types::ability::ProtectionDoesNotRemove;
        use crate::types::keywords::ProtectionTarget;
        use crate::types::mana::ManaColor;

        let mut state = setup();
        let host = spawn_creature(&mut state, "Bear");
        let equipment = spawn_equipment(&mut state, "Sword", 50);
        {
            let obj = state.objects.get_mut(&equipment).unwrap();
            obj.color.push(ManaColor::White);
            obj.controller = PlayerId(1);
            obj.base_controller = Some(PlayerId(1));
        }
        attach_to(&mut state, equipment, host);

        let grant_source = spawn_grant_source(&mut state, "Blessing", 51);
        apply_protection_grant(
            &mut state,
            grant_source,
            host,
            ProtectionTarget::Color(ManaColor::White),
            Some(ProtectionDoesNotRemove::ControlledAttachmentsAlreadyAttached),
        );
        evaluate_protection_layers(&mut state);

        assert!(
            !protection_snapshot_ids(&state, grant_source, 0, 0, host).contains(&equipment),
            "702.16p only exempts attachments you control at effect start"
        );
        assert_eq!(
            attachment_illegality(&state, equipment, host),
            Some(AttachIllegality::Protection)
        );

        let mut events = Vec::new();
        crate::game::sba::check_state_based_actions(&mut state, &mut events);
        assert_eq!(
            state.objects.get(&equipment).unwrap().attached_to,
            None,
            "opponent-controlled matching Equipment must unattach"
        );
    }
}
