use std::collections::HashSet;

use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::match_config::MatchPhase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::players;

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
        state.pending_trigger_order = None;
        state.deferred_triggers.clear();
        state.pending_trigger = None;
        state.pending_trigger_entry = None;
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

        if let Some(waiting_pid) = state.waiting_for.acting_player() {
            if !players::is_alive(state, waiting_pid) {
                let next = players::next_player(state, waiting_pid);
                state.waiting_for = WaitingFor::Priority { player: next };
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
        group
            .triggers
            .retain(|ctx| living_players.contains(&ctx.pending.controller));
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
        return;
    }

    // Some groups still need a choice — refresh the OrderTriggers prompt so
    // it points at the next-most-AP unordered group (possibly the same one
    // if its controller is alive).
    if let Some(wf) = super::triggers::build_next_order_triggers_prompt_public(state) {
        state.waiting_for = wf;
    }
}

/// CR 800.4a: Remove deferred triggers controlled by eliminated players.
fn prune_deferred_triggers_for_eliminated_players(state: &mut GameState) {
    state.deferred_triggers.retain(|ctx| {
        state
            .players
            .iter()
            .find(|player| player.id == ctx.pending.controller)
            .is_some_and(|player| !player.is_eliminated)
    });
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
        let req = crate::game::zone_pipeline::ZoneMoveRequest::player_left_game(id, Zone::Exile);
        crate::game::zone_pipeline::move_object(state, req, events);
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
        let req = crate::game::zone_pipeline::ZoneMoveRequest::player_left_game(id, Zone::Exile);
        crate::game::zone_pipeline::move_object(state, req, events);
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

    // Mark as eliminated
    if let Some(p) = state.players.iter_mut().find(|p| p.id == player) {
        p.is_eliminated = true;
    }
    if !state.eliminated_players.contains(&player) {
        state.eliminated_players.push(player);
    }

    crate::game::planechase::preserve_phenomenon_stack_abilities_for_handoff(state, planar_handoff);

    // CR 800.4a: Remove spells they control from the stack
    state.stack.retain(|entry| entry.controller != player);

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
        state.turn_decision_controller = None;
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
    // tracks is no longer on the stack, mirroring the `pending_cast` cleanup below.
    if state
        .pending_trigger_entry
        .is_some_and(|entry_id| !state.stack.iter().any(|entry| entry.id == entry_id))
    {
        state.pending_trigger_entry = None;
        state.pending_trigger = None;
        state.pending_trigger_event_batch.clear();
    }

    // CR 800.4a: Abandon any not-yet-resolved cast this player controls. A spell
    // paused mid-cast (e.g. a convoke spell awaiting `WaitingFor::ManaPayment`)
    // is held in `state.pending_cast`, not as a stack entry, so the stack retain
    // above does not clear it. Left behind, the in-progress cast lingers in the
    // GameState after the player leaves — and because the WASM engine is a
    // singleton reused across games, it can resurface as a stuck mana-payment
    // window in a later game. Only clear a pending cast the *leaving* player
    // controls; another living player's mid-cast must survive an opponent's
    // departure, so key off the spell object's controller (the caster).
    if state
        .pending_cast
        .as_ref()
        .and_then(|pc| state.objects.get(&pc.object_id))
        .is_some_and(|obj| obj.controller == player)
    {
        state.pending_cast = None;
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
    // (payer == affected, replacement.rs), and `do_eliminate` never mutates
    // `waiting_for` (the rewrite runs after the loop), so this key is CONSTANT
    // across a simultaneous multi-elimination batch and object-graph-independent.
    // (`ProposedEvent::affected_player` would mis-resolve here: once a co-eliminated
    // lower-id loser has exiled the affected object, its effective controller is
    // reverted to its owner — CR 616.1's owner-fallback is pre-existing and NOT
    // relied upon.) Mirror the `pending_cast` teardown: clear `pending_replacement`
    // (the SBA-gating slot) plus the parked replacement's own tightly-coupled
    // continuation slots (`replacement_may_cost_paused`, `post_replacement_*`,
    // `pending_connive_reentry`). The resume drain also touches OTHER resolution
    // slots on a normal answer (e.g. `pending_phase_transition_progress`,
    // `pending_team_draw_step`, `pending_continuation`); those are intentionally
    // NOT cleared here. Stranding some of them is its own PRE-EXISTING soft-lock
    // (PPT gates `auto_advance`; `pending_continuation` gates the deferred-trigger
    // drain) that predates this PR and is NOT the reported regression; repairing
    // them correctly requires resuming the interrupted APNAP queue for the
    // remaining players (not field-nulling), tracked as a separate follow-up. This
    // fix deliberately addresses only the CR 704.4 SBA-freeze introduced by the
    // `pending_replacement` guard.
    let leaving_is_latched_chooser = state.waiting_for.acting_player() == Some(player);
    // CR 800.4a + CR 616.1: SearchFound owns an outer per-card batch, and a
    // replacement-selected zone move may own a nested batch completion. The
    // inner pause can be either replacement ordering or another zone-delivery
    // choice, so this teardown is keyed to the batch plus its latched chooser,
    // independently of `pending_replacement`.
    if state.pending_search_found_batch.is_some() && leaving_is_latched_chooser {
        state.pending_search_found_batch = None;
        if state
            .pending_batch_deliveries
            .as_ref()
            .and_then(|pending| pending.completion.as_ref())
            .is_some_and(|completion| {
                matches!(
                    completion,
                    crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery { .. }
                )
            })
        {
            state.pending_batch_deliveries = None;
        }
    }
    if state.pending_replacement.is_some() && leaving_is_latched_chooser {
        state.pending_replacement = None;
        state.replacement_may_cost_paused = false;
        super::replacement::abandon_post_replacement_continuation(state);
    }

    // CR 800.4a: A coupled ETB spell-resolution context can outlive its
    // `pending_replacement` (nested `ContinueZoneDeliveryTail` early-return,
    // engine_replacement.rs), so it is torn down under its OWN controller-keyed
    // guard — cleared only for the LEAVING player's own resolution (mirroring the
    // `pending_cast` controller key above) so a living player's paused resolution
    // survives an opponent's departure.
    if state
        .pending_spell_resolution
        .as_ref()
        .is_some_and(|psr| psr.controller == player)
    {
        state.pending_spell_resolution = None;
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
                    ability: venture_ability,
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
        state.command_zone.retain(|id| !scheme_ids.contains(id));
    }

    events.push(GameEvent::PlayerEliminated { player_id: player });
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
    use crate::types::ability::{Effect, PostReplacementContinuation, ResolvedAbility, TargetRef};
    use crate::types::counter::CounterType;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{
        CastingVariant, PendingCast, PendingConniveReentry, PendingReplacement,
        PendingSpellResolution, StackEntry, StackEntryKind,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaCost;
    use crate::types::proposed_event::{CounterPlacement, ProposedEvent};

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

    fn pending_search_found_batch(
        searcher: PlayerId,
        object_id: ObjectId,
    ) -> crate::types::game_state::PendingSearchFoundBatch {
        crate::types::game_state::PendingSearchFoundBatch {
            searcher,
            remaining: vec![object_id],
            survivors: Vec::new(),
            continuation: crate::types::game_state::PendingSearchFoundContinuation::Standard {
                split: None,
            },
            visibility: crate::types::game_state::SearchFoundVisibility::Private,
        }
    }

    fn pending_search_found_zone_delivery(
        object_id: ObjectId,
    ) -> crate::types::game_state::PendingBatchDeliveries {
        crate::types::game_state::PendingBatchDeliveries {
            remaining: Vec::new(),
            destination: Zone::Exile,
            source_id: None,
            enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            exile_tracking: crate::types::game_state::ZoneDeliveryExileTracking::None,
            library_placement: None,
            completion: Some(
                crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery { object_id },
            ),
            replacement_applied: HashSet::new(),
        }
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
            .post_replacement_drains
            .resident_mut()
            .expect("a drain must be resident")
            .source = Some(o);
        state
            .post_replacement_drains
            .resident_mut()
            .expect("a drain must be resident")
            .event_source = Some(o);
        state
            .post_replacement_drains
            .resident_mut()
            .expect("a drain must be resident")
            .event_target = Some(TargetRef::Object(o));
        // Issue #4886 (review #6): a live Jinnie Fay-class token-choice applied
        // seed, owned by this same abandoned continuation, must be abandoned
        // alongside its siblings — this field was added after the teardown
        // block below was written and was missed until this regression.
        state.post_replacement_token_choice_applied = Some(HashSet::from([
            crate::types::proposed_event::AppliedReplacementKey::object(o, 0),
        ]));
        state.pending_connive_reentry = Some(PendingConniveReentry {
            conniver: o,
            count: 1,
            applied: HashSet::new(),
        });
        state.pending_search_found_batch = Some(pending_search_found_batch(PlayerId(2), o));
        state.pending_batch_deliveries = Some(pending_search_found_zone_delivery(o));
        // CR 121.2: a paused draw instruction owned by the LEAVING chooser (P2) —
        // single-player-scoped, must clear alongside its siblings via
        // `abandon_post_replacement_continuation` (replacement.rs).
        state.draw_sequences.push(PlayerId(2), 1);
        // Coupled spell-resolution ctx owned by the LEAVING chooser (P2) — must clear.
        state.pending_spell_resolution = Some(PendingSpellResolution {
            object_id: o,
            controller: PlayerId(2),
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
        assert!(state.pending_connive_reentry.is_none());
        assert!(
            state.pending_search_found_batch.is_none(),
            "the eliminated chooser's outer found-card batch must be abandoned"
        );
        assert!(
            state.pending_batch_deliveries.is_none(),
            "the eliminated chooser's nested found-card zone completion must be abandoned"
        );
        assert!(
            state.draw_sequences.is_empty(),
            "CR 121.2: the leaving chooser's paused draw instruction must be \
             cleared via abandon_post_replacement_continuation, not stranded"
        );
        assert!(
            state.pending_spell_resolution.is_none(),
            "the leaving chooser's coupled spell-resolution ctx must be torn down"
        );
    }

    #[test]
    fn elimination_clears_outer_search_found_batch_without_nested_zone_completion() {
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
        assert!(state.pending_batch_deliveries.is_none());

        eliminate_player(&mut state, PlayerId(0), &mut Vec::new());

        assert!(state.pending_replacement.is_none());
        assert!(state.pending_search_found_batch.is_none());
        assert!(state.pending_batch_deliveries.is_none());
    }

    #[test]
    fn opponent_leaving_preserves_living_choosers_replacement() {
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
        state.pending_batch_deliveries = Some(pending_search_found_zone_delivery(parked_found));
        // A coupled spell-resolution ctx owned by the LIVING chooser (P0).
        state.pending_spell_resolution = Some(PendingSpellResolution {
            object_id: create_object(&mut state, CardId(7), PlayerId(0), "S".into(), Zone::Stack),
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
        // CR 121.2: a paused draw instruction owned by the LIVING chooser (P0)
        // must survive a different player's departure — no over-clear.
        let living_frame = state.draw_sequences.push(PlayerId(0), 2);
        state
            .draw_sequences
            .active_if(living_frame)
            .expect("the frame just pushed is active")
            .accumulated = 1;

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
            state
                .pending_batch_deliveries
                .as_ref()
                .is_some_and(|pending| {
                    matches!(
                        pending.completion,
                        Some(
                            crate::types::game_state::BatchCompletion::SearchFoundZoneDelivery {
                                object_id
                            }
                        ) if object_id == parked_found
                    )
                }),
            "an opponent leaving must preserve the living chooser's nested found-card completion"
        );
        assert!(
            state.pending_spell_resolution.is_some(),
            "an opponent leaving must not tear down the living player's spell-resolution ctx"
        );
        let survivor = state
            .draw_sequences
            .active()
            .expect("an opponent leaving must not clear the living chooser's paused instruction");
        assert_eq!(
            (survivor.player, survivor.remaining, survivor.accumulated),
            (PlayerId(0), 2, 1),
            "the living chooser's paused draw instruction must survive intact — owed units and \
             already-delivered count both preserved"
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
                grant_extra_turn_after: false,
                window: crate::types::ability::ControlWindow::NextTurn,
            });
        state.turn_decision_controller = Some(controller);
        // An unrelated control by a different controller (reach-guard: proves the
        // cleanup is scoped to the leaving player, not a blanket wipe).
        state
            .scheduled_turn_controls
            .push(crate::types::game_state::ScheduledTurnControl {
                target_player: other_owner,
                controller: other_controller,
                grant_extra_turn_after: false,
                window: crate::types::ability::ControlWindow::NextTurn,
            });
        let mut events = Vec::new();

        eliminate_player(&mut state, controller, &mut events);

        assert_eq!(
            state.turn_decision_controller, None,
            "the departed controller's live control ends"
        );
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
}
