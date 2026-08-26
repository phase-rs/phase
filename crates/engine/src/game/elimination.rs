use std::collections::HashSet;

use crate::types::events::GameEvent;
use crate::types::game_state::{
    ActiveSearchDecisionAuthority, CollectEvidenceResume, DeferredLifeCostResume, GameState,
    PendingCast, PendingCostMoveResume, WaitingFor,
};
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::match_config::MatchPhase;
use crate::types::player::PlayerId;
use crate::types::resolved_commands::{
    ResolvedPlayerLeaveCommand, ResolvedPlayerLeaveReplayInvariantError,
};
use crate::types::zones::Zone;

use super::players;

/// CR 800.4a: A spell that has been announced but not yet cast can be parked in
/// several replacement-aware cost continuations. Once its controller leaves,
/// none may resume into finalization after the announcement stack entry has
/// been removed.
fn is_abandoned_spell(
    state: &GameState,
    departing_player: PlayerId,
    spell_ids: &[crate::types::identifiers::ObjectId],
    pending: &PendingCast,
) -> bool {
    pending.activation_ability_index.is_none()
        && (spell_ids.contains(&pending.object_id)
            || state
                .objects
                .get(&pending.object_id)
                .is_some_and(|object| object.controller == departing_player))
}

fn abandon_pending_spell_casts(
    state: &mut GameState,
    departing_player: PlayerId,
    spell_ids: &[crate::types::identifiers::ObjectId],
) {
    if state
        .pending_cast
        .as_ref()
        .is_some_and(|pending| is_abandoned_spell(state, departing_player, spell_ids, pending))
    {
        state.pending_cast = None;
    }

    if state
        .waiting_for
        .pending_cast_ref()
        .is_some_and(|pending| is_abandoned_spell(state, departing_player, spell_ids, pending))
    {
        state.waiting_for = WaitingFor::Priority {
            player: state.active_player,
        };
    }

    if matches!(
        state.pending_deferred_life_cost_resume.as_ref(),
        Some(DeferredLifeCostResume::Cast {
            pending: Some(pending),
            ..
        }) if is_abandoned_spell(state, departing_player, spell_ids, pending)
    ) {
        state.pending_deferred_life_cost_resume = None;
    }

    if let Some(resume) = state.pending_cost_move_resume.take() {
        let abandons_spell = match &resume {
            PendingCostMoveResume::Cast {
                pending: Some(pending),
                ..
            }
            | PendingCostMoveResume::SacrificeForCost { pending, .. } => {
                is_abandoned_spell(state, departing_player, spell_ids, pending)
            }
            PendingCostMoveResume::CollectEvidencePayment { resume, .. } => matches!(
                resume.as_ref(),
                CollectEvidenceResume::Casting { pending_cast, .. }
                    if is_abandoned_spell(state, departing_player, spell_ids, pending_cast)
            ),
            PendingCostMoveResume::ActivationMillPayment { pending, .. } => {
                is_abandoned_spell(state, departing_player, spell_ids, pending)
            }
            PendingCostMoveResume::Cast { pending: None, .. }
            | PendingCostMoveResume::WardSacrificePayment { .. }
            | PendingCostMoveResume::ReplacementMayCost { .. }
            | PendingCostMoveResume::Foretell { .. }
            | PendingCostMoveResume::DelveManaPayment { .. }
            | PendingCostMoveResume::UnlessBouncePayment { .. }
            | PendingCostMoveResume::ManaAbilityPayment { .. }
            | PendingCostMoveResume::LoyaltyActivation { .. }
            | PendingCostMoveResume::CounterAdditionUnlessPayment { .. }
            | PendingCostMoveResume::RandomDiscardUnlessPayment(..) => false,
        };
        if !abandons_spell {
            state.pending_cost_move_resume = Some(resume);
        }
    }

    if state
        .pending_discard_for_cost
        .as_ref()
        .is_some_and(|resume| {
            is_abandoned_spell(state, departing_player, spell_ids, &resume.pending)
        })
    {
        state.pending_discard_for_cost = None;
    }
}

/// Eliminate a player from the game per CR 800.4.
///
/// - Marks the player as eliminated
/// - Removes their spells from the stack
/// - Exiles all objects they own (all zones)
/// - Emits PlayerEliminated event
/// - For team-based formats (2HG): also eliminates all teammates
/// - Checks if the game is over (1 or fewer living players/teams remain)
pub fn eliminate_player(state: &mut GameState, player: PlayerId, events: &mut Vec<GameEvent>) {
    eliminate_players_simultaneously(state, &[player], events);
}

/// CR 704.3 + CR 104.4a: Eliminate a set of players who lost in the SAME
/// state-based-action event.
///
/// All eliminations (and, for team formats, their teammate eliminations) are
/// applied BEFORE the single `check_game_over`, so the game-over check observes
/// the true post-event living set. When every remaining player is in the set
/// the result is a draw (`GameOver { winner: None }`) per CR 104.4a, rather than
/// crowning whichever player happened to be processed first. With a single loser
/// this is exactly the previous per-player behavior.
pub fn eliminate_players_simultaneously(
    state: &mut GameState,
    players_to_eliminate: &[PlayerId],
    events: &mut Vec<GameEvent>,
) {
    let mut eliminated_any = false;
    let mut leaving_set = HashSet::new();

    for &player in players_to_eliminate {
        if !players::is_alive(state, player) {
            continue;
        }
        leaving_set.insert(player);
        if super::topology::has_two_headed_giant_shared_resources(state) {
            for teammate in players::teammates(state, player) {
                if players::is_alive(state, teammate) {
                    leaving_set.insert(teammate);
                }
            }
        }
    }

    let interrupted_ordinary_search = state
        .pending_scoped_library_search
        .is_none()
        .then(|| {
            state
                .active_search_decision_controls
                .iter()
                .find(|(_, decision)| {
                    leaving_set.contains(&decision.searched_zone_owner)
                })
                .map(|(&searcher, _)| {
                    let split = match &state.waiting_for {
                        WaitingFor::SearchChoice { player, split, .. } if *player == searcher => {
                            split.clone()
                        }
                        _ => state.pending_search_found_batch.as_ref().and_then(|batch| {
                            if batch.searcher != searcher {
                                return None;
                            }
                            match &batch.continuation {
                                crate::types::game_state::PendingSearchFoundContinuation::Standard {
                                    split,
                                } => split.clone(),
                                crate::types::game_state::PendingSearchFoundContinuation::Scoped => {
                                    None
                                }
                            }
                        }),
                    };
                    (searcher, split)
                })
        })
        .flatten();

    for &player in players_to_eliminate {
        // Skip if already eliminated (e.g. a teammate eliminated alongside an
        // earlier loser in this same batch).
        if !players::is_alive(state, player) {
            continue;
        }

        do_eliminate(state, player, &leaving_set, events);
        eliminated_any = true;

        if super::topology::has_two_headed_giant_shared_resources(state) {
            for teammate in players::teammates(state, player) {
                if players::is_alive(state, teammate) {
                    do_eliminate(state, teammate, &leaving_set, events);
                }
            }
        }
    }

    if !eliminated_any {
        return;
    }

    // CR 800.4a: after ALL owned-exiles, end control effects the leaving players
    // control and exile anything still under a leaver's control. Runs ONCE over
    // the full `leaving_set` — the retain+sweep scope is what makes a co-leaver's
    // steal of a survivor's object revert instead of being over-exiled.
    end_control_effects_for_leaving_players(state, &leaving_set, events);

    // CR 704.3 + CR 104.4a: a SINGLE game-over check after all simultaneous
    // eliminations — so a finish where every remaining player lost at once
    // resolves to a draw (`winner: None`) rather than a spurious winner.
    check_game_over(state, events);

    let game_over_winner = match &state.waiting_for {
        WaitingFor::GameOver { winner } => Some(*winner),
        _ => None,
    };

    // CR 603.3b + CR 800.4a: Always resolve in-flight trigger-ordering work
    // when players leave — including lethal combat damage that ends the game
    // (issue #1350). Previously this ran only when the game continued, leaving
    // `pending_trigger_order` / `deferred_triggers` orphaned on `GameOver`.
    prune_pending_trigger_order(state);
    prune_deferred_triggers_for_eliminated_players(state);

    if let Some(winner) = game_over_winner {
        // Terminal: drop trigger scaffolding the client would otherwise show as
        // a stuck stack / ordering prompt.
        let mut terminal_firings = state
            .pending_trigger_order
            .take()
            .into_iter()
            .flat_map(|order| order.groups)
            .flat_map(|group| group.triggers)
            .map(|context| context.firing())
            .collect::<Vec<_>>();
        terminal_firings.extend(
            std::mem::take(&mut state.deferred_triggers)
                .into_iter()
                .map(|context| context.firing()),
        );
        terminal_firings.extend(state.pending_trigger_firing.take());
        state.pending_trigger = None;
        state.pending_trigger_entry = None;
        state.pending_trigger_event_batch.clear();
        // CR 117.3c: The construction priority recipient is scheduling state for
        // a batch that no longer exists. Leaving it installed would durably
        // serialize a departed player into a terminal `GameOver` snapshot — the
        // exact leak the surrounding comment already calls out for the reused
        // singleton engine. The terminal arm needs no re-point, because there is
        // no later construction to route.
        state.pending_trigger_construction_priority_recipient = None;
        for firing in terminal_firings {
            crate::game::lifecycle::record_delayed_terminal(
                firing,
                crate::game::lifecycle::DelayedTerminalDisposition::Eliminated,
            );
        }
        state.waiting_for = WaitingFor::GameOver { winner };
    } else {
        // CR 603.3b: If prune collapsed an ordering pass into
        // `deferred_triggers` while `waiting_for` is Priority, dispatch now so
        // combat auto-advance does not skip them (issue #1350).
        drain_or_clear_deferred_triggers_after_elimination(state, events);

        // CR 800.4a: If the active `WaitingFor` was waiting on any
        // newly-eliminated player, advance to `Priority` for the next living
        // player so the game does not deadlock waiting on a player who has left.
        // CR 103.5: For simultaneous mulligan states, prune eliminated players
        // from the pending list. If the list becomes empty, advance the flow
        // by emitting MulliganStarted-equivalent transition state.
        prune_mulligan_pending(state, events);

        if let Some((searcher, split)) = interrupted_ordinary_search {
            if state
                .pending_search_found_batch
                .as_ref()
                .is_some_and(|batch| batch.searcher == searcher)
            {
                state.pending_search_found_batch = None;
                if state.pending_replacement.as_ref().is_some_and(|pending| {
                    matches!(
                        pending.proposed,
                        crate::types::proposed_event::ProposedEvent::SearchFound {
                            searcher: pending_searcher,
                            ..
                        } if pending_searcher == searcher
                    )
                }) {
                    state.pending_replacement = None;
                    state.replacement_may_cost_paused = false;
                }
                if state
                    .active_batch_delivery()
                    .and_then(|pending| pending.completion.as_ref())
                    .is_some_and(|completion| {
                        matches!(
                            completion,
                            crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
                                ..
                            }
                        )
                    })
                {
                    state
                        .take_active_batch_delivery()
                        .expect("eliminated search batch must own the active frame");
                }
            }
            if let Err(error) =
                super::engine_resolution_choices::settle_search_after_zone_owner_elimination(
                    state, searcher, split, events,
                )
            {
                debug_assert!(false, "ordinary search elimination resume failed: {error}");
            }
        }

        // CR 800.4a + CR 101.4: if the departing player owned the current
        // scoped-search acceptance/selection prompt, continue the already
        // pruned APNAP cursor instead of replacing it with unrelated priority.
        if let Err(error) =
            super::effects::scoped_library_search::resume_after_elimination(state, events)
        {
            debug_assert!(false, "scoped search elimination resume failed: {error}");
        }

        if let Some(waiting_pid) = state.waiting_for.acting_player() {
            if !players::is_alive(state, waiting_pid) {
                let next = players::next_player(state, waiting_pid);
                state.waiting_for = WaitingFor::Priority { player: next };
            }
        }

        // CR 800.4a: A live trigger-construction batch can carry a priority
        // recipient who is not the prompt's controller, so neither cursor-
        // clearing site above fires when that recipient alone leaves. Priority
        // passes to the next player in turn order who is still in the game
        // (`docs/MagicCompRules.txt:6424`), so the carried recipient is
        // re-pointed rather than stranded — the same authority and remedy the
        // `waiting_for` re-point directly above uses for a dead acting player.
        if let Some(recipient) = state.pending_trigger_construction_priority_recipient {
            if !players::is_alive(state, recipient) {
                state.pending_trigger_construction_priority_recipient =
                    Some(players::next_player_in_turn_order(state, recipient));
            }
        }
    }
}

/// CR 103.5 + CR 800.4a: Prune eliminated players from the in-flight
/// mulligan pending list. If pruning empties it, finish the mulligan flow
/// directly — bottoming is now resolved per-entry at the declare point, so
/// there is no separate batch bottoms phase left to advance to.
fn prune_mulligan_pending(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let alive: HashSet<PlayerId> = state
        .prepaid_mulligan_bottoms
        .keys()
        .copied()
        .filter(|pid| players::is_alive(state, *pid))
        .collect();
    state
        .prepaid_mulligan_bottoms
        .retain(|pid, _| alive.contains(pid));

    match state.waiting_for.clone() {
        WaitingFor::MulliganDecision {
            pending,
            free_first_mulligan,
        } => {
            // CR 800.4a: A pruned player whose entry was mid-`BottomCards
            // { then: UseSerumPowder { object_id } }` needs no special
            // cleanup of `object_id` — that reference lives only inside this
            // `MulliganDecisionEntry`. By the time this function runs,
            // `eliminate_players_simultaneously` has already exiled every
            // object the leaving player owned, including the Serum Powder
            // itself. A plain is_alive-filtered removal of the whole entry
            // is sufficient.
            let alive: Vec<_> = pending
                .into_iter()
                .filter(|e| players::is_alive(state, e.player))
                .collect();
            if alive.is_empty() {
                state.prepaid_mulligan_bottoms.clear();
                state.waiting_for = super::mulligan::finish_mulligans_public(state, events);
            } else {
                state.waiting_for = WaitingFor::MulliganDecision {
                    pending: alive,
                    free_first_mulligan,
                };
            }
        }
        WaitingFor::OpeningHandBottomCards { pending, reason } => {
            let alive: Vec<_> = pending
                .into_iter()
                .filter(|e| players::is_alive(state, e.player))
                .collect();
            if alive.is_empty() {
                state.waiting_for = super::mulligan::enter_normal_mulligan_public(state);
            } else {
                state.waiting_for = WaitingFor::OpeningHandBottomCards {
                    pending: alive,
                    reason,
                };
            }
        }
        _ => {}
    }
}

