//! Meld resolution (CR 701.42 / CR 712.4).
//!
//! Live Oracle referents are selected from current layered characteristics.
//! The selected objects are then exiled simultaneously; only afterward are
//! their physical front-face identities checked. This distinction is required
//! for copied/renamed/token objects: they may satisfy the instruction's live
//! condition while still being unable to form the printed meld pair.

use crate::game::combat::{self, AttackTarget, AttackerInfo};
use crate::game::game_object::MergeKind;
use crate::game::merge;
use crate::game::printed_cards::{
    apply_card_face_to_object, copiable_values_from_face, printed_ref_from_face,
};
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest, ZoneMoveResult};
use crate::types::ability::{
    ControllerRef, Effect, EffectError, FilterProp, PermanentEntryMode, ResolvedAbility,
    TargetFilter, TypedFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    BatchCompletion, GameState, LiminalEntry, LiminalEntryKind, MeldSelection, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::EtbTapState;
use crate::types::zones::Zone;

/// CR 701.42a-b: choose the live referents for a meld instruction. Physical
/// pair validation deliberately occurs after the exile instruction.
pub fn perform_meld(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::Meld {
        source: expected_source,
        partner: expected_partner,
        result,
        source_filter,
        partner_filter,
        entry,
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam("Meld".to_string()));
    };

    let source_id = ability.source_id;
    let controller = ability.controller;
    let filter_ctx = crate::game::filter::FilterContext::from_ability(ability);
    let source_matches = state.objects.get(&source_id).is_some_and(|object| {
        object.zone == Zone::Battlefield
            && object.owner == controller
            && object.controller == controller
            && crate::game::filter::matches_target_filter(
                state,
                source_id,
                source_filter,
                &filter_ctx,
            )
    });
    if !source_matches {
        emit_resolved(state, ability.source_id, events);
        return Ok(());
    }

    let effective_partner_filter = if matches!(partner_filter, TargetFilter::Any) {
        legacy_partner_filter(expected_partner)
    } else {
        partner_filter.clone()
    };
    let choices: Vec<MeldSelection> = state
        .battlefield
        .iter()
        .copied()
        .filter(|candidate| *candidate != source_id)
        .filter(|candidate| {
            state.objects.get(candidate).is_some_and(|object| {
                object.owner == controller
                    && object.controller == controller
                    && crate::game::filter::matches_target_filter(
                        state,
                        *candidate,
                        &effective_partner_filter,
                        &filter_ctx,
                    )
            })
        })
        .map(|partner_id| MeldSelection {
            source_id,
            partner_id,
            controller,
            expected_source: expected_source.clone(),
            expected_partner: expected_partner.clone(),
            result: result.clone(),
            entry: entry.clone(),
        })
        .collect();

    match choices.as_slice() {
        [] => emit_resolved(state, ability.source_id, events),
        [only] => begin_selected_meld(state, only.clone(), events),
        _ => {
            state.waiting_for = WaitingFor::MeldPairChoice {
                player: controller,
                choices,
            };
        }
    }
    Ok(())
}

fn legacy_partner_filter(name: &str) -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: Vec::new(),
        controller: Some(ControllerRef::You),
        properties: vec![
            FilterProp::Named {
                name: name.to_string(),
            },
            FilterProp::Owned {
                controller: ControllerRef::You,
            },
        ],
    })
}

/// Start the exact selected pair's simultaneous exile batch.
pub(crate) fn begin_selected_meld(
    state: &mut GameState,
    context: MeldSelection,
    events: &mut Vec<GameEvent>,
) {
    let requests = vec![
        ZoneMoveRequest::effect(context.source_id, Zone::Exile, context.source_id),
        ZoneMoveRequest::effect(context.partner_id, Zone::Exile, context.source_id),
    ];
    let completion = BatchCompletion::MeldExile {
        context: context.clone(),
    };
    match zone_pipeline::move_objects_simultaneously_then(state, requests, Some(completion), events)
    {
        BatchMoveResult::Done | BatchMoveResult::NeedsChoice => {}
    }
}

