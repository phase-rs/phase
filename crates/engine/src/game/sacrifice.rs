use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::restrictions;
use crate::game::zone_pipeline::{
    self, ApprovedZoneChange, DeliveryCtx, ExileLinkSpec, ZoneDeliveryResult,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, PendingSacrificeDelivery, PendingSacrificeMove, PendingSacrificeProvenance,
    ResidentDrainPolicy,
};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

use super::engine::EngineError;

/// Outcome of a sacrifice attempt routed through the replacement pipeline.
pub(crate) enum SacrificeOutcome {
    /// Sacrifice completed (normally or via replacement redirect).
    Complete,
    /// A replacement effect requires player choice before sacrifice can proceed.
    /// Callers must handle this by surfacing the replacement choice to the player.
    NeedsReplacementChoice(PlayerId),
    /// CR 109.5 + CR 701.21a: this actor is barred from sacrificing the
    /// object (resolution-created "you can't sacrifice" binds its installer;
    /// printed "can't be sacrificed" bars all). Nothing was sacrificed, moved,
    /// or emitted. Cost paths must fail payment; effect paths skip the object
    /// (CR 609.3: do as much as possible).
    Prohibited,
}

/// Signed current power of the selected permanents. Aggregate sacrifice costs
/// count every selected permanent, including zero- and negative-power objects.
pub(crate) fn selected_total_power(state: &GameState, selected: &[ObjectId]) -> i32 {
    selected
        .iter()
        .filter_map(|id| state.objects.get(id))
        .map(|object| object.power.unwrap_or(0))
        .sum()
}

/// CR 701.21 + CR 118.3: Sacrifice a permanent — move to graveyard as a cost or effect.
/// Routes through replacement pipeline (e.g., Rest in Peace → exile).
///
/// Returns `SacrificeOutcome` so callers can handle the `NeedsChoice` case appropriately:
/// - Effect resolution: pause via `WaitingFor::ReplacementChoice`
/// - Cost payment: proceed with default sacrifice (extremely rare edge case)
pub(crate) fn sacrifice_permanent(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<SacrificeOutcome, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Sacrifice target not found".to_string()))?;
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::ActionNotAllowed(
            "Cannot sacrifice: permanent is not on the battlefield".to_string(),
        ));
    }

    // CR 109.5 + CR 701.21: "Can't be sacrificed" prevents this action for the
    // barred actor — no graveyard move, no leaves-the-battlefield triggers,
    // no events emitted. Cost callers fail on `Prohibited`; effect callers
    // skip the object.
    if crate::game::static_abilities::object_cant_be_sacrificed_by(state, player, object_id) {
        return Ok(SacrificeOutcome::Prohibited);
    }

    let proposed = ProposedEvent::Sacrifice {
        object_id,
        player_id: player,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            match apply_sacrifice_after_replacement(state, event, events) {
                SacrificeApply::Complete => {}
                SacrificeApply::NeedsChoice(player) => {
                    // `state.waiting_for` is already set by the inner pass.
                    return Ok(SacrificeOutcome::NeedsReplacementChoice(player));
                }
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(choice_player) => {
            return Ok(SacrificeOutcome::NeedsReplacementChoice(choice_player));
        }
    }

    Ok(SacrificeOutcome::Complete)
}

/// Outcome of delivering an accepted Sacrifice proposed event through the inner
/// graveyard-move replacement pass. `NeedsChoice` carries the player who must
/// order/choose among multiple applicable graveyard replacements (CR 616.1);
/// `state.waiting_for` is already set when it is returned.
pub(crate) enum SacrificeApply {
    Complete,
    NeedsChoice(PlayerId),
}