/// CR 603.3b + CR 800.4a: Resolve an in-flight trigger-ordering pass when one
/// or more players have left the game. Triggers controlled by eliminated
/// players are dropped (CR 800.4a — abilities they would control are removed
/// from the queue / not placed). Groups for eliminated controllers are
/// auto-resolved with the identity order (an eliminated player makes no
/// choices). If the prompted group is the one being resolved, the
/// `WaitingFor::OrderTriggers` prompt is updated to point at the next-most-AP
/// unordered group; if every group becomes ordered, the pending ordering
/// pass is collapsed and the concatenated queue is stashed in
/// `state.deferred_triggers` so the next drain-site picks it up.
fn prune_pending_trigger_order(state: &mut GameState) {
    let living_players: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|player| !player.is_eliminated)
        .map(|player| player.id)
        .collect();
    let Some(order) = state.pending_trigger_order.as_mut() else {
        return;
    };

    let mut terminal_firings = Vec::new();

    // Drop triggers controlled by eliminated players and auto-resolve
    // eliminated controllers' groups with identity order.
    for group in order.groups.iter_mut() {
        if !living_players.contains(&group.controller) {
            // Identity order = current order; just mark as resolved.
            group.ordered = true;
        }
        // CR 800.4a: even within an alive controller's group, drop any
        // triggers whose own controller is now eliminated (delayed-trigger
        // re-attribution corner case — pre-elimination snapshot may have
        // triggers whose `pending.controller` belongs to a now-dead player).
        let triggers = std::mem::take(&mut group.triggers);
        for context in triggers {
            if living_players.contains(&context.pending.controller) {
                group.triggers.push(context);
            } else {
                terminal_firings.push(context.firing());
            }
        }
        if group.triggers.len() <= 1 {
            group.ordered = true;
        }
    }
    // Drop groups whose controller is gone AND whose triggers were all dropped.
    order.groups.retain(|g| !g.triggers.is_empty());

    // If every group is now ordered, collapse the pending pass and stash
    // the concatenated queue into deferred_triggers so the next drain-site
    // (engine_stack, engine_resolution_choices) flushes it onto the stack.
    if order.groups.iter().all(|g| g.ordered) {
        let order = state.pending_trigger_order.take().expect("present above");
        let triggers: Vec<_> = order.groups.into_iter().flat_map(|g| g.triggers).collect();
        state.deferred_triggers.extend(triggers);
        // The waiting_for caller below (`acting_player()` is_alive check) will
        // re-point to a living player's Priority since OrderTriggers no longer
        // matches.
    } else {
        // Some groups still need a choice — refresh the OrderTriggers prompt so
        // it points at the next-most-AP unordered group (possibly the same one
        // if its controller is alive).
        if let Some(wf) = super::triggers::build_next_order_triggers_prompt_public(state) {
            state.waiting_for = wf;
        }
    }

    for firing in terminal_firings {
        crate::game::lifecycle::record_delayed_terminal(
            firing,
            crate::game::lifecycle::DelayedTerminalDisposition::Eliminated,
        );
    }
}

/// CR 800.4a: Remove deferred triggers controlled by eliminated players.
fn prune_deferred_triggers_for_eliminated_players(state: &mut GameState) {
    let deferred = std::mem::take(&mut state.deferred_triggers);
    let mut terminal_firings = Vec::new();
    for context in deferred {
        if state
            .players
            .iter()
            .find(|player| player.id == context.pending.controller)
            .is_some_and(|player| !player.is_eliminated)
        {
            state.deferred_triggers.push(context);
        } else {
            terminal_firings.push(context.firing());
        }
    }
    for firing in terminal_firings {
        crate::game::lifecycle::record_delayed_terminal(
            firing,
            crate::game::lifecycle::DelayedTerminalDisposition::Eliminated,
        );
    }
}

/// CR 603.3b: If prune collapsed an ordering pass into `deferred_triggers`
/// while `waiting_for` is Priority, dispatch now so phase auto-advance does
/// not skip them (issue #1350).
fn drain_or_clear_deferred_triggers_after_elimination(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) {
    if state.deferred_triggers.is_empty()
        || state.pending_trigger.is_some()
        || state.pending_trigger_order.is_some()
    {
        return;
    }
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        if let Some(wf) = super::triggers::drain_deferred_trigger_queue(state, events) {
            state.waiting_for = wf;
        }
    }
}

/// CR 800.4a: Exile every object `player` owns, regardless of zone.
fn exile_owned_objects_on_player_left_game(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    // CR 702.26k: phased-out permanents owned by a leaving player also leave the
    // game; zone_object_ids(Battlefield) filters is_phased_in (targeting.rs:2009),
    // so the battlefield leg iterates state.battlefield UNFILTERED.
    let non_battlefield_zones = [
        Zone::Graveyard,
        Zone::Hand,
        Zone::Library,
        Zone::Exile,
        Zone::Command,
        Zone::Stack,
    ];
    let mut to_exile: Vec<_> = state
        .battlefield
        .iter()
        .copied()
        .chain(
            non_battlefield_zones
                .into_iter()
                .flat_map(|zone| super::targeting::zone_object_ids(state, zone)),
        )
        .filter(|id| state.objects.get(id).is_some_and(|obj| obj.owner == player))
        .collect();
    to_exile.sort_by_key(|id| id.0);
    to_exile.dedup();

    for id in to_exile {
        move_object_for_player_left_game(state, id, player, events);
    }
}

/// CR 800.4a: End every control effect that gives a LEAVING player control of an
/// object, then exile anything still controlled by a leaver. Runs ONCE after all
/// per-player owned-exiles, over the full `leaving_set`, so a co-leaver's steal of
/// a survivor's object reverts symmetrically rather than being over-exiled by the
/// per-player pass.
fn end_control_effects_for_leaving_players(
    state: &mut GameState,
    leaving_set: &HashSet<PlayerId>,
    events: &mut Vec<GameEvent>,
) {
    use crate::types::ability::ContinuousModification;
    use crate::types::identifiers::ObjectId;

    // CR 800.4a: any effect giving a LEAVING player control of an object ends.
    // Prune every single-mod ChangeController TCE controlled by any leaver, over
    // the FULL leaving_set (symmetric with the sweep below), so a co-leaver's
    // steal of a survivor's object reverts rather than being over-exiled.
    state.transient_continuous_effects.retain(|e| {
        !(leaving_set.contains(&e.controller)
            && e.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::ChangeController)))
    });

    // CR 613.1b: recompute layers so control reverts to base_controller/owner for
    // every object whose control TCE was pruned. evaluate_layers is pure (no events).
    super::layers::mark_layers_full(state);
    super::layers::evaluate_layers(state);

    // CR 800.4a: "if there are any objects still controlled by that player, those
    // objects are exiled" — e.g. an object whose base_controller reverted to a
    // leaver ("enters under [leaver]'s control", zones.rs:1172) with no surviving
    // control effect. Sweep only PHASED-IN battlefield objects: evaluate_layers
    // above skips phased-OUT permanents (CR 702.26b — layers.rs:1602/1613-1615
    // only reset controller for phased-in ids), so a survivor-OWNED permanent
    // phased-out while stolen by a leaver still reads obj.controller == leaver
    // after the re-derive. Such a permanent must stay frozen (CR 702.26b) and
    // revert to its owner when it phases back in — it must NOT be exiled here. A
    // leaver-OWNED phased-out permanent is already exiled by step 1 (the CR
    // 702.26k unfiltered owned-exile leg), so restricting to phased-in objects
    // loses no required exile. (step-1-exiled objects are already gone, so no
    // already-exiled id reaches move_object.)
    let mut to_exile: Vec<ObjectId> = state
        .battlefield_phased_in_ids()
        .into_iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|obj| leaving_set.contains(&obj.controller))
        })
        .collect();
    to_exile.sort_by_key(|id| id.0);
    to_exile.dedup();
    for id in to_exile {
        let leaving_controller = state
            .objects
            .get(&id)
            .expect("battlefield sweep retains the selected object")
            .controller;
        move_object_for_player_left_game(state, id, leaving_controller, events);
    }
}

/// CR 800.4a: A shared pending batch owns the original announced move, while
/// the player-leaves-game procedure owns this separate exile. Remove only the
/// undelivered exact member before that exile, and mark the original logical
/// member terminal without manufacturing an original `ZoneChanged` occurrence.
fn move_object_for_player_left_game(
    state: &mut GameState,
    object_id: crate::types::identifiers::ObjectId,
    leaving_player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let identity = state
        .objects
        .get(&object_id)
        .map(ObjectIncarnationRef::from_object);
    if let Some(identity) = identity {
        abandon_pending_zone_change_member_for_player_left(state, identity, leaving_player);
    }
    let req = crate::game::zone_pipeline::ZoneMoveRequest::player_left_game(object_id, Zone::Exile);
    crate::game::zone_pipeline::move_object(state, req, events);
}

/// CR 800.4a: Both paused zone-change carriers own prospective members. A
/// player-left-game exile atomically retires an undelivered exact member from
/// its logical owner, current pause, and tail. Completed original deliveries
/// remain event-time facts, while a shared batch retains and resumes survivors.
fn abandon_pending_zone_change_member_for_player_left(
    state: &mut GameState,
    identity: ObjectIncarnationRef,
    leaving_player: PlayerId,
) {
    let mut canceled_pauses = Vec::new();
    if let Some(pending) = state
        .active_change_zone_frame_mut()
        .and_then(|frame| frame.pending.as_mut())
    {
        pending
            .logical_zone_change_group
            .record_abandoned_by_player_left(identity)
            .expect("pending change-zone iteration owns a coherent logical group");
        pending.remaining.retain(|id| *id != identity.object_id);
        if pending
            .paused_current
            .as_ref()
            .is_some_and(|paused| paused.member == identity && paused.terminal_completion.is_none())
        {
            canceled_pauses.push(
                pending
                    .paused_current
                    .take()
                    .expect("checked paused delivery is present")
                    .expected_event,
            );
        }
    }
    if let Some(pending) = state.active_batch_delivery_mut() {
        pending
            .logical_zone_change_group
            .record_abandoned_by_player_left(identity)
            .expect("pending batch owns a coherent logical group");
        pending.remaining.retain(|id| *id != identity.object_id);
        pending
            .requests
            .retain(|request| request.object_id != identity.object_id);
        if pending
            .paused_current
            .as_ref()
            .is_some_and(|paused| paused.member == identity && paused.terminal_completion.is_none())
        {
            canceled_pauses.push(
                pending
                    .paused_current
                    .take()
                    .expect("checked paused delivery is present")
                    .expected_event,
            );
        }
    }

    for expected_event in canceled_pauses {
        if state
            .pending_replacement
            .as_ref()
            .is_some_and(|pending| pending.proposed == expected_event)
        {
            state.pending_replacement = None;
            state.replacement_may_cost_paused = false;
            super::replacement::abandon_post_replacement_continuation(state);
        }
        // The canceled pause was the sole current resolution operation. Its
        // surviving carrier tail will resume through the ordinary Priority
        // drain; no unrelated continuation is cleared here.
        state.waiting_for = WaitingFor::Priority {
            player: players::next_player(state, leaving_player),
        };
    }
}