/// CR 701.42b-c + CR 400.7j: after both exile attempts settle, validate the
/// tracked physical cards in whatever public zones the instruction put them.
pub(crate) fn finish_meld_exile(
    state: &mut GameState,
    context: MeldSelection,
    events: &mut Vec<GameEvent>,
) {
    if !is_canonical_physical_meld_pair(state, &context)
        || !state
            .card_face_registry
            .contains_key(&context.result.to_lowercase())
    {
        finish_resolution(state, context.source_id, events);
        return;
    }

    // CR 508.4 + CR 614.12a: replacement effects can determine the entrant's
    // controller. The attack-destination choice is therefore made by the final
    // controller after the replacement pipeline has produced its approved
    // event, immediately before delivery.
    finish_meld_entry(state, context, None, events);
}

/// CR 701.42b + CR 400.2 + CR 400.7j: whether a selected pair is the canonical
/// physical meld pair and remains findable after the preceding exile
/// instruction. Replacement effects may leave a card where it was or move it
/// to a different public zone; neither changes the tracked card identity.
pub fn is_canonical_physical_meld_pair(state: &GameState, context: &MeldSelection) -> bool {
    let pair_key = format!(
        "{}\0{}",
        context.expected_source.to_lowercase(),
        context.expected_partner.to_lowercase()
    );
    let canonical_pair = state
        .meld_pair_registry
        .get(&pair_key)
        .is_some_and(|record| {
            record.source.eq_ignore_ascii_case(&context.expected_source)
                && record
                    .partner
                    .eq_ignore_ascii_case(&context.expected_partner)
                && record.result.eq_ignore_ascii_case(&context.result)
        });
    let physical_pair = state.objects.get(&context.source_id).is_some_and(|object| {
        is_public_zone(object.zone)
            && object.is_represented_by_a_card()
            && object
                .base_name
                .eq_ignore_ascii_case(&context.expected_source)
    }) && state
        .objects
        .get(&context.partner_id)
        .is_some_and(|object| {
            is_public_zone(object.zone)
                && object.is_represented_by_a_card()
                && object
                    .base_name
                    .eq_ignore_ascii_case(&context.expected_partner)
        });
    canonical_pair && physical_pair
}

/// CR 400.2: graveyard, battlefield, stack, exile, and command are public
/// zones. Hand and library remain hidden even when their cards are revealed.
fn is_public_zone(zone: Zone) -> bool {
    match zone {
        Zone::Battlefield | Zone::Graveyard | Zone::Stack | Zone::Exile | Zone::Command => true,
        Zone::Library | Zone::Hand => false,
    }
}

/// Commit the projected result entry, then atomically make the second card a
/// component if (and only if) the result actually reached the battlefield.
pub(crate) fn finish_meld_entry(
    state: &mut GameState,
    context: MeldSelection,
    attack_target: Option<AttackTarget>,
    events: &mut Vec<GameEvent>,
) {
    let Some(result_face) = state
        .card_face_registry
        .get(&context.result.to_lowercase())
        .cloned()
    else {
        finish_resolution(state, context.source_id, events);
        return;
    };

    // CR 614.12: replacement effects inspect the characteristics the meld result
    // will have on the battlefield. Keep that projection detached while choices
    // are pending: the two physical component cards remain ordinary objects in
    // their post-instruction public zones until the entry actually commits.
    let Some(mut projected) = state.objects.get(&context.source_id).cloned() else {
        finish_resolution(state, context.source_id, events);
        return;
    };
    apply_card_face_to_object(&mut projected, &result_face);
    projected.controller = context.controller;
    let enters_tapped = matches!(context.entry, PermanentEntryMode::TappedAndAttacking { .. });
    if enters_tapped {
        projected.tapped = true;
    }
    state.liminal_entries.insert(
        context.source_id,
        LiminalEntry {
            object: projected,
            name: context.result.clone(),
            source_id: context.source_id,
            controller: context.controller,
            enters_attacking: attack_target.is_some(),
            attach_to: None,
            sacrifice_at: None,
            remaining_count: 0,
            created_ids: Vec::new(),
            copy_resume: None,
            spec_resume: None,
            enter_tapped: if enters_tapped {
                EtbTapState::Tapped
            } else {
                EtbTapState::Unspecified
            },
            enter_with_counters: Vec::new(),
            kind: LiminalEntryKind::Meld {
                context: context.clone(),
                attack_target,
            },
            replacement_applied: std::collections::HashSet::new(),
        },
    );

    let mut request =
        ZoneMoveRequest::effect(context.source_id, Zone::Battlefield, context.source_id);
    if enters_tapped {
        request = request.tapped();
    }
    match zone_pipeline::move_object(state, request, events) {
        ZoneMoveResult::NeedsChoice(_) | ZoneMoveResult::NeedsAuraAttachmentChoice => {
            zone_pipeline::defer_completion_on_pause(
                state,
                BatchCompletion::MeldEntry {
                    context,
                    attack_target,
                },
            );
        }
        ZoneMoveResult::Done => finish_meld_delivery(state, context, attack_target, events),
    }
}