/// CR 701.21a + CR 614.1: Apply an accepted Sacrifice proposed event.
///
/// The sacrifice's move to the graveyard is itself a zone change subject to
/// replacement (CR 701.21a + CR 614.1), so this routes the inner graveyard
/// `ZoneChange` through the replacement pipeline — mirroring
/// `apply_destroy_after_replacement` — instead of moving straight to the
/// graveyard. A "would be put into a graveyard from anywhere → exile/redirect
/// instead" `Moved` replacement (Disturb back faces, Rest in Peace, Leyline of
/// the Void) therefore applies on sacrifice too. Records the sacrifice for
/// restriction tracking (CR 701.21) and emits `PermanentSacrificed` regardless
/// of the redirected zone (leaves-the-battlefield is still a sacrifice).
///
/// Shared by the cost/effect path (`sacrifice_permanent`) and the
/// post-replacement-choice delivery path (`handle_replacement_choice`).
pub(crate) fn apply_sacrifice_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> SacrificeApply {
    match event {
        ProposedEvent::Sacrifice {
            object_id: oid,
            player_id: pid,
            ..
        } => {
            // CR 701.21: record the sacrifice for restriction tracking before any
            // redirect — leaving the battlefield is still a sacrifice.
            restrictions::record_sacrifice(state, oid, pid);
            // CR 701.21a + CR 614.1: propose the inner graveyard move so a Moved
            // replacement can intercept it. The Sacrifice proposal carries no
            // source, so `cause` is None.
            let zone_proposed =
                ProposedEvent::zone_change(oid, Zone::Battlefield, Zone::Graveyard, None);
            match replacement::replace_event(state, zone_proposed, events) {
                ReplacementResult::Execute(zone_event @ ProposedEvent::ZoneChange { .. }) => {
                    // CR 701.21a + CR 614: the inner ZoneChange already cleared the
                    // replacement consult — seal it as a proof token and deliver
                    // through the single pipeline tail so a sacrifice redirected to
                    // the battlefield gets the full delivery treatment.
                    if let SacrificeApply::NeedsChoice(player) =
                        deliver_sacrifice_zone_change(state, zone_event, events)
                    {
                        if let Some(pending) = state.pending_replacement.as_mut() {
                            pending.sacrifice_provenance = Some(PendingSacrificeProvenance {
                                object_id: oid,
                                player_id: pid,
                            });
                        } else {
                            // CR 701.21: The delivery move completed; only an ordinary post-effect
                            // child prompt paused. Emit the sacrifice event now so observers trigger.
                            events.push(GameEvent::PermanentSacrificed {
                                object_id: oid,
                                player_id: pid,
                            });
                        }
                        return SacrificeApply::NeedsChoice(player);
                    }
                }
                // Defensive: the inner proposal is always a ZoneChange.
                ReplacementResult::Execute(_) => {}
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    // CR 616.1: an ordered/optional Moved replacement needs a
                    // choice — pause and let the caller resume. The pending
                    // replacement now owns the inner ZoneChange, so retain the
                    // enclosing sacrifice's identity until that move resumes.
                    if let Some(pending) = state.pending_replacement.as_mut() {
                        pending.sacrifice_provenance = Some(PendingSacrificeProvenance {
                            object_id: oid,
                            player_id: pid,
                        });
                    }
                    state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
                    return SacrificeApply::NeedsChoice(player);
                }
            }
            // CR 701.21: PermanentSacrificed fires regardless of the redirected
            // zone (leaves-the-battlefield is still a sacrifice).
            events.push(GameEvent::PermanentSacrificed {
                object_id: oid,
                player_id: pid,
            });
            SacrificeApply::Complete
        }
        ProposedEvent::ZoneChange { .. } => {
            // CR 614.6: the outer Sacrifice replacement redirected directly to a
            // zone change. Deliver through the pipeline tail so a redirect to the
            // battlefield gets the full delivery treatment, not a bare move.
            deliver_sacrifice_zone_change(state, event, events)
        }
        _ => SacrificeApply::Complete,
    }
}

/// Outcome of consulting a sacrifice's replacements ahead of its delivery.
pub(crate) enum SacrificeConsult {
    /// The move cleared every replacement consult; deliver it with
    /// [`deliver_consulted_sacrifice`].
    Ready(Box<PendingSacrificeDelivery>),
    /// Nothing moves: the permanent is gone or the sacrifice itself was
    /// replaced away.
    NoMove,
    /// CR 109.5 + CR 701.21: this actor is barred from sacrificing the object;
    /// nothing was consulted or emitted (see [`SacrificeOutcome::Prohibited`]).
    Prohibited,
    /// CR 616.1: a replacement choice is pending and `state.pending_replacement`
    /// owns the move. `state.waiting_for` is not set.
    NeedsChoice(PlayerId),
}