/// Perform the actual elimination of a single player (CR 800.4).
fn do_eliminate(
    state: &mut GameState,
    player: PlayerId,
    leaving_set: &HashSet<PlayerId>,
    events: &mut Vec<GameEvent>,
) {
    let planar_handoff =
        crate::game::planechase::prepare_player_left_game_handoff(state, player, leaving_set);

    // CR 733 + CR 800.4: open the leave's own execution node BEFORE the sweep, so
    // every command it settles below — the owned-object exiles, the control
    // reversions — is attributed to the departure rather than to whatever rules
    // work happened to be live when the state-based action fired. The node stays
    // active for the remainder of this function.
    let leave_node = state.begin_player_leave_journal_node();
    let enclosing_node = state.active_rules_execution_node.replace(leave_node);
    let command = ResolvedPlayerLeaveCommand {
        player,
        cause: leave_node,
    };
    apply_resolved_player_leave(state, &command)
        .expect("a living player must satisfy their own departure precondition");
    state
        .resolved_rules_journal
        .record_player_leave(command)
        .expect("resolved player leave must have a live journal cause");

    // CR 800.4a + CR 616.1: Capture the parked replacement chooser before
    // cast-abandonment teardown can replace its prompt with priority.
    let leaving_is_latched_chooser = state.waiting_for.acting_player() == Some(player);

    abandon_source_bound_resolution_prompt(state, player);
    retire_pending_zone_change_contexts_owned_by(state, player);
    abandon_change_zone_family_for_controller(state, player);

    crate::game::planechase::preserve_phenomenon_stack_abilities_for_handoff(state, planar_handoff);

    // CR 800.4a: Remove spells they control from the stack, one at a time
    // through the shared stack-removal authority — the same shape as the
    // scheduled-control release below. A `retain` would drop several entries in
    // one unjournalable mutation; removing by position instead records each
    // entry with the index it occupied at the moment IT was removed, so a replay
    // reproduces both the count and the surviving entries' relative order.
    let mut abandoned_spell_ids = Vec::new();
    while let Some(idx) = state
        .stack
        .iter()
        .position(|entry| entry.controller == player)
    {
        let removed = super::stack::remove_nonresolving_stack_entry_at(
            state,
            idx,
            super::lifecycle::DelayedTerminalDisposition::Eliminated,
        )
        .expect("position yielded a live stack index");
        if matches!(
            removed.entry.kind,
            crate::types::game_state::StackEntryKind::Spell { .. }
        ) {
            abandoned_spell_ids.push(removed.entry.id);
        }
    }
    abandon_pending_spell_casts(state, player, &abandoned_spell_ids);

    // CR 800.4a + CR 800.4b: A control-another-player effect (CR 723, e.g.
    // Mindslaver / Secret of Bloodbending) ends when EITHER party leaves the
    // game — the leaving player's control effects end (CR 800.4a) and a player
    // can't be controlled by someone who has left (CR 800.4b). Drop every
    // scheduled control where the leaving player is the controller or the target,
    // routing each removal through the single release authority. Covers both
    // windows and closes a latent gap that also affected Mindslaver's full-turn
    // control.
    let leaving = super::topology::normalize_shared_turn_recipient(state, player);
    while let Some(idx) = state
        .scheduled_turn_controls
        .iter()
        .position(|scheduled| scheduled.controller == player || scheduled.target_player == leaving)
    {
        super::turn_control::release_control_at(state, idx);
    }
    // CR 800.4b: If the controlled active player just left, `turn_decision_controller`
    // still points at the (living) controller of a now-departed player — stale;
    // clear it so the departed seat isn't piloted by anyone.
    if state.turn_decision_controller.is_some()
        && super::topology::normalize_shared_turn_recipient(state, state.active_player) == leaving
    {
        state.active_full_turn_control = None;
        state.active_combat_phase_control = None;
        super::turn_control::recompute_active_player_control(state);
    }

    // A consent run freezes canonical representatives and submitters. Player
    // elimination changes that topology, so discard the run rather than
    // allowing a stale prompt or Ready state to authorize anyone.
    super::turn_control::invalidate_resolve_all_consent(state);

    // CR 800.4a + CR 800.4b: a departing searcher/zone owner invalidates its
    // live session, while a departing latched controller ends only that
    // controller's decision/knowledge role and falls back to the searcher.
    state.active_library_searches.retain(|searcher, search| {
        if *searcher == player || search.searched_zone_owner() == player {
            return false;
        }
        search.remove_from_audience(player);
        true
    });
    state
        .active_search_decision_controls
        .retain(|searcher, decision| {
            if *searcher == player {
                return false;
            }
            if matches!(
                decision.authority,
                ActiveSearchDecisionAuthority::LatchedController { controller }
                    if controller == player
            ) {
                decision.authority = ActiveSearchDecisionAuthority::SearcherFallback;
            }
            true
        });
    if let Some(pending) = state.pending_scoped_library_search.as_mut() {
        match &mut pending.phase {
            crate::types::game_state::ScopedLibrarySearchPhase::CollectAcceptance {
                remaining_players,
                accepted_players,
                acceptance_authorities,
                current_player,
            } => {
                remaining_players.retain(|participant| *participant != player);
                accepted_players.retain(|participant| *participant != player);
                acceptance_authorities.retain_mut(|(searcher, authority)| {
                    if *searcher == player {
                        return false;
                    }
                    if matches!(
                        *authority,
                        ActiveSearchDecisionAuthority::LatchedController { controller }
                            if controller == player
                    ) {
                        *authority = ActiveSearchDecisionAuthority::SearcherFallback;
                    }
                    true
                });
                if *current_player == Some(player) {
                    *current_player = None;
                }
            }
            crate::types::game_state::ScopedLibrarySearchPhase::CollectSelections {
                prepared_choices,
                next_selection_index,
                current_player,
                selections,
                frozen_dispositions,
                pending_reveals,
            } => {
                let removed_before_cursor = prepared_choices
                    .iter()
                    .take(*next_selection_index)
                    .filter(|choice| choice.player == player)
                    .count();
                prepared_choices.retain(|choice| choice.player != player);
                *next_selection_index = next_selection_index
                    .saturating_sub(removed_before_cursor)
                    .min(prepared_choices.len());
                selections.retain(|(searcher, _)| *searcher != player);
                frozen_dispositions.retain(|frozen| frozen.searcher != player);
                pending_reveals.retain(|(searcher, _)| *searcher != player);
                if *current_player == Some(player) {
                    *current_player = None;
                }
            }
            crate::types::game_state::ScopedLibrarySearchPhase::Delivering { search_keys } => {
                search_keys.retain(|searcher| *searcher != player);
            }
        }
    }
    // CR 800.4a: "all objects … owned by that player leave the game", so a
    // departed seat has no hand left to discard and iterating it can only be a
    // no-op. Drop it from the discard fan-out's not-yet-prompted roster — the
    // same treatment the scoped-library-search roster above already gets.
    //
    // `matching_players` is deliberately NOT pruned, and the reason is PARITY
    // rather than a rule: the un-paused driver computes its reduction domain
    // once at clause entry and never re-derives it, so a paused clause that
    // pruned would answer differently from an identical unpaused one — which is
    // precisely the divergence this repair exists to remove. CR 800.4i is what
    // makes the retained seat well-defined: "the effect uses the last known
    // information about that player before they left the game." The seat's
    // truthful contribution is zero, and dropping it would silently change a
    // `Min` answer.
    //
    // (Deliberately NOT cited: CR 608.2f, which an earlier revision leaned on.
    // Read in full it is about simultaneity and APNAP ORDER — it latches no
    // domain, and both its examples are about ordering. Same class of stretch as
    // the CR 608.2b citation removed from `discard.rs`.)
    //
    // PINNED BY `effects/mod.rs`'s
    // `eliminating_a_seat_prunes_the_paused_roster_but_not_its_reduction_domain`,
    // which lives there to reuse the fan-out fixture. It asserts BOTH halves,
    // so pruning the second list too is a red test rather than a silent change.
    if let Some(fan_out) = state
        .pending_discard_batch
        .as_mut()
        .and_then(|batch| batch.fan_out.as_mut())
    {
        fan_out.remaining_players.retain(|seat| *seat != player);
    }
    if let Some(crate::types::game_state::PendingBatchDeliveries {
        completion:
            Some(crate::types::game_state::BatchCompletion::LibrarySearchDeliverySettled {
                resume:
                    crate::types::game_state::LibrarySearchDeliveryResume::Scoped {
                        search_keys,
                        grants,
                        ..
                    },
            }),
        ..
    }) = state.active_batch_delivery_mut()
    {
        search_keys.retain(|searcher| *searcher != player);
        grants.retain(|(_, grant)| grant.grantee != player && grant.controller != player);
    }

    // CR 800.4a: A paused triggered ability on the stack is "an object on the
    // stack not represented by a card" and ceases to exist when its controller
    // leaves the game. The stack retain above drops that entry, but a trigger
    // paused mid-target-selection (e.g. Lathiel's end-step trigger awaiting
    // `WaitingFor::DistributeAmong`) also leaves a live cursor in
    // `state.pending_trigger` / `pending_trigger_entry` pointing at that now-gone
    // entry. Left dangling, the next surviving player's action drives
    // `begin_pending_trigger_target_selection` (which gates on `pending_trigger`)
    // back into target selection for a dead entry id, panicking in
    // `mutate_pending_trigger_entry`. Clear the cursor only when the entry it
    // tracks is no longer on the stack, mirroring the early
    // `abandon_pending_spell_casts` teardown above.
    if state
        .pending_trigger_entry
        .is_some_and(|entry_id| !state.stack.iter().any(|entry| entry.id == entry_id))
    {
        if let Some(firing) = state.pending_trigger_firing.take() {
            crate::game::lifecycle::record_delayed_terminal(
                firing,
                crate::game::lifecycle::DelayedTerminalDisposition::Eliminated,
            );
        }
        state.pending_trigger_entry = None;
        state.pending_trigger = None;
        state.pending_trigger_event_batch.clear();
        // CR 117.3c: The batch this recipient was scheduled for has ceased with
        // its tracked entry. Clear it with the cursors — unconditionally, not
        // only when the departing player happens to be the recipient — so the
        // next construction cannot consume a stale carrier and mis-route its
        // terminal priority.
        state.pending_trigger_construction_priority_recipient = None;
    }

    // CR 800.4a + CR 616.1 + CR 704.4: Abandon a parked replacement choice this
    // leaving player was answering. A CR 616.1 replacement-order (or optional
    // MayCost / MayCost sub-choice re-park) is held in `state.pending_replacement`
    // and resumed ONLY via `(WaitingFor::ReplacementChoice, ChooseReplacement)`
    // (engine.rs) or that sub-choice's own resolution — both re-enter
    // `continue_replacement`. If the player who must answer leaves the game, the
    // choice is unanswerable: the post-loop reconcile rewrite advances
    // `waiting_for` to `Priority{next}`, and every later `check_state_based_actions`
    // then bails at its `pending_replacement` guard (sba.rs) — freezing all
    // object-destroying SBAs for the rest of the game.
    //
    // Key off the LATCHED chooser identity, not the mutating object graph:
    // `waiting_for.acting_player()` is the affected player for both
    // `ReplacementChoice{player}` (game_state.rs) and a MayCost sub-choice re-park
    // (payer == affected, replacement.rs). The identity was captured before
    // teardown can mutate `waiting_for`, so it remains constant across a
    // simultaneous multi-elimination batch and object-graph-independent.
    // (`ProposedEvent::affected_player` would mis-resolve here: once a co-eliminated
    // lower-id loser has exiled the affected object, its effective controller is
    // reverted to its owner — CR 616.1's owner-fallback is pre-existing and NOT
    // relied upon.) Mirror the `pending_cast` teardown: clear `pending_replacement`
    // (the SBA-gating slot) plus the parked replacement's own tightly-coupled
    // continuation slots (`replacement_may_cost_paused`, `post_replacement_*`,
    // the stack-owned Connive re-entry). The resume drain also touches OTHER resolution
    // slots on a normal answer (e.g. `pending_phase_transition_progress`,
    // `pending_team_draw_step`, `pending_continuation`); those are intentionally
    // NOT cleared here. Stranding some of them is its own PRE-EXISTING soft-lock
    // (PPT gates `auto_advance`; `pending_continuation` gates the deferred-trigger
    // drain) that predates this PR and is NOT the reported regression; repairing
    // them correctly requires resuming the interrupted APNAP queue for the
    // remaining players (not field-nulling), tracked as a separate follow-up. This
    // fix deliberately addresses only the CR 704.4 SBA-freeze introduced by the
    // `pending_replacement` guard.
    // CR 800.4a: Tear down choices and continuations owned by the leaving player.
    // CR 616.1: SearchFound owns an outer per-card batch, and a replacement-
    // selected zone move may own a nested batch completion. The
    // inner pause can be either replacement ordering or another zone-delivery
    // choice, so this teardown is keyed to the batch plus its latched chooser,
    // independently of `pending_replacement`.
    if state.pending_search_found_batch.is_some() && leaving_is_latched_chooser {
        state.pending_search_found_batch = None;
        if state
            .active_batch_delivery()
            .and_then(|pending| pending.completion.as_ref())
            .is_some_and(|completion| {
                matches!(
                    completion,
                    crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery { .. }
                )
            })
        {
            state
                .take_active_batch_delivery()
                .expect("eliminated search batch must own the active frame");
        }
    }
    if state.pending_replacement.is_some() && leaving_is_latched_chooser {
        state.pending_replacement = None;
        state.replacement_may_cost_paused = false;
        super::replacement::abandon_post_replacement_continuation(state);
    }

    // A leaving player gains no life: they are no longer a player in the game, so
    // there is no one for the owed CR 702.15b gain to be applied to. NO CR 800.4
    // SUBPART STATES THIS DIRECTLY — a sweep of 800.4 and 800.4a-800.4p returns no
    // mention of life at all, so this sentence carries no citation on purpose. The
    // nearest analogues are CR 800.4d (a triggered ability that would be controlled
    // by a player who has left the game isn't put on the stack), CR 800.4e (combat
    // damage that would be assigned to a player who has left the game isn't
    // assigned), and CR 614.9 (damage redirected to or from a player who has left
    // the game does nothing). 800.4d is the closest in SHAPE — an owed effect for a
    // departed seat simply does not happen — but it is about triggered abilities,
    // and the other two are about damage; NONE may be cited as authority for life
    // gain. A re-sweep of 800.4 and 800.4a-800.4p confirms the enumerated subparts
    // cover objects, control, creation, combat damage, costs, choices, information,
    // and turns, and none of them life.
    //
    // Drop only THAT seat's owed lifelink gains — the rest of the batch belongs to
    // other controllers and must still land, and the batch itself must still
    // complete so its CR 603.3b triggers fire. Per-entry, mirroring
    // `abandon_pending_spell_casts`, never a blanket null.
    //
    // CR 800.4j: when the seat that left is the ACTIVE player, the turn still
    // continues to its completion, so the batch DOES complete — `auto_advance_once`
    // discharges it through `resume_pending_combat_lifelink` before the CR 800.4
    // turn skip. (An earlier revision of this comment claimed the batch "can no
    // longer complete at all" and that `turns::enter_phase` owned that case; both
    // halves are false now that the discharge exists.)
    if let Some(record) = state.pending_combat_lifelink.as_mut() {
        record.remaining.retain(|gain| gain.controller != player);
    }

    // CR 800.4a: A coupled ETB spell-resolution context can outlive its
    // `pending_replacement` (nested `ContinueZoneDeliveryTail` early-return,
    // engine_replacement.rs), so it is torn down under its OWN controller-keyed
    // guard — cleared only for the LEAVING player's own resolution (mirroring the
    // cast-abandonment controller key above) so a living player's paused resolution
    // survives an opponent's departure.
    if state
        .active_spell_resolution()
        .is_some_and(|psr| psr.controller == player)
    {
        let _ = state.take_active_spell_resolution();
    }

    // CR 800.4a: All objects the player owns leave the game (exiled). Route each
    // through the zone pipeline under the `PlayerLeftGame` exempt cause — "This
    // is not a state-based action", and no replacement effect applies to a
    // player leaving the game, so the consult is skipped while the
    // unconditional primitive guards still run (PLAN §3).
    exile_owned_objects_on_player_left_game(state, player, events);
    crate::game::planechase::finish_player_left_game_handoff(state, planar_handoff, events);

    state.auto_pass.remove(&player);
    state.planar_die_actions_this_turn.remove(&player);

    // CR 725.4: If the monarch leaves the game, the active player becomes the monarch.
    // If the active player is also leaving, the next living player in turn order gets it.
    if state.monarch == Some(player) {
        let any_alive = state
            .players
            .iter()
            .any(|p| !p.is_eliminated && p.id != player);

        if !any_alive {
            state.monarch = None;
        } else {
            // Prefer active player; fall back to next living in turn order.
            let new_monarch =
                if players::is_alive(state, state.active_player) && state.active_player != player {
                    state.active_player
                } else {
                    players::next_player_in_turn_order(state, player)
                };
            state.monarch = Some(new_monarch);
            events.push(GameEvent::MonarchChanged {
                player_id: new_monarch,
            });
        }
    }

    // CR 726.4: If the player who has the initiative leaves the game,
    // the active player takes the initiative. If the active player is
    // also leaving, the next living player in turn order gets it.
    if state.initiative == Some(player) {
        let any_alive = state
            .players
            .iter()
            .any(|p| !p.is_eliminated && p.id != player);

        if !any_alive {
            state.initiative = None;
        } else {
            let new_holder =
                if players::is_alive(state, state.active_player) && state.active_player != player {
                    state.active_player
                } else {
                    players::next_player_in_turn_order(state, player)
                };
            state.initiative = Some(new_holder);
            events.push(GameEvent::InitiativeTaken {
                player_id: new_holder,
            });
            // CR 725.2: "Whenever a player takes the initiative, that player ventures
            // into Undercity." Push as a pending trigger so it goes on the stack.
            let source_id = crate::game::dungeon::dungeon_sentinel_id(new_holder);
            let venture_ability = crate::types::ability::ResolvedAbility::new(
                crate::types::ability::Effect::VentureInto {
                    dungeon: crate::game::dungeon::DungeonId::Undercity,
                },
                vec![],
                source_id,
                new_holder,
            );
            crate::game::triggers::push_pending_trigger_to_stack(
                state,
                crate::game::triggers::PendingTrigger {
                    source_id,
                    controller: new_holder,
                    condition: None,
                    ability: Box::new(venture_ability),
                    timestamp: 0,
                    target_constraints: Vec::new(),
                    distribute: None,
                    trigger_event: Some(GameEvent::InitiativeTaken {
                        player_id: new_holder,
                    }),
                    modal: None,
                    mode_abilities: vec![],
                    description: Some("Take the initiative — venture into Undercity".to_string()),
                    may_trigger_origin: None,
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
                events,
            );
        }
    }

    // CR 800.4a: If the archenemy leaves the game, the Archenemy subsystem ends.
    // The archenemy is unique (CR 904.2a), so there is no reassignment — unlike the
    // planar controller. Scheme cards are owned by the archenemy and are locked to
    // the command zone (CR 314.2), so they are dropped as bookkeeping here rather
    // than routed through the normal owner-leaves zone pipeline.
    if state.archenemy == Some(player) {
        state.archenemy = None;
        state.scheme_deck.clear();
        let scheme_ids: Vec<crate::types::identifiers::ObjectId> = state
            .command_zone
            .iter()
            .copied()
            .filter(|&id| crate::game::archenemy::is_scheme_object(state, id))
            .collect();
        // allow-raw-zone: archenemy teardown removes command-zone-only schemes as their owner leaves (CR 800.4a + CR 904.4).
        state.command_zone.retain(|id| !scheme_ids.contains(id));
    }

    events.push(GameEvent::PlayerEliminated { player_id: player });

    // The leave node covers this sweep only. Restoring the enclosing scope keeps
    // a later, unrelated command from being attributed to the departure.
    state.active_rules_execution_node = enclosing_node;
}

/// CR 800.4 + CR 104.3a: Installs one already-resolved player departure verbatim.
///
/// Deliberately re-runs none of the CR 104 loss conditions that produced the
/// departure: whether this player lost was settled when the command was
/// recorded.
pub fn apply_resolved_player_leave(
    state: &mut GameState,
    command: &ResolvedPlayerLeaveCommand,
) -> Result<(), ResolvedPlayerLeaveReplayInvariantError> {
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == command.player)
        .ok_or(ResolvedPlayerLeaveReplayInvariantError::UnknownPlayer(
            command.player,
        ))?;
    if player.is_eliminated {
        return Err(ResolvedPlayerLeaveReplayInvariantError::AlreadyEliminated(
            command.player,
        ));
    }
    player.is_eliminated = true;
    if !state.eliminated_players.contains(&command.player) {
        state.eliminated_players.push(command.player);
    }
    Ok(())
}