/// Finish a synchronously delivered or replacement-resumed meld entry.
pub(crate) fn finish_meld_delivery(
    state: &mut GameState,
    context: MeldSelection,
    attack_target: Option<AttackTarget>,
    events: &mut Vec<GameEvent>,
) {
    // The approved entry event may have changed the entrant's controller and
    // may have auto-selected the only CR 508.4 destination after replacement
    // processing. That post-replacement liminal state is authoritative for both
    // synchronous delivery and a deferred BatchCompletion resume.
    let (context, _attack_target, replacement_applied) = state
        .liminal_entries
        .get(&context.source_id)
        .and_then(|entry| match &entry.kind {
            LiminalEntryKind::Meld {
                context,
                attack_target,
                ..
            } => Some((
                context.clone(),
                *attack_target,
                entry.replacement_applied.clone(),
            )),
            LiminalEntryKind::Token => None,
        })
        .unwrap_or((context, attack_target, Default::default()));
    state.liminal_entries.remove(&context.source_id);
    let destination = state
        .objects
        .get(&context.source_id)
        .map(|object| object.zone);
    if destination != Some(Zone::Battlefield) {
        // CR 701.42c: a prevented entry leaves both cards in exile. If a
        // replacement sent the result elsewhere, route the second physical card
        // through its own CR 400.6 replacement consult to the same destination.
        // CR 614.5: seed that modified event with the result event's applied set
        // so the redirect that fixed the shared destination cannot apply again.
        if let Some(zone) = destination.filter(|zone| *zone != Zone::Exile) {
            let request = ZoneMoveRequest::effect(context.partner_id, zone, context.source_id)
                .with_replacement_applied(replacement_applied);
            let completion = BatchCompletion::MeldRedirect {
                source_id: context.source_id,
            };
            match zone_pipeline::move_objects_simultaneously_then(
                state,
                vec![request],
                Some(completion),
                events,
            ) {
                BatchMoveResult::Done | BatchMoveResult::NeedsChoice => {}
            }
            return;
        }
        finish_resolution(state, context.source_id, events);
        return;
    }

    if commit_meld_battlefield(state, &context) {
        finish_deferred_meld_entry(state, context, events);
    } else {
        finish_resolution(state, context.source_id, events);
    }
}

/// Materialize the meld result's layer-1 identity and physical component set.
/// This is separate from resolution completion so a CR 707.9 copy-as-enters
/// choice can install its later-timestamp copy effect after the meld identity.
pub(crate) fn commit_meld_battlefield(state: &mut GameState, context: &MeldSelection) -> bool {
    state.liminal_entries.remove(&context.source_id);

    let Some(result_face) = state
        .card_face_registry
        .get(&context.result.to_lowercase())
        .cloned()
    else {
        return false;
    };
    let entry_controller = state
        .objects
        .get(&context.source_id)
        .map(|object| object.controller)
        .unwrap_or(context.controller);
    let values = copiable_values_from_face(&result_face);
    let printed_ref = printed_ref_from_face(&result_face);
    merge::install_merge_layer_effect(
        state,
        context.source_id,
        entry_controller,
        values,
        crate::game::game_object::DisplaySource::Card,
        printed_ref,
        None,
    );
    // CR 701.42a / CR 730.2: absorb the partner into the single melded permanent
    // — it is no longer an independent object; remove it from the exile list and
    // mark it absorbed (zone == Battlefield, in no zone list), mirroring
    // merge_object_onto, so the CR 712.21 leave-split routes it to the graveyard
    // exactly once. This runs BEFORE the survivor's pipeline entry below: an
    // entry-replacement consult (CR 614.1c) can park a `NeedsChoice` pause, and
    // absorbing first guarantees the partner is never stranded in exile across
    // that pause.
    crate::game::zones::absorb_component(state, context.partner_id, Some(Zone::Exile));
    if let Some(survivor) = state.objects.get_mut(&context.source_id) {
        survivor.merged_components = vec![context.source_id, context.partner_id];
        survivor.merge_kind = Some(MergeKind::Meld);
    }
    crate::game::layers::flush_layers(state);

    if matches!(context.entry, PermanentEntryMode::TappedAndAttacking { .. }) {
        if let Some(object) = state.objects.get_mut(&context.source_id) {
            object.tapped = true;
        }
    }
    true
}