impl SacrificeConsult {
    fn ready(sacrifice: PendingSacrificeMove) -> Self {
        Self::Ready(Box::new(PendingSacrificeDelivery {
            sacrifice,
            rider: None,
        }))
    }
}

/// CR 101.4 + CR 701.21a + CR 614.1: The consult half of
/// [`sacrifice_permanent`] for a simultaneous sacrifice of several permanents.
/// Runs the outer Sacrifice and inner graveyard `ZoneChange` replacement passes
/// without moving anything, so each permanent's replacements are determined
/// while every co-sacrificed permanent is still on the battlefield (e.g. Dauthi
/// Voidwalker still exiles an opponent's card sacrificed alongside it). The
/// caller delivers every `Ready` move afterwards.
pub(crate) fn consult_sacrifice(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> SacrificeConsult {
    with_consult_rider(state, |state| {
        consult_sacrifice_move(state, object_id, player, events)
    })
}

/// CR 101.4 + CR 616.1: finish the consult of a sacrifice that
/// [`consult_sacrifice`] parked on a CR 616.1 choice, through `operation`: the
/// chooser's answer, or a departure's reconsultation of the changed event.
/// Like the consult itself, nothing moves: an approved move is returned
/// `Ready` for the caller to queue with the rest of the batch, and another
/// pending choice is returned `NeedsChoice`.
pub(crate) fn consult_answered_sacrifice(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    operation: impl FnOnce(&mut GameState, &mut Vec<GameEvent>) -> ReplacementResult,
) -> SacrificeConsult {
    // The inner graveyard move carries the sacrifice it belongs to; the outer
    // Sacrifice proposal names it itself.
    let inner = state
        .pending_replacement
        .as_ref()
        .and_then(|pending| pending.sacrifice_provenance);
    with_consult_rider(state, |state| {
        let result = operation(state, events);
        match inner {
            Some(PendingSacrificeProvenance {
                object_id,
                player_id,
            }) => consult_graveyard_move(state, object_id, player_id, result),
            None => consult_sacrifice_result(state, result, events),
        }
    })
}

/// CR 614.6: a ready drain resident before the consult belongs to earlier
/// work; only one this consult installed is this move's rider.
fn with_consult_rider(
    state: &mut GameState,
    consult: impl FnOnce(&mut GameState) -> SacrificeConsult,
) -> SacrificeConsult {
    let ready_before = state
        .active_post_replacement_drains()
        .is_some_and(|drains| drains.has_ready());
    let mut consult = consult(state);
    if let SacrificeConsult::Ready(delivery) = &mut consult {
        if !ready_before {
            delivery.rider = state.take_ready_post_replacement_drain();
        }
    }
    consult
}

fn consult_sacrifice_move(
    state: &mut GameState,
    object_id: ObjectId,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> SacrificeConsult {
    let on_battlefield = state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield);
    if !on_battlefield {
        return SacrificeConsult::NoMove;
    }
    // CR 109.5 + CR 701.21: "Can't be sacrificed" prevents this action for the
    // barred actor, exactly as in `sacrifice_permanent`.
    if crate::game::static_abilities::object_cant_be_sacrificed_by(state, player, object_id) {
        return SacrificeConsult::Prohibited;
    }
    let proposed = ProposedEvent::Sacrifice {
        object_id,
        player_id: player,
        applied: HashSet::new(),
    };
    let result = replacement::replace_event(state, proposed, events);
    consult_sacrifice_result(state, result, events)
}

/// CR 614.1: the outer Sacrifice replacement pass has a result; consult the
/// graveyard move it leads to.
fn consult_sacrifice_result(
    state: &mut GameState,
    result: ReplacementResult,
    events: &mut Vec<GameEvent>,
) -> SacrificeConsult {
    match result {
        ReplacementResult::Execute(ProposedEvent::Sacrifice {
            object_id: oid,
            player_id: pid,
            ..
        }) => {
            // CR 701.21: record the sacrifice for restriction tracking before any
            // redirect, as `apply_sacrifice_after_replacement` does.
            restrictions::record_sacrifice(state, oid, pid);
            let zone_proposed =
                ProposedEvent::zone_change(oid, Zone::Battlefield, Zone::Graveyard, None);
            let result = replacement::replace_event(state, zone_proposed, events);
            consult_graveyard_move(state, oid, pid, result)
        }
        // CR 614.6: the outer replacement redirected straight to a zone change.
        ReplacementResult::Execute(zone_event @ ProposedEvent::ZoneChange { .. }) => {
            SacrificeConsult::ready(PendingSacrificeMove::DirectMove { zone_event })
        }
        ReplacementResult::Execute(_) | ReplacementResult::Prevented => SacrificeConsult::NoMove,
        ReplacementResult::NeedsChoice(choice_player) => {
            SacrificeConsult::NeedsChoice(choice_player)
        }
    }
}

/// CR 701.21a + CR 614.1: the inner graveyard move of an accepted sacrifice
/// has a replacement result.
fn consult_graveyard_move(
    state: &mut GameState,
    object_id: ObjectId,
    player_id: PlayerId,
    result: ReplacementResult,
) -> SacrificeConsult {
    match result {
        ReplacementResult::Execute(zone_event @ ProposedEvent::ZoneChange { .. }) => {
            SacrificeConsult::ready(PendingSacrificeMove::Sacrifice {
                object_id,
                player_id,
                zone_event,
            })
        }
        // Defensive: the inner proposal is always a ZoneChange. A prevented
        // move is still a sacrifice (CR 701.21).
        ReplacementResult::Execute(_) | ReplacementResult::Prevented => {
            SacrificeConsult::ready(PendingSacrificeMove::InnerPrevented {
                object_id,
                player_id,
            })
        }
        ReplacementResult::NeedsChoice(choice_player) => {
            if let Some(pending) = state.pending_replacement.as_mut() {
                pending.sacrifice_provenance = Some(PendingSacrificeProvenance {
                    object_id,
                    player_id,
                });
            }
            SacrificeConsult::NeedsChoice(choice_player)
        }
    }
}

/// Outcome of delivering a consulted sacrifice.
pub(crate) enum SacrificeDelivery {
    Delivered,
    /// CR 400.7: the permanent left the battlefield (or its sacrificer's
    /// control) after the consult; nothing moves.
    Skipped,
    /// The delivery tail paused on a choice; `state.waiting_for` is set.
    NeedsChoice(PlayerId),
}

/// CR 701.21a: The delivery half of [`sacrifice_permanent`] for a move that
/// [`consult_sacrifice`] already approved. Emits `PermanentSacrificed` for a
/// sacrifice, whichever zone it was redirected to.
pub(crate) fn deliver_consulted_sacrifice(
    state: &mut GameState,
    delivery: PendingSacrificeDelivery,
    events: &mut Vec<GameEvent>,
) -> SacrificeDelivery {
    let PendingSacrificeDelivery { sacrifice, rider } = delivery;
    let still_present = match &sacrifice {
        PendingSacrificeMove::Sacrifice {
            object_id,
            player_id,
            ..
        } => state
            .objects
            .get(object_id)
            .is_some_and(|obj| obj.zone == Zone::Battlefield && obj.controller == *player_id),
        PendingSacrificeMove::InnerPrevented { .. } => true,
        PendingSacrificeMove::DirectMove { zone_event } => zone_event
            .affected_object_id()
            .and_then(|id| state.objects.get(&id))
            .is_some_and(|obj| obj.zone == Zone::Battlefield),
    };
    // CR 400.7: the permanent left the battlefield (or its sacrificer's
    // control) after the consult; nothing moves, and the replacement's own
    // work for that move does not happen either.
    if !still_present {
        return SacrificeDelivery::Skipped;
    }
    // CR 614.6: the replacement's own work (Kalitas's Zombie) happens with the
    // move it modified, so it is resident when this move's delivery tail drains.
    if let Some(rider) = rider {
        state.install_post_replacement_drain(rider, ResidentDrainPolicy::KeepResident);
    }
    match sacrifice {
        PendingSacrificeMove::Sacrifice {
            object_id,
            player_id,
            zone_event,
        } => {
            if let SacrificeApply::NeedsChoice(choice_player) =
                deliver_sacrifice_zone_change(state, zone_event, events)
            {
                if let Some(pending) = state.pending_replacement.as_mut() {
                    pending.sacrifice_provenance = Some(PendingSacrificeProvenance {
                        object_id,
                        player_id,
                    });
                } else {
                    // CR 701.21: the move completed; only a post-effect child
                    // prompt paused.
                    events.push(GameEvent::PermanentSacrificed {
                        object_id,
                        player_id,
                    });
                }
                return SacrificeDelivery::NeedsChoice(choice_player);
            }
            events.push(GameEvent::PermanentSacrificed {
                object_id,
                player_id,
            });
            SacrificeDelivery::Delivered
        }
        PendingSacrificeMove::InnerPrevented {
            object_id,
            player_id,
        } => {
            events.push(GameEvent::PermanentSacrificed {
                object_id,
                player_id,
            });
            SacrificeDelivery::Delivered
        }
        PendingSacrificeMove::DirectMove { zone_event } => {
            match deliver_sacrifice_zone_change(state, zone_event, events) {
                SacrificeApply::Complete => SacrificeDelivery::Delivered,
                SacrificeApply::NeedsChoice(choice_player) => {
                    SacrificeDelivery::NeedsChoice(choice_player)
                }
            }
        }
    }
}

/// CR 701.21a + CR 614.6: Deliver a post-replacement sacrifice `ZoneChange`
/// through the single zone-pipeline tail (`zone_pipeline::deliver`).
///
/// The event has already cleared the replacement consult, so it is sealed via
/// the `approve_post_replacement` proof token (which preserves its `applied`
/// set and re-validates that it is a ZoneChange). Routing through `deliver`
/// gives a sacrifice redirected to the battlefield the full enter-tapped /
/// enter-with-counters / ETB-counter-static delivery tail instead of a bare
/// `move_to_zone`. The Sacrifice proposal carries no source, so no exile-link
/// context is attributed. Returns `NeedsChoice` (with `state.waiting_for`
/// already set) if the delivery tail itself surfaced a replacement choice.
fn deliver_sacrifice_zone_change(
    state: &mut GameState,
    zone_event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> SacrificeApply {
    let Ok(approved) = ApprovedZoneChange::approve_post_replacement(zone_event) else {
        // Defensive: the inner proposal is always a ZoneChange.
        return SacrificeApply::Complete;
    };
    let ctx = DeliveryCtx {
        source_id: None,
        exile_links: ExileLinkSpec::default(),
        drain: crate::types::game_state::PostReplacementDrainOwner::DeliveryTail,
    };
    match zone_pipeline::deliver(state, approved, ctx, events) {
        ZoneDeliveryResult::Done => SacrificeApply::Complete,
        ZoneDeliveryResult::NeedsChoice(player) => SacrificeApply::NeedsChoice(player),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{StaticDefinition, TargetFilter};
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;

    #[test]
    fn sacrifice_moves_to_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        let result = sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events);
        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    /// CR 701.21a + CR 614.1: a sacrificed permanent's inner graveyard move is
    /// subject to replacement. With a Rest-in-Peace-style "would be put into a
    /// graveyard → exile instead" Moved replacement in play, sacrifice must land
    /// the permanent in exile (not the graveyard), still emit
    /// `PermanentSacrificed`, and still record the sacrifice. This is the general
    /// fix for Disturb back faces and global graveyard hate on sacrifice.
    #[test]
    fn sacrifice_redirected_to_exile_by_moved_replacement() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sacrifice Victim".to_string(),
            Zone::Battlefield,
        );

        // Rest in Peace: "If a card or token would be put into a graveyard from
        // anywhere, exile it instead." Modeled as a Moved (→Graveyard)
        // replacement that redirects to Exile.
        let rip_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut rip = GameObject::new(
            rip_id,
            CardId(888),
            PlayerId(1),
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        rip.replacement_definitions.push(
            crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
                .destination_zone(Zone::Graveyard)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::ChangeZone {
                        actor: crate::types::ability::LibraryInstructionActor::Controller,
                        library_shuffle: Default::default(),
                        destination: Zone::Exile,
                        origin: None,
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
                ))
                .description("Rest in Peace".to_string()),
        );
        state.objects.insert(rip_id, rip);
        state.battlefield.push_back(rip_id);

        let mut events = Vec::new();
        let result = sacrifice_permanent(&mut state, victim, PlayerId(0), &mut events);

        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        assert!(
            state.exile.contains(&victim),
            "the inner graveyard move must re-enter replacement; Moved→Exile sends the victim to exile"
        );
        assert!(
            !state.players[0].graveyard.contains(&victim),
            "redirected sacrifice must not land in the graveyard"
        );
        assert!(
            events.iter().any(|e| matches!(
                e,
                GameEvent::PermanentSacrificed { object_id, .. } if *object_id == victim
            )),
            "PermanentSacrificed must fire regardless of the redirected zone"
        );
        assert_eq!(
            state.sacrificed_permanents_this_turn.len(),
            1,
            "the sacrifice must still be recorded for restriction tracking"
        );
    }

    /// Phase B discriminating test for the sacrifice-redirected-to-battlefield
    /// delivery-tail bug-fix. Before Phase B the inner graveyard move was
    /// delivered with a bare `zones::move_to_zone`, so a sacrifice redirected to
    /// the battlefield (CR 614.6) dropped the CR 614.1c
    /// `EntersWithAdditionalCounters` static (Kalain class). Routing the inner
    /// delivery through `zone_pipeline::deliver` restores the full delivery tail.
    ///
    /// Drives the real production pipeline (`sacrifice_permanent` ->
    /// `replace_event` -> `apply_sacrifice_after_replacement` -> `deliver`) and
    /// asserts the re-entered creature receives the additional +1/+1 counter.
    /// FAILS on the old raw move (0 counters), passes through the tail.
    #[test]
    fn sacrifice_redirected_to_battlefield_applies_enters_with_counters_tail() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ControllerRef, Effect, FilterProp, TypedFilter,
        };
        use crate::types::counter::CounterType;
        use crate::types::replacements::ReplacementEvent;
        use crate::types::statics::StaticMode;

        let mut state = GameState::new_two_player(42);
        // Victim is a creature P0 controls; its own "would die -> return to the
        // battlefield" redirect.
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Resilient Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&victim).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.replacement_definitions.push(
                crate::types::ability::ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Graveyard)
                    .valid_card(TargetFilter::SelfRef)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            actor: crate::types::ability::LibraryInstructionActor::Controller,
                            library_shuffle: Default::default(),
                            destination: Zone::Battlefield,
                            origin: None,
                            target: TargetFilter::SelfRef,
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
                    ))
                    .description("Return to the battlefield instead of dying".to_string()),
            );
        }

        // CR 614.1c: a separate P0 permanent grants "other creatures you control
        // enter with an additional +1/+1 counter".
        let lord_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        let mut lord = GameObject::new(
            lord_id,
            CardId(777),
            PlayerId(0),
            "Counter Lord".to_string(),
            Zone::Battlefield,
        );
        lord.card_types
            .core_types
            .push(crate::types::card_type::CoreType::Enchantment);
        let extra_counter = crate::types::ability::StaticDefinition::new(
            StaticMode::EntersWithAdditionalCounters {
                counter_type: CounterType::Plus1Plus1,
                count: 1,
            },
        )
        .affected(TargetFilter::Typed(
            TypedFilter::creature()
                .controller(ControllerRef::You)
                .properties(vec![FilterProp::Another]),
        ));
        lord.static_definitions.push(extra_counter.clone());
        std::sync::Arc::make_mut(&mut lord.base_static_definitions).push(extra_counter);
        state.objects.insert(lord_id, lord);
        state.battlefield.push_back(lord_id);

        let mut events = Vec::new();
        let result = sacrifice_permanent(&mut state, victim, PlayerId(0), &mut events);

        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        assert_eq!(
            state.objects[&victim].zone,
            Zone::Battlefield,
            "the Moved redirect returns the sacrificed creature to the battlefield"
        );
        assert_eq!(
            state.objects[&victim]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(1),
            "a sacrifice redirected to the battlefield must receive the CR 614.1c \
             enters-with-additional-counter via the full delivery tail"
        );
    }

    #[test]
    fn sacrifice_emits_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();

        sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PermanentSacrificed { object_id, player_id }
                if *object_id == obj_id && *player_id == PlayerId(0)
        )));
    }

    #[test]
    fn sacrifice_artifact_records_restriction() {
        use crate::types::card_type::CoreType;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Artifact);
        let mut events = Vec::new();

        sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events).unwrap();

        // record_sacrifice tracks artifact sacrifices for restriction checking
        assert!(state
            .players_who_sacrificed_artifact_this_turn
            .contains(&PlayerId(0)));
        assert_eq!(state.sacrificed_permanents_this_turn.len(), 1);
        let record = &state.sacrificed_permanents_this_turn[0];
        assert_eq!(record.object_id, obj_id);
        assert_eq!(record.controller, PlayerId(0));
        assert!(record.core_types.contains(&CoreType::Artifact));
    }

    #[test]
    fn cant_be_sacrificed_prevents_sacrifice() {
        // CR 701.21: A permanent with a `CantBeSacrificed` static cannot be sacrificed.
        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sigarda".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&victim)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::Other("CantBeSacrificed".to_string()))
                    .affected(TargetFilter::SelfRef),
            );

        let mut events = Vec::new();
        let result = sacrifice_permanent(&mut state, victim, PlayerId(0), &mut events);

        // CR 118.3: the bar reports `Prohibited` (not a silent no-op) so
        // cost paths fail payment instead of resolving as no-sacrifice.
        assert!(matches!(result, Ok(SacrificeOutcome::Prohibited)));
        // Permanent is still on the battlefield — nothing moved.
        assert!(state.battlefield.contains(&victim));
        assert!(!state.players[0].graveyard.contains(&victim));
        // No PermanentSacrificed event was emitted.
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == victim
        )));
    }

    #[test]
    fn resolution_created_sacrifice_bar_binds_installer_only() {
        use crate::types::ability::{ContinuousModification, Duration};
        use crate::types::game_state::TransientContinuousEffectBindings;

        // CR 109.5: a resolution-created "you can't sacrifice those
        // creatures" (Call for Aid) binds "you" to the installing caster —
        // the installed TCE carries the caster as its `bound_actor`, so only
        // that player is barred. Installed through the same
        // `add_transient_continuous_effect_inner` entry the resolution path
        // (`install_transient`) uses, with the same resolved binding.
        let mut state = GameState::new_two_player(42);
        let victim = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Stolen Goblin".to_string(),
            Zone::Battlefield,
        );
        let installed = state.add_transient_continuous_effect_inner(
            ObjectId(999),
            PlayerId(0),
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: victim },
            vec![ContinuousModification::AddStaticMode {
                mode: StaticMode::Other("CantBeSacrificed".to_string()),
            }],
            None,
            None,
            TransientContinuousEffectBindings::default(),
            Some(PlayerId(0)),
            None,
        );
        assert!(
            installed.is_some(),
            "the installer grant must actually install"
        );

        // The installer is barred: nothing moves, nothing is emitted.
        let mut events = Vec::new();
        let result = sacrifice_permanent(&mut state, victim, PlayerId(0), &mut events);
        assert!(matches!(result, Ok(SacrificeOutcome::Prohibited)));
        assert!(state.battlefield.contains(&victim));
        assert!(events.is_empty());

        // A later controller is not bound by another player's "you": the
        // same object sacrifices normally.
        let mut events = Vec::new();
        let result = sacrifice_permanent(&mut state, victim, PlayerId(1), &mut events);
        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        assert!(!state.battlefield.contains(&victim));
    }

    #[test]
    fn sacrifice_non_battlefield_errors() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Hand,
        );
        let mut events = Vec::new();

        let result = sacrifice_permanent(&mut state, obj_id, PlayerId(0), &mut events);
        assert!(result.is_err());
    }
}