/// CR 800.4a: A paused carrier retains trigger-source contexts from the
/// original simultaneous action. Retire contexts owned by a player who has
/// left before any remaining member resumes; otherwise a pre-pause latch could
/// fire from an object that no longer exists in the game.
fn retire_pending_zone_change_contexts_owned_by(state: &mut GameState, player: PlayerId) {
    if let Some(pending) = state
        .active_change_zone_frame_mut()
        .and_then(|frame| frame.pending.as_mut())
    {
        pending
            .logical_zone_change_group
            .retire_contexts_owned_by(player);
    }
    if let Some(pending) = state.active_batch_delivery_mut() {
        pending
            .logical_zone_change_group
            .retire_contexts_owned_by(player);
    }
}

/// CR 800.4a: A response prompt owned by a player who left cannot retain a
/// resolution context for a later same-ID object. The prompt, its deferred
/// continuation, and the resolution-scoped re-latch form one atomic family.
fn abandon_source_bound_resolution_prompt(state: &mut GameState, player: PlayerId) {
    let abandon = match &state.waiting_for {
        WaitingFor::NamedChoice {
            free_entry: None,
            player: chooser,
            source,
            persist_player,
            ..
        } => {
            *chooser == player
                || *persist_player == Some(player)
                || source
                    .as_ref()
                    .is_some_and(|source| source.prompt.controller == player)
        }
        WaitingFor::OpponentGuess {
            player: guesser,
            source,
            owner,
            ..
        } => {
            *guesser == player
                || source.prompt.controller == player
                || owner
                    .as_ref()
                    .is_some_and(|owner| owner.context.lki.controller == player)
        }
        _ => false,
    };
    if !abandon {
        return;
    }

    let _ = state
        .clear_active_ability_continuation()
        .expect("elimination cannot clear a buried ability continuation");
    crate::game::stack::finish_resolving_stack_entry(
        state,
        crate::game::lifecycle::DelayedTerminalDisposition::Eliminated,
    );
    state.resolution_source_relatch = None;
    state.deferred_entry_events.clear();
    // The prompt and its ability continuation are abandoned, so no realization point will ever be
    // reached for a token battlefield entry parked by this resolution. Leaving the `Option` live
    // would let a later token's park trip the fail-loud overwrite assert, and would let the
    // action-boundary convergence write a CR 400.7 row and run a CR 603.6a trigger pass for a
    // resolution that no longer exists. If the token itself survives the abandonment its entry row
    // is lost — the same loss the `deferred_entry_events.clear()` above already accepts for that
    // entry's trigger replay.
    state.pending_token_battlefield_entry = None;
    state.waiting_for = WaitingFor::Priority {
        player: players::next_player(state, player),
    };
}

/// CR 800.4a: A paused ChangeZone iteration is a single resolving family's
/// owner. Dropping only its prompt would let an unrelated later resume consume
/// the captured source context, so abandonment removes the owner and its tail
/// together. `PendingBatchDeliveries` is intentionally not included: it is a
/// shared per-object batch and retains the existing prune/resume rules.
fn abandon_change_zone_family_for_controller(state: &mut GameState, player: PlayerId) {
    let Some(pending) = state
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
    else {
        return;
    };
    if pending.controller != player {
        return;
    }

    let _ = state
        .take_active_change_zone_frame()
        .expect("elimination cannot consume a buried ChangeZone frame");
    let _ = state
        .clear_active_ability_continuation()
        .expect("elimination cannot clear a buried ability continuation");
    crate::game::stack::finish_resolving_stack_entry(
        state,
        crate::game::lifecycle::DelayedTerminalDisposition::Eliminated,
    );
    state.resolution_source_relatch = None;
    state.deferred_entry_events.clear();
    // Same reasoning as `abandon_source_bound_resolution_prompt`: the owning resolution is gone,
    // so a parked token battlefield entry has no realization point left.
    state.pending_token_battlefield_entry = None;
    state.waiting_for = WaitingFor::Priority {
        player: players::next_player(state, player),
    };
}

/// CR 104.2a: A player wins if all opponents have left. CR 104.3g: A team loses if all members have lost.
///
/// Check if the game should end. Game ends when 1 or fewer living players/teams remain.
fn check_game_over(state: &mut GameState, events: &mut Vec<GameEvent>) {
    if state.match_phase != MatchPhase::InGame
        || matches!(state.waiting_for, WaitingFor::GameOver { .. })
    {
        return;
    }

    let living: Vec<PlayerId> = state
        .players
        .iter()
        .filter(|p| !p.is_eliminated)
        .map(|p| p.id)
        .collect();

    if let Some(archenemy) = super::topology::archenemy(state) {
        let archenemy_alive = living.contains(&archenemy);
        let living_heroes: Vec<PlayerId> = living
            .iter()
            .copied()
            .filter(|&pid| pid != archenemy)
            .collect();
        let winner = if archenemy_alive && living_heroes.is_empty() {
            Some(archenemy)
        } else if !archenemy_alive && !living_heroes.is_empty() {
            living_heroes.first().copied()
        } else if !archenemy_alive && living_heroes.is_empty() {
            None
        } else {
            return;
        };
        events.push(GameEvent::GameOver { winner });
        state.waiting_for = WaitingFor::GameOver { winner };
    } else if super::topology::has_two_headed_giant_shared_resources(state) {
        let mut living_teams = std::collections::BTreeSet::new();
        for &pid in &living {
            living_teams.insert(super::topology::team_dedup_key(state, pid));
        }

        if living_teams.len() <= 1 {
            let winner = if living.len() == 1 {
                Some(living[0])
            } else if living.len() > 1 {
                // Multiple living players on one team — pick the first
                Some(living[0])
            } else {
                None // draw
            };
            events.push(GameEvent::GameOver { winner });
            state.waiting_for = WaitingFor::GameOver { winner };
        }
    } else {
        // Non-team: game over when 0 or 1 living players
        if living.len() <= 1 {
            let winner = living.first().copied();
            events.push(GameEvent::GameOver { winner });
            state.waiting_for = WaitingFor::GameOver { winner };
        }
    }
}