/// CR 508.4 + CR 611.3: after every as-enters choice and continuous-effect
/// layer has finalized the melded permanent, use its current creature status
/// and controller to determine whether and where it enters attacking.
pub(crate) fn finish_deferred_meld_entry(
    state: &mut GameState,
    mut context: MeldSelection,
    events: &mut Vec<GameEvent>,
) {
    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);

    let Some(object) = state.objects.get(&context.source_id) else {
        finish_resolution(state, context.source_id, events);
        return;
    };
    if object.zone != Zone::Battlefield {
        finish_resolution(state, context.source_id, events);
        return;
    }
    let controller = object.controller;
    let is_creature = object
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Creature);
    context.controller = controller;

    let targets = match &context.entry {
        PermanentEntryMode::TappedAndAttacking { destination } if is_creature => {
            combat::valid_entry_attack_targets(state, controller, destination)
        }
        PermanentEntryMode::Normal | PermanentEntryMode::TappedAndAttacking { .. } => Vec::new(),
    };

    if targets.len() > 1 {
        park_meld_entry_event(state, context.source_id, events);
        state.waiting_for = WaitingFor::MeldAttackTargetChoice {
            player: controller,
            context,
            valid_targets: targets,
        };
        return;
    }

    let attack_target = targets.first().copied();
    commit_final_attack_status(state, &context, attack_target);
    finalize_meld_entry_snapshot(state, context.source_id, attack_target, events);
    finish_resolution(state, context.source_id, events);
}

/// Finish the CR 508.4 destination choice against the permanent's final live
/// controller and creature characteristics. A destination that became stale
/// leaves the permanent tapped but nonattacking (CR 508.4a).
pub(crate) fn finish_meld_attack_choice(
    state: &mut GameState,
    mut context: MeldSelection,
    selected: AttackTarget,
    events: &mut Vec<GameEvent>,
) {
    crate::game::layers::mark_layers_full(state);
    crate::game::layers::flush_layers(state);
    let (controller, is_creature) = state
        .objects
        .get(&context.source_id)
        .map(|object| {
            (
                object.controller,
                object
                    .card_types
                    .core_types
                    .contains(&crate::types::card_type::CoreType::Creature),
            )
        })
        .unwrap_or((context.controller, false));
    context.controller = controller;
    let still_valid = match &context.entry {
        PermanentEntryMode::TappedAndAttacking { destination } if is_creature => {
            combat::valid_entry_attack_targets(state, controller, destination).contains(&selected)
        }
        PermanentEntryMode::Normal | PermanentEntryMode::TappedAndAttacking { .. } => false,
    };
    let attack_target = still_valid.then_some(selected);
    commit_final_attack_status(state, &context, attack_target);
    finalize_meld_entry_snapshot(state, context.source_id, attack_target, events);
    finish_resolution(state, context.source_id, events);
}

fn commit_final_attack_status(
    state: &mut GameState,
    context: &MeldSelection,
    attack_target: Option<AttackTarget>,
) {
    if let Some(combat_state) = state.combat.as_mut() {
        combat_state
            .attackers
            .retain(|attacker| attacker.object_id != context.source_id);
    }
    let Some(target) = attack_target else {
        return;
    };
    let Some(defending_player) =
        combat::entry_attack_target_defender(state, context.controller, target)
    else {
        return;
    };
    if let Some(combat_state) = state.combat.as_mut() {
        combat_state.attackers.push(AttackerInfo::new(
            context.source_id,
            target,
            defending_player,
        ));
        state.layers_dirty.mark_full();
    }
}

fn park_meld_entry_event(state: &mut GameState, source_id: ObjectId, events: &mut Vec<GameEvent>) {
    if let Some(index) = events.iter().rposition(|event| {
        matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                to: Zone::Battlefield,
                ..
            } if *object_id == source_id
        )
    }) {
        state.deferred_entry_events.push(events.remove(index));
    }
}

fn finalize_meld_entry_snapshot(
    state: &mut GameState,
    source_id: ObjectId,
    attack_target: Option<AttackTarget>,
    events: &mut Vec<GameEvent>,
) {
    let defending_player = attack_target.and_then(|target| {
        state.objects.get(&source_id).and_then(|object| {
            combat::entry_attack_target_defender(state, object.controller, target)
        })
    });
    let combat_status = crate::types::game_state::ZoneChangeCombatStatus {
        attacking: defending_player.is_some(),
        defending_player,
        ..Default::default()
    };
    refresh_meld_entry_records(state, source_id, combat_status, events);

    if !state.deferred_entry_events.is_empty() {
        events.extend(state.deferred_entry_events.iter().cloned());
        let _ =
            crate::game::engine_replacement::replay_deferred_entry_events(state, source_id, events);
    }
}

fn refresh_meld_entry_records(
    state: &mut GameState,
    source_id: ObjectId,
    combat_status: crate::types::game_state::ZoneChangeCombatStatus,
    events: &mut [GameEvent],
) {
    let Some(object) = state.objects.get(&source_id).cloned() else {
        return;
    };
    let refresh = |record: &mut crate::types::game_state::ZoneChangeRecord| {
        let old = record.clone();
        let mut realized = object.snapshot_for_zone_change(source_id, old.from_zone, old.to_zone);
        realized.cast_from_zone = old.cast_from_zone;
        realized.played_from_zone = old.played_from_zone;
        realized.attachments = old.attachments;
        realized.linked_exile_snapshot = old.linked_exile_snapshot;
        realized.co_departed = old.co_departed;
        realized.entered_incarnation = old.entered_incarnation;
        realized.attached_to = old.attached_to;
        realized.turn_zone_change_index = old.turn_zone_change_index;
        realized.combat_status = combat_status;
        *record = realized;
    };

    for event in events
        .iter_mut()
        .chain(state.deferred_entry_events.iter_mut())
    {
        if let GameEvent::ZoneChanged {
            object_id,
            to: Zone::Battlefield,
            record,
            ..
        } = event
        {
            if *object_id == source_id {
                refresh(record);
            }
        }
    }
    if let Some(record) = state
        .zone_changes_this_turn
        .iter_mut()
        .rev()
        .find(|record| record.object_id == source_id && record.to_zone == Zone::Battlefield)
    {
        refresh(record);
    }
    if let Some(record) = state
        .battlefield_entries_this_turn
        .iter_mut()
        .rev()
        .find(|record| record.object_id == source_id)
    {
        record.name = object.name.clone();
        record.core_types = object.card_types.core_types.clone();
        record.subtypes = object.card_types.subtypes.clone();
        record.supertypes = object.card_types.supertypes.clone();
        record.colors = object.color.clone();
        record.keywords = object.keywords.clone();
        record.controller = object.controller;
    }
}

pub(crate) fn finish_deferred_meld_resolution(
    state: &mut GameState,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    finish_resolution(state, source_id, events);
}

fn emit_resolved(state: &mut GameState, source_id: ObjectId, events: &mut Vec<GameEvent>) {
    events.push(GameEvent::EffectResolved {
        kind: crate::types::ability::EffectKind::Meld,
        source_id,
        subject: None,
    });
    state.last_effect_count = Some(1);
}

fn finish_resolution(state: &mut GameState, source_id: ObjectId, events: &mut Vec<GameEvent>) {
    emit_resolved(state, source_id, events);
    crate::game::effects::drain_pending_continuation(state, events);
}