/// Re-establish the CR 104 terminal-state invariant if an outer action path
/// overwrote the `WaitingFor::GameOver` produced by elimination.
pub(super) fn ensure_game_over_if_terminal(state: &mut GameState, events: &mut Vec<GameEvent>) {
    check_game_over(state, events);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        Effect, EffectKind, PostReplacementContinuation, ReplacementDefinition, ReplacementMode,
        ResolvedAbility, TargetRef,
    };
    use crate::types::actions::GameAction;
    use crate::types::counter::CounterType;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        CastingVariant, NamedChoiceSource, NamedChoiceSourceBinding, OpponentGuessOwner,
        OpponentGuessSource, PendingCast, PendingConniveReentry, PendingContinuation,
        PendingReplacement, PendingSpellResolution, PendingZoneChangeDelivery, PromptSourceBinding,
        ResolutionSourceRelatch, StackEntry, StackEntryKind,
    };
    use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef};
    use crate::types::mana::ManaCost;
    use crate::types::proposed_event::{CounterPlacement, ProposedEvent};
    use crate::types::replacements::ReplacementEvent;

    fn setup_two_player() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 1;
        state
    }

    fn setup_three_player() -> GameState {
        let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
        state.turn_number = 1;
        state
    }

    fn setup_2hg() -> GameState {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        state.turn_number = 1;
        state
    }

    fn setup_archenemy() -> GameState {
        let mut state = GameState::new(FormatConfig::archenemy(), 4, 42);
        state.turn_number = 1;
        state
    }

    fn source_context(
        state: &GameState,
        object_id: ObjectId,
    ) -> crate::types::game_state::TriggerSourceContext {
        crate::game::triggers::trigger_source_context_for_latch(
            state,
            state.objects.get(&object_id).expect("test source exists"),
        )
    }

    fn pending_source_bound_continuation(
        state: &GameState,
        source_id: ObjectId,
        controller: PlayerId,
    ) -> PendingContinuation {
        PendingContinuation::new(
            Box::new(ResolvedAbility::new(
                Effect::NoOp,
                Vec::new(),
                source_id,
                controller,
            )),
            state,
        )
    }

    fn pending_search_found_batch(
        searcher: PlayerId,
        object_id: ObjectId,
    ) -> crate::types::game_state::PendingSearchFoundBatch {
        crate::types::game_state::PendingSearchFoundBatch {
            searcher,
            library_owner: Some(searcher),
            remaining: vec![crate::types::identifiers::ObjectIncarnationRef::of(
                object_id, 0,
            )],
            survivors: Vec::new(),
            current: None,
            continuation: crate::types::game_state::PendingSearchFoundContinuation::Standard {
                split: None,
            },
            visibility: crate::types::game_state::SearchFoundVisibility::Private,
        }
    }

    fn pending_search_found_zone_delivery(
        object_id: ObjectId,
    ) -> crate::types::game_state::PendingBatchDeliveries {
        let mut logical_zone_change_group = crate::types::game_state::LogicalZoneChangeGroup::new(
            crate::types::identifiers::LogicalZoneChangeGroupId(1),
            Vec::new(),
        );
        logical_zone_change_group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("test batch owner has explicit pre-delivery authority");
        crate::types::game_state::PendingBatchDeliveries {
            logical_zone_change_group,
            paused_current: None,
            remaining: Vec::new(),
            destination: Zone::Exile,
            source_id: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: Some(
                crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
                    object_id,
                    grant: None,
                },
            ),
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: Vec::new(),
            zone_change_record_start: 0,
            deferred_events: Vec::new(),
        }
    }

    fn pending_replacement_for(expected_event: ProposedEvent) -> PendingReplacement {
        PendingReplacement {
            proposed: expected_event,
            sacrifice_provenance: None,
            candidates: Vec::new(),
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        }
    }

    fn paused_zone_change_delivery(
        state: &GameState,
        object_id: ObjectId,
        source_id: ObjectId,
    ) -> PendingZoneChangeDelivery {
        let object = state
            .objects
            .get(&object_id)
            .expect("test paused member exists");
        PendingZoneChangeDelivery::new(
            ObjectIncarnationRef::from_object(object),
            ProposedEvent::zone_change(object_id, object.zone, Zone::Graveyard, Some(source_id)),
        )
    }

    fn pending_change_zone_iteration(
        group: crate::types::game_state::LogicalZoneChangeGroup,
        paused_current: Option<PendingZoneChangeDelivery>,
        remaining: Vec<ObjectId>,
        source_id: ObjectId,
        controller: PlayerId,
    ) -> crate::types::game_state::PendingChangeZoneIteration {
        crate::types::game_state::PendingChangeZoneIteration {
            logical_zone_change_group: group,
            paused_current,
            remaining,
            source_id,
            controller,
            origin: Some(Zone::Battlefield),
            destination: Zone::Graveyard,
            enter_transformed: false,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            enters_under_player: None,
            enters_attacking: false,
            enter_with_counters: Vec::new(),
            conditional_enter_with_counters: Vec::new(),
            duration: None,
            track_exiled_by_source: false,
            moved_count: None,
            face_down_profile: None,
            library_placement: None,
            enters_modified_if: None,
            enter_attached_to: None,
            effect_kind: EffectKind::ChangeZone,
        }
    }

    /// CR 800.4a: An owner leaving while a heterogeneous batch is parked must
    /// remove only that undelivered exact member. The shared owner retains the
    /// surviving tail, but the original group records no synthetic move for the
    /// abandoned member.
    #[test]
    fn leaving_owner_prunes_only_its_undelivered_pending_batch_member() {
        let mut state = setup_three_player();
        let leaving = create_object(
            &mut state,
            CardId(710),
            PlayerId(1),
            "Leaving batch member".to_string(),
            Zone::Battlefield,
        );
        let surviving = create_object(
            &mut state,
            CardId(711),
            PlayerId(0),
            "Surviving batch member".to_string(),
            Zone::Battlefield,
        );
        let mut group = state.allocate_logical_zone_change_group(&[leaving, surviving]);
        group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("parked batch has its pre-delivery authority");
        group.immediately_before_suppress_triggers.push(
            crate::types::game_state::LatchedSuppressTrigger {
                source_context: source_context(&state, leaving),
                source_filter: crate::types::ability::TargetFilter::Any,
                trigger_source_filter: None,
                events: vec![crate::types::statics::SuppressedTriggerEvent::Dies],
            },
        );
        state.push_batch_delivery(crate::types::game_state::PendingBatchDeliveries {
            logical_zone_change_group: group,
            paused_current: None,
            remaining: vec![leaving, surviving],
            destination: Zone::Graveyard,
            source_id: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: vec![
                crate::types::game_state::PendingBatchZoneMoveRequest {
                    object_id: leaving,
                    destination: Zone::Graveyard,
                    cause: crate::types::game_state::PendingBatchZoneChangeCause::StateBasedAction,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    enter_transformed: false,
                    controller_override: None,
                    enter_with_counters: Vec::new(),
                    face_down_profile: None,
                    chain_referent: crate::types::zones::ChainReferentIntent::Silent,
                    attach_to: None,
                    library_placement: None,
                    exile_duration: None,
                    exile_controller: None,
                    exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
                    replacement_applied: HashSet::new(),
                    face_down_in_exile: false,
                },
                crate::types::game_state::PendingBatchZoneMoveRequest {
                    object_id: surviving,
                    destination: Zone::Graveyard,
                    cause: crate::types::game_state::PendingBatchZoneChangeCause::StateBasedAction,
                    enter_tapped: crate::types::zones::EtbTapState::Unspecified,
                    enters_attacking: false,
                    enter_transformed: false,
                    controller_override: None,
                    enter_with_counters: Vec::new(),
                    face_down_profile: None,
                    chain_referent: crate::types::zones::ChainReferentIntent::Silent,
                    attach_to: None,
                    library_placement: None,
                    exile_duration: None,
                    exile_controller: None,
                    exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
                    replacement_applied: HashSet::new(),
                    face_down_in_exile: false,
                },
            ],
            attempted: vec![leaving, surviving],
            zone_change_record_start: 0,
            deferred_events: Vec::new(),
        });

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        let batch = state
            .active_batch_delivery()
            .expect("the surviving member keeps the shared batch parked");
        assert_eq!(batch.remaining, vec![surviving]);
        assert_eq!(
            batch
                .requests
                .iter()
                .map(|request| request.object_id)
                .collect::<Vec<_>>(),
            vec![surviving]
        );
        assert_eq!(batch.attempted, vec![leaving, surviving]);
        assert!(matches!(
            batch.logical_zone_change_group.terminal_outcomes.as_slice(),
            [
                crate::types::game_state::LogicalZoneChangeTerminalOutcome::AbandonedByPlayerLeft,
                crate::types::game_state::LogicalZoneChangeTerminalOutcome::Pending,
            ]
        ));
        assert!(
            batch
                .logical_zone_change_group
                .immediately_before_suppress_triggers
                .is_empty(),
            "a leaving owner's latched suppression source cannot survive the pause"
        );

        crate::game::zone_pipeline::drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(state.active_batch_delivery().is_none());
        assert_eq!(state.objects[&leaving].zone, Zone::Exile);
        assert_eq!(state.objects[&surviving].zone, Zone::Graveyard);
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        GameEvent::ZoneChanged {
                            object_id,
                            to: Zone::Exile,
                            ..
                        } if *object_id == leaving
                    )
                })
                .count(),
            1,
            "the player-leaves-game exile is the leaving member's only move"
        );
        assert!(!events.iter().any(|event| {
            matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Graveyard,
                    ..
                } if *object_id == leaving
            )
        }));
    }

    /// CR 800.4a + CR 616.1: A leaving owner can be the paused batch member
    /// while a living player owns the replacement prompt and a survivor remains
    /// in the batch tail. The departure cancels only that exact unfinished
    /// delivery; the tail must drain without trying to complete it a second time.
    #[test]
    fn leaving_owner_cancels_paused_batch_member_and_drains_surviving_tail() {
        let mut state = setup_three_player();
        let leaving = create_object(
            &mut state,
            CardId(712),
            PlayerId(1),
            "Paused leaving batch member".to_string(),
            Zone::Battlefield,
        );
        let surviving = create_object(
            &mut state,
            CardId(713),
            PlayerId(0),
            "Surviving batch tail".to_string(),
            Zone::Battlefield,
        );
        let mut group = state.allocate_logical_zone_change_group(&[leaving, surviving]);
        group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("parked batch has its pre-delivery authority");
        let paused = paused_zone_change_delivery(&state, leaving, ObjectId(900));
        state.pending_replacement = Some(pending_replacement_for(paused.expected_event.clone()));
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 1,
            candidates: Vec::new(),
        };
        state.push_batch_delivery(crate::types::game_state::PendingBatchDeliveries {
            logical_zone_change_group: group,
            paused_current: Some(paused),
            remaining: vec![surviving],
            destination: Zone::Graveyard,
            source_id: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: None,
            replacement_applied: HashSet::new(),
            requests: Vec::new(),
            attempted: vec![leaving, surviving],
            zone_change_record_start: 0,
            deferred_events: Vec::new(),
        });

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        let batch = state
            .active_batch_delivery()
            .expect("surviving tail retains its shared batch owner");
        assert!(batch.paused_current.is_none());
        assert_eq!(batch.remaining, vec![surviving]);
        assert!(state.pending_replacement.is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));

        crate::game::zone_pipeline::drain_pending_batch_deliveries(&mut state, &mut events);
        assert!(state.active_batch_delivery().is_none());
        assert_eq!(state.objects[&leaving].zone, Zone::Exile);
        assert_eq!(state.objects[&surviving].zone, Zone::Graveyard);
    }

    /// CR 800.4a + CR 616.1: A living controller's paused ChangeZone family
    /// retains its surviving tail, but must discard an eliminated owner's exact
    /// paused member and coupled replacement before it resumes.
    #[test]
    fn leaving_owner_cancels_paused_change_zone_member_and_drains_surviving_tail() {
        let mut state = setup_three_player();
        let leaving = create_object(
            &mut state,
            CardId(714),
            PlayerId(1),
            "Paused leaving change-zone member".to_string(),
            Zone::Battlefield,
        );
        let surviving = create_object(
            &mut state,
            CardId(715),
            PlayerId(0),
            "Surviving change-zone tail".to_string(),
            Zone::Battlefield,
        );
        let mut group = state.allocate_logical_zone_change_group(&[leaving, surviving]);
        group
            .latch_immediately_before(Vec::new(), Vec::new())
            .expect("parked iteration has its pre-delivery authority");
        let paused = paused_zone_change_delivery(&state, leaving, ObjectId(901));
        state.pending_replacement = Some(pending_replacement_for(paused.expected_event.clone()));
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 1,
            candidates: Vec::new(),
        };
        state.push_change_zone_iteration(pending_change_zone_iteration(
            group,
            Some(paused),
            vec![surviving],
            ObjectId(901),
            PlayerId(0),
        ));

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        let iteration = state
            .active_change_zone_frame()
            .and_then(|frame| frame.pending.as_ref())
            .expect("living controller keeps the change-zone family");
        assert!(iteration.paused_current.is_none());
        assert_eq!(iteration.remaining, vec![surviving]);
        assert!(state.pending_replacement.is_none());
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));

        crate::game::effects::drain_pending_continuation(&mut state, &mut events);
        assert!(state.active_change_zone_frame().is_none());
        assert_eq!(state.objects[&leaving].zone, Zone::Exile);
        assert_eq!(state.objects[&surviving].zone, Zone::Graveyard);
    }

    #[test]
    fn leaving_named_choice_chooser_abandons_the_whole_source_bound_family() {
        let mut state = setup_three_player();
        let source = create_object(
            &mut state,
            CardId(700),
            PlayerId(0),
            "Persisted choice source".to_string(),
            Zone::Battlefield,
        );
        let context = source_context(&state, source);
        state.waiting_for = WaitingFor::NamedChoice {
            free_entry: None,
            player: PlayerId(1),
            choice_type: crate::types::ability::ChoiceType::Labeled {
                options: vec!["chosen".to_string()],
            },
            options: vec!["chosen".to_string()],
            source: Some(NamedChoiceSource::from_trigger_source(
                context,
                NamedChoiceSourceBinding::ExactObjectAndResolution,
            )),
            persist_player: None,
        };
        state.park_ability_continuation(pending_source_bound_continuation(
            &state,
            source,
            PlayerId(0),
        ));
        state.resolution_source_relatch = Some(ResolutionSourceRelatch {
            object_id: source,
            original_stamp: state.objects[&source].incarnation,
            current_incarnation: state.objects[&source].incarnation,
        });

        eliminate_player(&mut state, PlayerId(1), &mut Vec::new());

        assert!(state.active_ability_continuation().is_none());
        assert!(state.resolving_stack_entry.is_none());
        assert!(state.resolution_source_relatch.is_none());
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ));
    }

    #[test]
    fn leaving_persisted_named_choice_player_abandons_the_whole_family() {
        let mut state = setup_three_player();
        state.waiting_for = WaitingFor::NamedChoice {
            free_entry: None,
            player: PlayerId(0),
            choice_type: crate::types::ability::ChoiceType::Labeled {
                options: vec!["chosen".to_string()],
            },
            options: vec!["chosen".to_string()],
            source: None,
            persist_player: Some(PlayerId(1)),
        };
        state.park_ability_continuation(pending_source_bound_continuation(
            &state,
            ObjectId(700),
            PlayerId(0),
        ));

        eliminate_player(&mut state, PlayerId(1), &mut Vec::new());

        assert!(state.active_ability_continuation().is_none());
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(2)
            }
        ));
    }

    #[test]
    fn leaving_opponent_guess_source_owner_abandons_private_authority() {
        let mut state = setup_three_player();
        let source = create_object(
            &mut state,
            CardId(701),
            PlayerId(0),
            "Opponent guess source".to_string(),
            Zone::Battlefield,
        );
        let context = source_context(&state, source);
        state.waiting_for = WaitingFor::OpponentGuess {
            player: PlayerId(1),
            options: vec!["Yes".to_string(), "No".to_string()],
            choice_type: crate::types::ability::ChoiceType::Labeled {
                options: vec!["Yes".to_string(), "No".to_string()],
            },
            source: OpponentGuessSource {
                prompt: PromptSourceBinding::from_trigger_source(&context),
            },
            owner: Some(OpponentGuessOwner {
                context,
                committed_choice: None,
            }),
            proposition_truth: Some(true),
        };
        state.park_ability_continuation(pending_source_bound_continuation(
            &state,
            source,
            PlayerId(0),
        ));

        eliminate_player(&mut state, PlayerId(0), &mut Vec::new());

        assert!(state.active_ability_continuation().is_none());
        assert!(state.resolving_stack_entry.is_none());
        assert!(matches!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn searched_zone_owner_elimination_settles_ordinary_search_as_empty() {
        use crate::types::ability::{
            ControllerRef, QuantityExpr, SearchSelectionConstraint, TargetFilter, TypedFilter,
        };

        let mut state = setup_three_player();
        let candidate = create_object(
            &mut state,
            CardId(90),
            PlayerId(1),
            "Departing library card".to_string(),
            Zone::Library,
        );
        let shuffle = ResolvedAbility::new(
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(900),
            PlayerId(0),
        );
        let search = ResolvedAbility::new(
            Effect::SearchLibrary {
                filter: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: Some(TargetFilter::Typed(
                    TypedFilter::default().controller(ControllerRef::Opponent),
                )),
                selection_constraint: SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::search_library::resolve(&mut state, &search, &mut events)
            .expect("start opponent-library search");
        state.park_ability_continuation(crate::types::game_state::PendingContinuation::new(
            Box::new(shuffle),
            &state,
        ));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::SearchChoice {
                player: PlayerId(0),
                ..
            }
        ));
        events.clear();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(state.active_library_searches.get(&PlayerId(0)).is_none());
        assert!(state.active_search_decision_controls.is_empty());
        assert!(state.active_ability_continuation().is_none());
        assert!(state.pending_search_found_batch.is_none());
        assert!(state.pending_replacement.is_none());
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Shuffle,
                ..
            }
        )));
        assert_ne!(
            state.objects.get(&candidate).map(|object| object.zone),
            Some(Zone::Hand)
        );
    }

    #[test]
    fn searched_zone_owner_elimination_reconciles_prompt_without_hidden_provenance() {
        use crate::types::ability::{
            ControllerRef, QuantityExpr, SearchSelectionConstraint, TargetFilter, TypedFilter,
        };

        for source_zones in [vec![Zone::Graveyard], vec![Zone::Hand, Zone::Graveyard]] {
            let exercise_prompt_independent_reconciliation = source_zones.len() > 1;
            let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
            state.turn_number = 1;
            state.turn_decision_controller = Some(PlayerId(2));
            create_object(
                &mut state,
                CardId(91),
                PlayerId(1),
                "Departing grave candidate".to_string(),
                Zone::Graveyard,
            );
            let search = ResolvedAbility::new(
                Effect::SearchLibrary {
                    filter: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    reveal: false,
                    target_player: Some(TargetFilter::Typed(
                        TypedFilter::default().controller(ControllerRef::Opponent),
                    )),
                    selection_constraint: SearchSelectionConstraint::None,
                    split: None,
                    source_zones,
                },
                vec![TargetRef::Player(PlayerId(1))],
                ObjectId(901),
                PlayerId(0),
            );
            let mut events = Vec::new();
            crate::game::effects::search_library::resolve(&mut state, &search, &mut events)
                .expect("start cross-owner nonlibrary search");
            assert!(state.active_library_searches.is_empty());
            assert_eq!(
                state
                    .active_search_decision_controls
                    .get(&PlayerId(0))
                    .unwrap()
                    .searched_zone_owner,
                PlayerId(1)
            );
            assert!(matches!(
                state
                    .active_search_decision_controls
                    .get(&PlayerId(0))
                    .unwrap()
                    .authority,
                ActiveSearchDecisionAuthority::LatchedController {
                    controller: PlayerId(2)
                }
            ));
            eliminate_player(&mut state, PlayerId(2), &mut events);
            assert!(matches!(
                state
                    .active_search_decision_controls
                    .get(&PlayerId(0))
                    .unwrap()
                    .authority,
                ActiveSearchDecisionAuthority::SearcherFallback
            ));
            if exercise_prompt_independent_reconciliation {
                state.waiting_for = WaitingFor::Priority {
                    player: PlayerId(0),
                };
            }

            eliminate_player(&mut state, PlayerId(1), &mut events);

            assert!(state.active_library_searches.get(&PlayerId(0)).is_none());
            assert!(state.active_search_decision_controls.is_empty());
            assert!(state.pending_search_found_batch.is_none());
            assert!(!matches!(
                state.waiting_for,
                WaitingFor::SearchChoice { .. }
            ));
        }
    }

    #[test]
    fn scoped_searcher_elimination_prunes_paused_delivery_resume_keys() {
        let mut state = setup_three_player();
        let ability = ResolvedAbility::new(
            Effect::Shuffle {
                target: crate::types::ability::TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(902),
            PlayerId(0),
        );
        state.pending_scoped_library_search =
            Some(crate::types::game_state::PendingScopedLibrarySearch {
                ability: Box::new(ability),
                phase: crate::types::game_state::ScopedLibrarySearchPhase::Delivering {
                    search_keys: vec![PlayerId(0), PlayerId(1), PlayerId(2)],
                },
                after_scope: None,
            });
        let mut batch = pending_search_found_zone_delivery(ObjectId(77));
        batch.completion = Some(
            crate::types::game_state::BatchCompletion::LibrarySearchDeliverySettled {
                resume: crate::types::game_state::LibrarySearchDeliveryResume::Scoped {
                    player: PlayerId(0),
                    source_id: ObjectId(902),
                    search_keys: vec![PlayerId(0), PlayerId(1), PlayerId(2)],
                    grants: Vec::new(),
                    after_scope: None,
                },
            },
        );
        state.push_batch_delivery(batch);

        eliminate_player(&mut state, PlayerId(1), &mut Vec::new());

        let phase_keys = match &state.pending_scoped_library_search.as_ref().unwrap().phase {
            crate::types::game_state::ScopedLibrarySearchPhase::Delivering { search_keys } => {
                search_keys
            }
            _ => panic!("delivery phase must remain parked"),
        };
        assert_eq!(phase_keys, &vec![PlayerId(0), PlayerId(2)]);
        let resume_keys = match state
            .active_batch_delivery()
            .and_then(|batch| batch.completion.as_ref())
            .unwrap()
        {
            crate::types::game_state::BatchCompletion::LibrarySearchDeliverySettled {
                resume:
                    crate::types::game_state::LibrarySearchDeliveryResume::Scoped {
                        search_keys, ..
                    },
            } => search_keys,
            _ => panic!("scoped completion must remain parked"),
        };
        assert_eq!(resume_keys, &vec![PlayerId(0), PlayerId(2)]);
    }

    // --- 2-player elimination (immediate GameOver) ---

    #[test]
    fn two_player_elimination_ends_game() {
        let mut state = setup_two_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(state.players[0].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerEliminated {
                player_id: PlayerId(0)
            }
        )));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::GameOver {
                winner: Some(PlayerId(1))
            }
        )));
    }

    // --- 3-player elimination (game continues) ---

    #[test]
    fn three_player_elimination_game_continues() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(state.players[1].is_eliminated);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerEliminated {
                player_id: PlayerId(1)
            }
        )));
        // Game should NOT be over — 2 players still alive
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn three_player_two_eliminations_ends_game() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);
        eliminate_player(&mut state, PlayerId(2), &mut events);

        // Now only P0 remains — game over
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    // --- Simultaneous loss / draw (CR 104.4a + CR 704.3) ---

    #[test]
    fn simultaneous_two_player_loss_is_a_draw() {
        // CR 104.4a + CR 704.3: when all remaining players lose in a single SBA
        // event, the game is a DRAW (winner: None) — NOT a win for whichever
        // player happened to be processed first.
        let mut state = setup_two_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(&mut state, &[PlayerId(0), PlayerId(1)], &mut events);

        assert!(
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            "simultaneous loss of all players must be a draw, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn simultaneous_single_loss_has_sole_winner() {
        // Only one player loses → the other wins (single-loser behavior preserved).
        let mut state = setup_two_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(&mut state, &[PlayerId(1)], &mut events);

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "a single loser leaves the other player as sole winner, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn three_player_two_simultaneous_losses_leave_sole_winner() {
        // Two of three players die together; the lone survivor wins (not a draw).
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(&mut state, &[PlayerId(1), PlayerId(2)], &mut events);

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "two simultaneous losses with one survivor → that survivor wins, got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn three_player_all_simultaneous_losses_is_a_draw() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_players_simultaneously(
            &mut state,
            &[PlayerId(0), PlayerId(1), PlayerId(2)],
            &mut events,
        );

        assert!(
            matches!(state.waiting_for, WaitingFor::GameOver { winner: None }),
            "all players losing simultaneously is a draw, got {:?}",
            state.waiting_for
        );
    }

    // --- Elimination cleanup ---

    #[test]
    fn elimination_removes_spells_from_stack() {
        let mut state = setup_two_player();
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Stack,
        );
        state.stack.push_back(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(1),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(state.stack.is_empty());
    }

    #[test]
    fn elimination_exiles_owned_permanents() {
        let mut state = setup_three_player();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        // Permanent should be exiled, not on battlefield
        assert!(!state.battlefield.contains(&id));
        assert!(state.exile.contains(&id));
    }

    #[test]
    fn elimination_exiles_owned_graveyard_and_library_cards() {
        let mut state = setup_three_player();
        let graveyard_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Graveyard Bear".to_string(),
            Zone::Graveyard,
        );
        let library_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Library Bear".to_string(),
            Zone::Library,
        );

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(
            !state.players[1].graveyard.contains(&graveyard_id),
            "eliminated player's graveyard cards must leave the game (CR 800.4a)"
        );
        assert!(
            !state.players[1].library.contains(&library_id),
            "eliminated player's library cards must leave the game (CR 800.4a)"
        );
        assert!(state.exile.contains(&graveyard_id));
        assert!(state.exile.contains(&library_id));
    }

    /// Build a mid-cast spell (on the stack, awaiting payment) controlled by
    /// `caster` and stash it in `state.pending_cast`, mirroring the engine state
    /// during `WaitingFor::ManaPayment` (e.g. a convoke spell awaiting taps).
    fn stash_pending_cast(state: &mut GameState, caster: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(99),
            caster,
            "Convoke Spell".to_string(),
            Zone::Stack,
        );
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            obj.controller = caster;
        }
        let ability = ResolvedAbility::new(
            Effect::Unimplemented {
                name: "test".to_string(),
                description: None,
            },
            vec![],
            obj_id,
            caster,
        );
        state.pending_cast = Some(Box::new(PendingCast::new(
            obj_id,
            CardId(99),
            ability,
            ManaCost::NoCost,
        )));
        obj_id
    }

    // --- CR 800.4a: abandon the leaving player's in-progress cast ---

    #[test]
    fn elimination_abandons_leaving_players_pending_cast() {
        // Repro: conceding mid-convoke (WaitingFor::ManaPayment) must not strand
        // the in-progress cast in the (singleton) GameState, where it would
        // resurface as a stuck mana-payment window in a later game.
        let mut state = setup_three_player();
        stash_pending_cast(&mut state, PlayerId(1));
        assert!(state.pending_cast.is_some());

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(
            state.pending_cast.is_none(),
            "the leaving player's mid-cast must be abandoned"
        );
    }

    #[test]
    fn elimination_preserves_other_players_pending_cast() {
        // A living player's mid-cast must survive an opponent's departure —
        // pending_cast is keyed off the spell's controller, not cleared blindly.
        let mut state = setup_three_player();
        stash_pending_cast(&mut state, PlayerId(0));

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(
            state.pending_cast.is_some(),
            "an opponent leaving must not abandon the caster's in-progress spell"
        );
    }

    #[test]
    fn elimination_abandons_deferred_life_cast_and_preserves_living_replacement() {
        // CR 104.3a + CR 800.4a: a player may concede during a paused life-cost
        // continuation. The announcement leaves the stack at departure, so the
        // deferred cast must be retired rather than resumed into finalization.
        let mut state = setup_three_player();
        let discarded = create_object(
            &mut state,
            CardId(101),
            PlayerId(2),
            "Replacement-prompt discard".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&discarded)
            .expect("discarded card exists")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Discard)
                    .valid_card(crate::types::ability::TargetFilter::SelfRef)
                    .mode(ReplacementMode::Optional { decline: None }),
            );
        let mut setup_events = Vec::new();
        assert!(matches!(
            crate::game::replacement::replace_event(
                &mut state,
                ProposedEvent::Discard {
                    player_id: PlayerId(2),
                    object_id: discarded,
                    source_id: None,
                    caused_by_effect: false,
                    discard_frame: None,
                    applied: HashSet::new(),
                },
                &mut setup_events,
            ),
            crate::game::replacement::ReplacementResult::NeedsChoice(PlayerId(2))
        ));
        crate::game::replacement::park_waiting_for(&mut state, PlayerId(2));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(2),
                ..
            }
        ));

        let leaving_spell = stash_pending_cast(&mut state, PlayerId(1));
        let leaving_pending = state.pending_cast.take().expect("test cast exists");
        state.stack.push_back(StackEntry {
            id: leaving_spell,
            source_id: leaving_spell,
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        state.pending_deferred_life_cost_resume = Some(DeferredLifeCostResume::Cast {
            player: PlayerId(1),
            pending: Some(leaving_pending),
            remaining_life_payments: vec![],
            resume_at_resolution_depth: 0,
        });
        let result = super::super::engine::apply_as_current(
            &mut state,
            GameAction::Concede {
                player_id: PlayerId(1),
            },
        )
        .expect("a player may concede during a paused cast");

        assert!(
            state.pending_deferred_life_cost_resume.is_none(),
            "a departed caster's deferred life-payment continuation must not resume"
        );
        assert!(
            !state.stack.iter().any(|entry| entry.id == leaving_spell),
            "the departed caster's announced spell leaves the stack"
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(2),
                ..
            }
        ));
        let resolved = super::super::engine::apply_as_current(
            &mut state,
            GameAction::ChooseReplacement { index: 1 },
        )
        .expect("the living player's replacement choice must not resume the abandoned cast");
        assert!(
            !resolved
                .events
                .iter()
                .any(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == leaving_spell)),
            "answering the living player's replacement choice must not cast the departed player's spell"
        );
    }

    #[test]
    fn simultaneous_elimination_clears_object_referential_replacement_for_eliminated_chooser() {
        // CR 800.4a + CR 616.1: 4-player FFA so two simultaneous losses leave the
        // game running (exercises the reconcile rewrite that strands the choice).
        let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
        state.turn_number = 1;

        // O: OWNED by X = P1, CONTROLLED by chooser C = P2. X.0 (1) < C.0 (2), so
        // do_eliminate(X) runs first and reverts O's effective controller to its
        // OWNER (P1) on exile (zones.rs revert_layered_characteristics_to_base) --
        // by the time do_eliminate(C) runs, `affected_player(O) == P1 != C`, so the
        // OLD live key would SKIP the clear. This is the revert-failing wedge.
        let o = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Contested".into(),
            Zone::Battlefield,
        );
        if let Some(obj) = state.objects.get_mut(&o) {
            obj.controller = PlayerId(2);
            obj.base_controller = Some(PlayerId(2));
        }

        // Parked OBJECT-REFERENTIAL replacement: affected_player reads O's controller.
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::AddCounter {
                placement: CounterPlacement::Object {
                    actor: PlayerId(2),
                    object_id: o,
                    counter_type: CounterType::Plus1Plus1,
                },
                count: 1,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: Vec::new(),
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        // Latched chooser identity — the fix's key. C = P2.
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(2),
            candidate_count: 1,
            candidates: vec![],
        };
        // Coupled continuation slots the resume drain would clear on a normal answer.
        state.replacement_may_cost_paused = true;
        state.install_ready_continuation(PostReplacementContinuation::Resolved(Box::new(
            ResolvedAbility::new(
                Effect::Unimplemented {
                    name: "psrc".into(),
                    description: None,
                },
                vec![],
                o,
                PlayerId(2),
            ),
        )));
        state
            .active_post_replacement_drains_mut()
            .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
            .expect("a drain must be resident")
            .source = Some(o);
        state
            .active_post_replacement_drains_mut()
            .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
            .expect("a drain must be resident")
            .event_source = Some(o);
        state
            .active_post_replacement_drains_mut()
            .and_then(crate::types::game_state::PostReplacementDrainStack::resident_mut)
            .expect("a drain must be resident")
            .event_target = Some(TargetRef::Object(o));
        // Issue #4886 (review #6): a live Jinnie Fay-class token-choice applied
        // seed, owned by this same abandoned continuation, must be abandoned
        // alongside its siblings — this field was added after the teardown
        // block below was written and was missed until this regression.
        state.post_replacement_token_choice_applied = Some(HashSet::from([
            crate::types::proposed_event::AppliedReplacementKey::object(o, 0),
        ]));
        // Make the real atomic paused-drain/draw pair.
        // The dispatch handle proves that the parent is Paused, rather than the
        // Ready-parent approximation that cannot exercise child-before-parent
        // abandonment.
        let (_, dispatch) = state
            .active_post_replacement_drains_mut()
            .and_then(crate::types::game_state::PostReplacementDrainStack::begin_dispatch)
            .expect("the resident continuation begins its exact dispatch");
        assert!(state
            .active_post_replacement_drains_mut()
            .expect("the dispatch parent remains resident")
            .pause_dispatch(dispatch));
        let leaving_frame = state.push_draw_sequence_with_origin(
            PlayerId(2),
            1,
            HashSet::new(),
            crate::types::game_state::DrawSequenceOrigin::Plain,
        );
        state
            .resolution_stack
            .validate(&state.waiting_for)
            .expect("the paused parent and active child form the shipped pair");
        let mut events = Vec::new();
        // Real path: X (P1) and C (P2) leave in the SAME simultaneous SBA event
        // (losers sorted by id -> [P1, P2] -> do_eliminate(P1) then do_eliminate(P2)).
        eliminate_players_simultaneously(&mut state, &[PlayerId(1), PlayerId(2)], &mut events);

        assert!(state.players[1].is_eliminated && state.players[2].is_eliminated);
        // Gap 1 core (revert-failing vs the affected_player key): the parked choice
        // of the eliminated chooser is cleared even though a lower-id co-loser
        // already exiled the affected object.
        assert!(
            state.pending_replacement.is_none(),
            "eliminating the parked chooser must clear pending_replacement (latched acting_player key, not affected_player)"
        );
        // Every coupled continuation slot the resume drain owns is torn down.
        assert!(!state.replacement_may_cost_paused);
        assert!(!state.has_post_replacement_drain());
        assert!(state.post_replacement_source().is_none());
        assert!(state.post_replacement_event_source().is_none());
        assert!(state.post_replacement_event_target().is_none());
        assert!(
            state.post_replacement_token_choice_applied.is_none(),
            "abandoning the parked chooser's continuation must also clear the token-choice \
             applied seed, not just its established siblings (issue #4886, review #6)"
        );
        assert!(
            state.active_draw_sequence().is_none(),
            "CR 121.2: the leaving chooser's paused draw instruction must be \
             cleared via abandon_post_replacement_continuation, not stranded"
        );
        assert!(
            state.resolution_stack.is_empty(),
            "the active child must be abandoned before its paused parent can retire"
        );
        let later_frame = state.push_draw_sequence_with_origin(
            PlayerId(0),
            1,
            HashSet::new(),
            crate::types::game_state::DrawSequenceOrigin::Plain,
        );
        assert!(
            later_frame > leaving_frame,
            "abandoning the paired child must not rewind the draw-frame allocator"
        );
    }

    #[test]
    fn elimination_clears_active_connive_reentry_for_leaving_chooser() {
        let mut state = setup_three_player();
        let conniver = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Conniver".into(),
            Zone::Battlefield,
        );
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: Vec::new(),
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 1,
            candidates: Vec::new(),
        };
        state.push_connive_reentry(PendingConniveReentry {
            conniver: state
                .capture_connive_subject(conniver)
                .expect("fixture conniver exists"),
            count: 1,
            applied: HashSet::new(),
        });

        eliminate_player(&mut state, PlayerId(0), &mut Vec::new());

        assert!(state.active_connive_reentry().is_none());
    }

    #[test]
    fn elimination_clears_search_found_batch_with_nested_zone_completion() {
        let mut state = setup_three_player();
        let found = create_object(
            &mut state,
            CardId(8),
            PlayerId(0),
            "Found card".into(),
            Zone::Library,
        );
        state.pending_search_found_batch = Some(pending_search_found_batch(PlayerId(0), found));
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::SearchFound {
                searcher: PlayerId(0),
                library_owner: Some(PlayerId(0)),
                object_id: found,
                disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: Vec::new(),
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 1,
            candidates: Vec::new(),
        };
        state.push_batch_delivery(pending_search_found_zone_delivery(found));
        assert!(state.active_batch_delivery().is_some());

        eliminate_player(&mut state, PlayerId(0), &mut Vec::new());

        assert!(state.pending_replacement.is_none());
        assert!(state.pending_search_found_batch.is_none());
        assert!(state.active_batch_delivery().is_none());
    }

    #[test]
    fn opponent_leaving_preserves_living_choosers_search_found_replacement() {
        // CR 800.4a affects only the leaving player: a DIFFERENT player's departure
        // must NOT clear the living chooser's parked replacement (no over-clear).
        let mut state = setup_three_player();

        // Chooser C = P0 (survivor). Player-keyed parked Draw.
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: Vec::new(),
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 1,
            candidates: vec![],
        };
        let parked_found = ObjectId(77);
        state.pending_search_found_batch =
            Some(pending_search_found_batch(PlayerId(0), parked_found));
        state.push_batch_delivery(pending_search_found_zone_delivery(parked_found));

        let mut events = Vec::new();
        eliminate_players_simultaneously(&mut state, &[PlayerId(1)], &mut events);

        assert!(state.players[1].is_eliminated);
        assert!(!state.players[0].is_eliminated);
        assert!(
            state.pending_replacement.is_some(),
            "an opponent leaving must not clear the living chooser's parked replacement"
        );
        assert!(
            state.pending_search_found_batch.is_some(),
            "an opponent leaving must not clear the living chooser's outer found-card batch"
        );
        assert!(
            state.active_batch_delivery().is_some_and(|pending| {
                matches!(
                    pending.completion,
                    Some(
                        crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
                            object_id,
                            grant: None,
                        }
                    ) if object_id == parked_found
                )
            }),
            "an opponent leaving must preserve the living chooser's nested found-card completion"
        );
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::ReplacementChoice {
                    player: PlayerId(0),
                    ..
                }
            ),
            "the living chooser's ReplacementChoice park must be preserved"
        );
    }

    #[test]
    fn opponent_leaving_preserves_living_choosers_draw_replacement() {
        let mut state = setup_three_player();
        state.pending_replacement = Some(PendingReplacement {
            proposed: ProposedEvent::Draw {
                player_id: PlayerId(0),
                count: 1,
                applied: HashSet::new(),
            },
            sacrifice_provenance: None,
            candidates: Vec::new(),
            search_found_candidates: Vec::new(),
            depth: 0,
            is_optional: false,
            library_placement: None,
            exile_controller: None,
            exile_duration: None,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            excess_recipient: None,
            lifelink_bonus: 0,
            may_cost_paid: false,
            may_cost_remaining: None,
        });
        state.waiting_for = WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 1,
            candidates: Vec::new(),
        };
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Paused draw replacement".into(),
            Zone::Battlefield,
        );
        state.install_ready_continuation(PostReplacementContinuation::Resolved(Box::new(
            ResolvedAbility::new(Effect::NoOp, Vec::new(), source, PlayerId(0)),
        )));
        let (_, dispatch) = state
            .active_post_replacement_drains_mut()
            .and_then(crate::types::game_state::PostReplacementDrainStack::begin_dispatch)
            .expect("the resident continuation begins its exact dispatch");
        assert!(state
            .active_post_replacement_drains_mut()
            .expect("the dispatch parent remains resident")
            .pause_dispatch(dispatch));
        let living_frame = state.push_draw_sequence_with_origin(
            PlayerId(0),
            2,
            HashSet::new(),
            crate::types::game_state::DrawSequenceOrigin::Plain,
        );
        state
            .draw_sequence_frame_mut(living_frame)
            .expect("the frame just pushed is active")
            .accumulated = 1;
        state
            .resolution_stack
            .validate(&state.waiting_for)
            .expect("the living chooser owns a valid paused parent/draw pair");
        let paired_stack_before = serde_json::to_value(&state.resolution_stack)
            .expect("the paired resolution stack serializes");

        eliminate_players_simultaneously(&mut state, &[PlayerId(1)], &mut Vec::new());

        assert!(state.pending_replacement.is_some());
        let survivor = state
            .active_draw_sequence()
            .expect("an opponent leaving must not clear the living chooser's paused instruction");
        assert_eq!(
            (survivor.player, survivor.remaining, survivor.accumulated),
            (PlayerId(0), 2, 1),
            "the living chooser's paused draw instruction must survive intact — owed units and \
             already-delivered count both preserved"
        );
        assert_eq!(
            serde_json::to_value(&state.resolution_stack)
                .expect("the surviving paired resolution stack serializes"),
            paired_stack_before,
            "an unrelated departure must preserve the paired parent, child, status, refs, and allocator byte-for-byte"
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
    }

    #[test]
    fn elimination_clears_only_the_leaving_players_active_spell_resolution() {
        let mut state = setup_three_player();
        let spell = create_object(
            &mut state,
            CardId(7),
            PlayerId(0),
            "Paused permanent".into(),
            Zone::Stack,
        );
        state.push_spell_resolution(PendingSpellResolution {
            object_id: spell,
            controller: PlayerId(0),
            casting_variant: CastingVariant::Normal,
            cast_from_zone: None,
            cast_controller: None,
            cast_timing_permission: None,
            spell_targets: vec![],
            actual_mana_spent: 0,
            kickers_paid: vec![],
            additional_cost_payment_count: 0,
            additional_cost_payments: vec![],
            convoked_creatures: vec![],
        });

        eliminate_player(&mut state, PlayerId(1), &mut Vec::new());
        assert!(
            state.active_spell_resolution().is_some(),
            "an opponent leaving must not tear down the living player's active spell frame"
        );

        eliminate_player(&mut state, PlayerId(0), &mut Vec::new());
        assert!(
            state.active_spell_resolution().is_none(),
            "the leaving controller's active spell frame must be torn down"
        );
    }

    #[test]
    fn elimination_skips_already_eliminated_player() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);
        let event_count = events.len();

        // Try to eliminate again
        eliminate_player(&mut state, PlayerId(1), &mut events);

        // No new events should be emitted
        assert_eq!(events.len(), event_count);
    }

    // --- Simultaneous elimination ---

    #[test]
    fn simultaneous_elimination_multiple_players() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        // Eliminate P1 and P2 simultaneously
        eliminate_player(&mut state, PlayerId(1), &mut events);
        // After P1 eliminated, game still goes (P0 and P2 alive)
        // Now eliminate P2
        eliminate_player(&mut state, PlayerId(2), &mut events);

        assert!(state.players[1].is_eliminated);
        assert!(state.players[2].is_eliminated);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn archenemy_hero_loss_eliminates_only_that_hero() {
        let mut state = setup_archenemy();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(state.players[1].is_eliminated);
        assert!(!state.players[2].is_eliminated);
        assert!(!state.players[3].is_eliminated);
        assert!(!matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn archenemy_wins_after_all_heroes_are_eliminated() {
        let mut state = setup_archenemy();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);
        eliminate_player(&mut state, PlayerId(2), &mut events);
        eliminate_player(&mut state, PlayerId(3), &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn archenemy_loss_uses_persistent_topology_after_runtime_state_cleared() {
        let mut state = setup_archenemy();
        state.archenemy = None;
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
    }

    // --- 2HG team elimination ---

    #[test]
    fn two_hg_eliminating_one_teammate_eliminates_both() {
        let mut state = setup_2hg();
        let mut events = Vec::new();

        // Eliminate P0 (team A)
        eliminate_player(&mut state, PlayerId(0), &mut events);

        // Both P0 and P1 (team A) should be eliminated
        assert!(state.players[0].is_eliminated);
        assert!(state.players[1].is_eliminated);

        // Team B wins
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver { winner: Some(_) }
        ));
    }

    #[test]
    fn two_hg_team_b_elimination() {
        let mut state = setup_2hg();
        let mut events = Vec::new();

        // Eliminate P2 (team B)
        eliminate_player(&mut state, PlayerId(2), &mut events);

        // Both P2 and P3 (team B) should be eliminated
        assert!(state.players[2].is_eliminated);
        assert!(state.players[3].is_eliminated);

        // Team A wins (P0 is first living player)
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(0))
            }
        ));
    }

    #[test]
    fn eliminated_player_added_to_eliminated_list() {
        let mut state = setup_three_player();
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(state.eliminated_players.contains(&PlayerId(1)));
    }

    // --- Monarch transfer on elimination (CR 725.4) ---

    #[test]
    fn monarch_transfers_to_next_turn_order_player_when_active_leaving_and_reversed() {
        let mut state = setup_three_player();
        state.active_player = PlayerId(0);
        state.turn_direction = crate::types::phase::TurnDirection::Reversed;
        state.monarch = Some(PlayerId(0));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        // CR 725.4 + CR 103.1: active player is leaving, so reversed turn
        // order gives the monarch designation to P2, not physical-next P1.
        assert_eq!(state.monarch, Some(PlayerId(2)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::MonarchChanged {
                player_id: PlayerId(2)
            }
        )));
    }

    // --- Initiative transfer on elimination (CR 726.4) ---

    #[test]
    fn initiative_transfers_on_elimination() {
        let mut state = setup_three_player();
        state.active_player = PlayerId(0);
        state.initiative = Some(PlayerId(1));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(1), &mut events);

        // CR 726.4: Active player (P0) takes the initiative.
        assert_eq!(state.initiative, Some(PlayerId(0)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::InitiativeTaken {
                player_id: PlayerId(0)
            }
        )));
        // CR 725.2: Venture into Undercity should be on the stack.
        assert!(
            !state.stack.is_empty(),
            "venture trigger should be pushed to stack"
        );
    }

    #[test]
    fn initiative_transfers_to_next_when_active_leaving() {
        let mut state = setup_three_player();
        state.active_player = PlayerId(0);
        state.initiative = Some(PlayerId(0));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        // CR 726.4: Active player is leaving, so next living player in turn order gets it.
        // P1 is next after P0 in a 3-player game.
        assert_eq!(state.initiative, Some(PlayerId(1)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::InitiativeTaken {
                player_id: PlayerId(1)
            }
        )));
    }

    #[test]
    fn initiative_transfers_in_two_player_game() {
        let mut state = setup_two_player();
        state.active_player = PlayerId(0);
        state.initiative = Some(PlayerId(0));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        // CR 726.4: P1 is still alive, so they get initiative (game ends immediately after).
        assert_eq!(state.initiative, Some(PlayerId(1)));
        assert!(matches!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        ));
    }

    #[test]
    fn initiative_transfers_to_next_turn_order_player_when_active_leaving_and_reversed() {
        let mut state = setup_three_player();
        state.active_player = PlayerId(0);
        state.turn_direction = crate::types::phase::TurnDirection::Reversed;
        state.initiative = Some(PlayerId(0));
        let mut events = Vec::new();

        eliminate_player(&mut state, PlayerId(0), &mut events);

        // CR 726.4 + CR 103.1: active player is leaving, so reversed turn
        // order gives the initiative to P2, not physical-next P1.
        assert_eq!(state.initiative, Some(PlayerId(2)));
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::InitiativeTaken {
                player_id: PlayerId(2)
            }
        )));
    }

    // --- CR 800.4a: control effects end when a player leaves the game ---

    use crate::types::ability::{ContinuousModification, Duration, TargetFilter};

    fn setup_four_player() -> GameState {
        let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
        state.turn_number = 1;
        state
    }

    /// Create a battlefield object owned by `owner` and give `controller` control
    /// of it via a real ChangeController TCE (mirrors gain_control.rs). Evaluates
    /// layers so `obj.controller` reflects the effect. Returns the object id.
    fn create_controlled_object(
        state: &mut GameState,
        owner: PlayerId,
        controller: PlayerId,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            owner,
            "Stolen Bear".to_string(),
            Zone::Battlefield,
        );
        state.add_transient_continuous_effect(
            id,
            controller,
            Duration::Permanent,
            TargetFilter::SpecificObject { id },
            vec![ContinuousModification::ChangeController],
            None,
        );
        super::super::layers::mark_layers_full(state);
        super::super::layers::evaluate_layers(state);
        id
    }

    fn controller_of(state: &GameState, id: ObjectId) -> PlayerId {
        state.objects.get(&id).unwrap().controller
    }

    /// (a) Dynamic control reverts on a single leave: survivor P0 owns O, a TCE
    /// gives leaver P1 control. Eliminating P1 must prune the TCE and revert O to
    /// P0 — O stays on the battlefield, not exiled. Reverting the fix (never
    /// pruning the TCE) leaves O.controller == P1 stuck under an absent player and
    /// then step-4 exiles it, so `battlefield.contains(&o) && controller == P0`
    /// both flip.
    #[test]
    fn control_effect_reverts_when_controller_leaves() {
        let mut state = setup_three_player();
        let o = create_controlled_object(&mut state, PlayerId(0), PlayerId(1));
        assert_eq!(controller_of(&state, o), PlayerId(1));

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(
            state.battlefield.contains(&o),
            "survivor's object must remain on the battlefield after its thief leaves"
        );
        assert!(!state.exile.contains(&o));
        assert_eq!(
            controller_of(&state, o),
            PlayerId(0),
            "control reverts to the surviving owner (CR 800.4a + CR 613.1b)"
        );
    }

    /// (a2) Aura/Mind-Control-style control reverts via owned-exile: P1 owns a
    /// control-granting permanent (Aura) that gives P1 control of survivor P0's
    /// creature C. Eliminating P1 exiles the Aura (step 1, owner=P1) which removes
    /// its TCE source; the retain sweep drops the TCE and C reverts to P0.
    #[test]
    fn control_aura_reverts_when_owner_leaves() {
        let mut state = setup_three_player();
        // Survivor P0's creature.
        let c = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Survivor Creature".to_string(),
            Zone::Battlefield,
        );
        // P1's control Aura on the battlefield.
        let aura = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Control Magic".to_string(),
            Zone::Battlefield,
        );
        // Aura gives P1 control of C.
        state.add_transient_continuous_effect(
            aura,
            PlayerId(1),
            Duration::Permanent,
            TargetFilter::SpecificObject { id: c },
            vec![ContinuousModification::ChangeController],
            None,
        );
        super::super::layers::mark_layers_full(&mut state);
        super::super::layers::evaluate_layers(&mut state);
        assert_eq!(controller_of(&state, c), PlayerId(1));

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(state.exile.contains(&aura), "P1's Aura is owned-exiled");
        assert!(
            state.battlefield.contains(&c),
            "survivor's creature stays in play"
        );
        assert_eq!(controller_of(&state, c), PlayerId(0));
    }

    /// (b) Step-1 owned-exile + hostile negative. O is owned by the LEAVER P1 but
    /// controlled by survivor P0 via a TCE → O is exiled by step-1 owned-exile.
    /// Hostile: O2 owned by a LIVING third player P2, controlled by survivor P0 →
    /// eliminating P1 must NOT exile O2 and must NOT disturb its controller.
    #[test]
    fn leaver_owned_but_survivor_controlled_is_exiled_living_owned_is_not() {
        let mut state = setup_three_player();
        // O: owned by leaver P1, controlled by survivor P0.
        let o = create_controlled_object(&mut state, PlayerId(1), PlayerId(0));
        assert_eq!(controller_of(&state, o), PlayerId(0));
        // O2: owned by living P2, controlled by survivor P0.
        let o2 = create_controlled_object(&mut state, PlayerId(2), PlayerId(0));
        assert_eq!(controller_of(&state, o2), PlayerId(0));

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(
            state.exile.contains(&o),
            "object owned by the leaver leaves the game (step 1)"
        );
        assert!(!state.battlefield.contains(&o));
        assert!(
            state.battlefield.contains(&o2),
            "object owned by a LIVING player must not leave the game"
        );
        assert_eq!(
            controller_of(&state, o2),
            PlayerId(0),
            "a living player's control effect is untouched by an unrelated departure"
        );
    }

    /// (g) Step-4 controller-leg — the reachable CR-800.4a step-3 exile. A
    /// survivor-owned object whose `base_controller` is the leaver P1 (entered
    /// under P1's control, zones.rs:1172) with NO surviving control TCE. After the
    /// leaver leaves, layer re-derivation resets controller to base_controller ==
    /// P1, and the step-4 sweep exiles it. Reverting the sweep leaves O on the
    /// battlefield under an absent controller, so `exile.contains(&o)` flips.
    #[test]
    fn base_controller_reverts_to_leaver_then_step4_exiles() {
        let mut state = setup_three_player();
        let o = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Entered Under P1".to_string(),
            Zone::Battlefield,
        );
        // Enters under P1's control: sets base_controller = controller = P1.
        let mut events = Vec::new();
        crate::game::zones::apply_battlefield_entry_controller_override(
            &mut state,
            &mut events,
            o,
            PlayerId(1),
        );
        super::super::layers::mark_layers_full(&mut state);
        super::super::layers::evaluate_layers(&mut state);
        assert_eq!(controller_of(&state, o), PlayerId(1));

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(
            state.exile.contains(&o),
            "an object still controlled by the leaver (via base_controller) is exiled (CR 800.4a)"
        );
        assert!(!state.battlefield.contains(&o));
    }

    /// (c) CR 702.26k: a phased-OUT permanent owned by the leaver leaves the game.
    /// Pre-fix the battlefield leg used zone_object_ids which filters is_phased_in,
    /// so this object was skipped; the unfiltered iteration exiles it.
    #[test]
    fn phased_out_owned_permanent_leaves_the_game() {
        use crate::game::game_object::{PhaseOutCause, PhaseStatus};
        let mut state = setup_three_player();
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Phased Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        assert!(!state.objects.get(&id).unwrap().is_phased_in());

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        assert!(
            !state.battlefield.contains(&id),
            "phased-out permanent owned by the leaver must leave the battlefield (CR 702.26k)"
        );
        assert!(state.exile.contains(&id));
    }

    /// (d) An unrelated survivor's own creature and control effects are untouched
    /// when a different, uninvolved player leaves.
    #[test]
    fn uninvolved_survivor_creature_untouched() {
        let mut state = setup_three_player();
        // P0 owns a plain creature it controls itself.
        let own = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 Bear".to_string(),
            Zone::Battlefield,
        );
        let tce_count_before = state.transient_continuous_effects.len();

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(2), &mut events);

        assert!(state.battlefield.contains(&own));
        assert_eq!(controller_of(&state, own), PlayerId(0));
        assert_eq!(
            state.transient_continuous_effects.len(),
            tce_count_before,
            "no control effect is pruned when an uninvolved player leaves"
        );
    }

    /// (e) 2HG idempotency: an entire team leaves; each teammate's owned object is
    /// exiled exactly once (no double move_object / panic) and the other team wins.
    #[test]
    fn two_headed_giant_team_leaves_idempotent() {
        let mut state = setup_2hg();
        // Team A = {P0, P1}; Team B = {P2, P3} (free-for-all pairing in 2HG setup).
        let o0 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A0 Bear".to_string(),
            Zone::Battlefield,
        );
        let o1 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "A1 Bear".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        eliminate_players_simultaneously(&mut state, &[PlayerId(0), PlayerId(1)], &mut events);

        assert!(state.exile.contains(&o0));
        assert!(state.exile.contains(&o1));
        // Exiled exactly once each (no duplicate ids in exile).
        assert_eq!(state.exile.iter().filter(|&&x| x == o0).count(), 1);
        assert_eq!(state.exile.iter().filter(|&&x| x == o1).count(), 1);
        assert!(matches!(state.waiting_for, WaitingFor::GameOver { .. }));
    }

    /// (f) The hoist test (round-2 blocker). Co-leavers P1 < P2. Survivor P0 owns
    /// S, controlled by the HIGHER-id co-leaver P2 via a TCE. Eliminating [P1, P2]
    /// simultaneously must revert S to P0 and keep it on the battlefield. Under a
    /// per-player structure the retain/sweep would run inside each do_eliminate:
    /// when P1 is processed, P2's TCE is still live (S controlled by P2, a leaver)
    /// and the per-P1 sweep would over-exile S. Hoisting the retain+sweep to run
    /// ONCE over the full leaving_set is what lets S survive.
    #[test]
    fn hoisted_sweep_survivor_object_controlled_by_higher_id_coleaver_survives() {
        let mut state = setup_four_player();
        // Survivor P0 owns S; higher-id co-leaver P2 controls it.
        let s = create_controlled_object(&mut state, PlayerId(0), PlayerId(2));
        assert_eq!(controller_of(&state, s), PlayerId(2));

        let mut events = Vec::new();
        eliminate_players_simultaneously(&mut state, &[PlayerId(1), PlayerId(2)], &mut events);

        assert!(
            state.battlefield.contains(&s),
            "survivor's object must survive when a co-leaver controlled it (hoist)"
        );
        assert!(!state.exile.contains(&s));
        assert_eq!(
            controller_of(&state, s),
            PlayerId(0),
            "control reverts to the surviving owner P0"
        );
    }

    /// (h) Step-4 phased-out survivor guard. Survivor P0 OWNS a permanent that a
    /// leaver P1 stole via a ChangeController TCE, and it is then phased OUT.
    /// evaluate_layers skips phased-out permanents (CR 702.26b), so after the TCE
    /// is pruned and layers re-derive, obj.controller is NOT reset and still reads
    /// P1. A raw-battlefield step-4 sweep (pre-fix) would then over-EXILE this
    /// survivor-owned permanent. Restricting the sweep to battlefield_phased_in_ids
    /// leaves it frozen on the battlefield (it will revert to P0 on phase-in).
    /// Revert the fix (raw state.battlefield sweep) and this object gets exiled,
    /// flipping `battlefield.contains(&o)` and `!exile.contains(&o)`.
    #[test]
    fn phased_out_survivor_owned_stolen_permanent_not_over_exiled() {
        use crate::game::game_object::{PhaseOutCause, PhaseStatus};
        let mut state = setup_three_player();
        // Survivor P0 OWNS the permanent; leaver P1 controls it via a TCE.
        let o = create_controlled_object(&mut state, PlayerId(0), PlayerId(1));
        assert_eq!(controller_of(&state, o), PlayerId(1));

        // Phase it OUT while stolen. Layers freeze it (CR 702.26b): the controller
        // field is not reset by evaluate_layers, so it stays == P1 (the leaver).
        state.objects.get_mut(&o).unwrap().phase_status = PhaseStatus::PhasedOut {
            cause: PhaseOutCause::Directly,
        };
        assert!(!state.objects.get(&o).unwrap().is_phased_in());
        assert_eq!(
            controller_of(&state, o),
            PlayerId(1),
            "phased-out permanent keeps its stale (leaver) controller — evaluate_layers skips it"
        );

        let mut events = Vec::new();
        eliminate_player(&mut state, PlayerId(1), &mut events);

        // The survivor-owned, phased-out permanent must NOT be over-exiled by the
        // step-4 sweep: it stays frozen on the battlefield (CR 702.26b) and will
        // revert to its owner P0 when it phases back in.
        assert!(
            state.battlefield.contains(&o),
            "survivor-owned phased-out permanent must stay on the battlefield, not be over-exiled"
        );
        assert!(
            !state.exile.contains(&o),
            "survivor-owned phased-out permanent must not be exiled by the step-4 sweep"
        );
    }

    // CR 800.4a + CR 800.4b (test 7.4 — 4c controller leaves): a live control
    // (CR 723) ends when the controlling player leaves the game. Eliminating the
    // controller clears `turn_decision_controller` and drops their scheduled
    // control, while an UNRELATED control by a different controller survives (the
    // non-vacuous reach-guard). Revert-to-red: without the `do_eliminate` control
    // cleanup, `turn_decision_controller` stays `Some(controller)` and the entry
    // persists.
    #[test]
    fn controller_leaving_ends_scheduled_control() {
        let mut state = setup_three_player();
        let controller = PlayerId(0);
        let owner = PlayerId(1);
        let other_controller = PlayerId(1);
        let other_owner = PlayerId(2);
        state.active_player = owner;
        // C actively pilots O's turn (CR 723).
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: owner,
                controller,
                timestamp: 0,
                grant_extra_turn_after: false,
                window: crate::types::ability::ControlWindow::NextTurn,
            });
        state.turn_decision_controller = Some(controller);
        state.turn_decision_control_timestamp = Some(0);
        // An unrelated control by a different controller (reach-guard: proves the
        // cleanup is scoped to the leaving player, not a blanket wipe).
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: other_owner,
                controller: other_controller,
                timestamp: 0,
                grant_extra_turn_after: false,
                window: crate::types::ability::ControlWindow::NextTurn,
            });
        let mut events = Vec::new();

        eliminate_player(&mut state, controller, &mut events);

        assert_eq!(
            state.turn_decision_controller, None,
            "the departed controller's live control ends"
        );
        assert_eq!(state.turn_decision_control_timestamp, None);
        assert!(
            !state
                .scheduled_turn_controls
                .iter()
                .any(|s| s.controller == controller),
            "the departed controller's scheduled control is dropped"
        );
        assert!(
            state
                .scheduled_turn_controls
                .iter()
                .any(|s| s.controller == other_controller && s.target_player == other_owner),
            "an unrelated control by a living controller survives (non-vacuous)"
        );
    }

    /// Put a real in-construction triggered-ability entry on the stack for
    /// `controller`, park the construction cursors on it, and open a live
    /// `AbilityModeChoice` prompt for that same player. Returns the entry id.
    fn open_live_trigger_construction_prompt(
        state: &mut GameState,
        controller: PlayerId,
    ) -> ObjectId {
        let source = create_object(
            state,
            CardId(state.next_object_id),
            controller,
            "Construction prompt source".to_string(),
            crate::types::zones::Zone::Battlefield,
        );
        let trigger = crate::game::triggers::PendingTrigger::ordinary(
            source,
            controller,
            None,
            Box::new(ResolvedAbility::new(
                Effect::Draw {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Controller,
                },
                Vec::new(),
                source,
                controller,
            )),
            state.turn_number,
        );
        let mut events = Vec::new();
        let entry = crate::game::triggers::push_pending_trigger_to_stack(
            state,
            trigger.clone(),
            &mut events,
        );
        state.pending_trigger = Some(Box::new(trigger));
        state.pending_trigger_entry = Some(entry);
        state.pending_trigger_firing = Some(crate::types::identifiers::TriggerFiring::Ordinary);
        state.waiting_for = WaitingFor::AbilityModeChoice {
            player: controller,
            modal: crate::types::ability::ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                ..Default::default()
            },
            source_id: source,
            mode_abilities: Vec::new(),
            is_activated: false,
            ability_index: None,
            ability_cost: None,
            unavailable_modes: Vec::new(),
        };
        entry
    }

    fn concede(state: &mut GameState, player_id: PlayerId) {
        crate::game::engine::apply(
            state,
            player_id,
            crate::types::actions::GameAction::Concede { player_id },
        )
        .expect("concession is always legal");
    }

    /// CR 800.4a (plan Step 5, elimination site 1): `do_eliminate`'s
    /// tracked-entry-gone cleanup clears the construction priority recipient
    /// beside the three construction cursors it already clears — and it does so
    /// **with the cursors**, not only when the departing player happens to be
    /// the carried recipient. Both clones below exercise the same site.
    ///
    /// Revert discriminator: with only this site's clearing removed, the
    /// uncleared `Some(P1)` survives into the game-continues arm, where the
    /// re-point branch sees a no-longer-alive P1 and installs `Some(P2)` — so
    /// the `== None` assertion reads `Some(P2)` and fails.
    #[test]
    fn tracked_entry_cleanup_clears_the_construction_priority_recipient() {
        // Clone A: the leaver is both the prompt controller and the recipient.
        let mut state = setup_three_player();
        let entry = open_live_trigger_construction_prompt(&mut state, PlayerId(1));
        state.pending_trigger_construction_priority_recipient = Some(PlayerId(1));
        assert!(
            state.stack.iter().any(|e| e.id == entry),
            "positive reach guard: the tracked entry is really on the stack"
        );

        concede(&mut state, PlayerId(1));

        assert!(
            !state.stack.iter().any(|e| e.id == entry),
            "the leaver's trigger entry ceases to exist (CR 800.4a)"
        );
        assert_eq!(state.pending_trigger, None);
        assert_eq!(state.pending_trigger_entry, None);
        assert!(state.pending_trigger_event_batch.is_empty());
        assert_eq!(
            state.pending_trigger_construction_priority_recipient, None,
            "the recipient is cleared beside the three construction cursors"
        );
        let restored: GameState =
            serde_json::from_value(serde_json::to_value(&state).expect("serialize"))
                .expect("trusted round trip");
        assert_eq!(
            restored.pending_trigger_construction_priority_recipient, None,
            "the cleared recipient must survive trusted serde"
        );
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { player } if player != PlayerId(1)),
            "CR 800.4a hands the wait to a surviving player, got {:?}",
            state.waiting_for
        );

        // Clone B (non-carrier): the eliminated prompt controller is NOT the
        // carried recipient, so the site must still clear with the cursors.
        let mut state = setup_three_player();
        let entry = open_live_trigger_construction_prompt(&mut state, PlayerId(2));
        state.pending_trigger_construction_priority_recipient = Some(PlayerId(1));
        assert!(
            state.stack.iter().any(|e| e.id == entry),
            "positive reach guard: the tracked entry is really on the stack"
        );
        assert!(
            players::is_alive(&state, PlayerId(1)),
            "positive reach guard: the carried recipient is a DIFFERENT, still-living \
             player, so any clearing observed below came from this site rather than \
             from the departed-recipient re-point"
        );

        concede(&mut state, PlayerId(2));

        assert!(!state.stack.iter().any(|e| e.id == entry));
        assert_eq!(state.pending_trigger_entry, None);
        assert_eq!(
            state.pending_trigger_construction_priority_recipient, None,
            "the site clears the recipient with the cursors, not only when the two coincide"
        );
    }

    /// CR 800.4a (plan Step 5, elimination site 2): the terminal `GameOver`
    /// cleanup clears the recipient beside the same three cursors, in a fixture
    /// where **no leaving player controls the tracked entry** — so this site is
    /// provably the only one that can clear it.
    ///
    /// Revert discriminator: with only this site's clearing removed, a player
    /// who has left the game stays installed in a serialized `GameOver` state.
    /// No other branch masks it — site 1 never fires here, and the
    /// game-continues re-point cannot run on the terminal path.
    #[test]
    fn terminal_game_over_cleanup_clears_the_construction_priority_recipient() {
        let mut state = setup_three_player();
        let entry = open_live_trigger_construction_prompt(&mut state, PlayerId(0));
        state.pending_trigger_construction_priority_recipient = Some(PlayerId(1));

        concede(&mut state, PlayerId(2));

        // Pre-terminal reach guard: P1 controls no stack entry, so the
        // tracked-entry-gone site is false and this row provably exercises the
        // terminal branch rather than site 1.
        assert_eq!(
            state.pending_trigger_construction_priority_recipient,
            Some(PlayerId(1)),
            "a still-living recipient is neither cleared nor re-pointed"
        );
        assert_eq!(state.pending_trigger_entry, Some(entry));
        assert!(
            state.stack.iter().any(|e| e.id == entry),
            "the tracked entry is still on the stack before the final concession"
        );

        concede(&mut state, PlayerId(1));

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::GameOver {
                    winner: Some(PlayerId(0))
                }
            ),
            "the single game-over check crowns P0, got {:?}",
            state.waiting_for
        );
        assert_eq!(state.pending_trigger, None);
        assert_eq!(state.pending_trigger_entry, None);
        assert!(state.pending_trigger_event_batch.is_empty());
        assert_eq!(
            state.pending_trigger_construction_priority_recipient, None,
            "a departed player must not stay installed in a terminal snapshot"
        );
        let restored: GameState =
            serde_json::from_value(serde_json::to_value(&state).expect("serialize"))
                .expect("trusted round trip");
        assert_eq!(
            restored.pending_trigger_construction_priority_recipient,
            None
        );
    }

    /// CR 800.4a (plan Step 5, elimination site 3 — the new game-continues
    /// re-point): when the carried recipient alone leaves and neither cursor-
    /// clearing site fires, priority passes to the next player still in the
    /// game rather than stranding a departed recipient.
    ///
    /// Revert discriminator: without the re-point, `Some(P1)` stays installed
    /// with P1 out of the game, and the finisher would later return
    /// `WaitingFor::Priority { player: P1 }` for a departed player. Neither
    /// cursor-clearing row detects this; both still pass.
    #[test]
    fn game_continues_repoints_a_departed_construction_priority_recipient() {
        let mut state = setup_three_player();
        let entry = open_live_trigger_construction_prompt(&mut state, PlayerId(0));
        state.pending_trigger_construction_priority_recipient = Some(PlayerId(1));

        concede(&mut state, PlayerId(1));

        // Neither clearing site fired: the tracked entry is P0's and survives,
        // and the game continues so the terminal arm was never entered.
        assert!(
            state.stack.iter().any(|e| e.id == entry),
            "P0's tracked entry survives an opponent's departure"
        );
        assert_eq!(state.pending_trigger_entry, Some(entry));
        assert!(
            matches!(state.waiting_for, WaitingFor::AbilityModeChoice { player, .. } if player == PlayerId(0)),
            "the construction prompt is still live and unchanged for P0, got {:?}",
            state.waiting_for
        );
        assert_eq!(
            state.pending_trigger_construction_priority_recipient,
            Some(PlayerId(2)),
            "the departed recipient is re-pointed to the next living player, not stranded"
        );
        let restored: GameState =
            serde_json::from_value(serde_json::to_value(&state).expect("serialize"))
                .expect("trusted round trip");
        assert_eq!(
            restored.pending_trigger_construction_priority_recipient,
            Some(PlayerId(2)),
            "the re-pointed recipient must survive trusted serde"
        );
    }

    /// CR 800.4a + CR 101.4: the re-point follows the CURRENT turn-order
    /// direction, not fixed seating. Under `TurnDirection::Reversed` the next
    /// player in turn order after a departed P1 is P0, not seat-forward P2.
    ///
    /// Revert discriminator: `players::next_player` (seat-forward) re-points to
    /// P2 here; `players::next_player_in_turn_order` re-points to P0.
    #[test]
    fn game_continues_repoints_a_departed_recipient_in_reversed_turn_order() {
        let mut state = setup_three_player();
        state.turn_direction = crate::types::phase::TurnDirection::Reversed;
        let entry = open_live_trigger_construction_prompt(&mut state, PlayerId(0));
        state.pending_trigger_construction_priority_recipient = Some(PlayerId(1));

        concede(&mut state, PlayerId(1));

        assert!(
            state.stack.iter().any(|e| e.id == entry),
            "P0's tracked entry survives an opponent's departure"
        );
        assert_eq!(
            state.pending_trigger_construction_priority_recipient,
            Some(PlayerId(0)),
            "under reversed turn order the departed recipient re-points backward \
             through seating (the next player in TURN order), not seat-forward"
        );
    }
}
