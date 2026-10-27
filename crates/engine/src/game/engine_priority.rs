use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

use super::engine::{begin_pending_trigger_target_selection, check_exile_returns, EngineError};
use super::match_flow;
use super::players;
use super::sba;
use super::triggers;
use super::triggers::DeferredTriggerDrainPolicy;

/// CR 608.2n: whether a paused spell's final part ran before this pass began.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SpellFinalPartBoundary {
    AtPassTop,
    RanBeforePass,
}

pub(super) fn run_post_action_pipeline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
    skip_deferred_trigger_drain: bool,
) -> Result<WaitingFor, EngineError> {
    run_post_action_pipeline_from(
        state,
        events,
        0,
        default_wf,
        skip_trigger_scan,
        skip_deferred_trigger_drain,
    )
}

/// Run the normal post-action settlement while scanning only events produced at
/// or after `event_start`. Use for nested resume paths that carry earlier
/// payment/choice events in the same output buffer.
pub(crate) fn run_post_action_pipeline_from(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
    skip_deferred_trigger_drain: bool,
) -> Result<WaitingFor, EngineError> {
    run_post_action_pipeline_from_with_policy(
        state,
        events,
        event_start,
        default_wf,
        skip_trigger_scan,
        if skip_deferred_trigger_drain {
            DeferredTriggerDrainPolicy::Skip
        } else {
            DeferredTriggerDrainPolicy::ResolutionSafe
        },
        None,
        SpellFinalPartBoundary::AtPassTop,
        None,
        None,
    )
}

/// CR 603.2c + CR 603.3b: replay history is already collected; only the
/// canonical continuation's suffix joins this completed resolution's batch.
pub(crate) fn run_post_action_pipeline_for_canonical_tail(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    continuation_event_start: usize,
    default_wf: &WaitingFor,
    completed_resolution: Option<ObjectId>,
) -> Result<WaitingFor, EngineError> {
    run_post_action_pipeline_from_with_policy(
        state,
        events,
        continuation_event_start,
        default_wf,
        false,
        DeferredTriggerDrainPolicy::ResolutionSafe,
        None,
        SpellFinalPartBoundary::AtPassTop,
        Some(continuation_event_start),
        completed_resolution,
    )
}

/// CR 603.3b: an ordinary choice that retired its exact parent joins the
/// answer's observers to everything retained during that resolution.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_post_action_pipeline_after_resumed_resolution(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
    skip_deferred_trigger_drain: bool,
    completed_resolution: ObjectId,
) -> Result<WaitingFor, EngineError> {
    run_post_action_pipeline_from_with_policy(
        state,
        events,
        event_start,
        default_wf,
        skip_trigger_scan,
        if skip_deferred_trigger_drain {
            DeferredTriggerDrainPolicy::Skip
        } else {
            DeferredTriggerDrainPolicy::ResolutionSafe
        },
        None,
        SpellFinalPartBoundary::AtPassTop,
        Some(0),
        Some(completed_resolution),
    )
}

/// CR 117.3c + CR 117.5 + CR 605.4a: the settled-Priority convergence wrapper.
///
/// The one caller family is a handler that returns its `ActionResult` directly,
/// bypassing the reducer's ordinary epilogue, after a sidecar-owned accepted
/// triggered-mana occurrence resumed a **direct `ManaAbilityResume::Priority`
/// root** with no live cast/resolution/payment owner left. Its `settled_priority`
/// is that root's own exact reconstructed wait, and it is exhaustively validated
/// here rather than trusted.
///
/// It differs from the ordinary wrappers in exactly three ways: the drain policy
/// permits a passive announced spell to remain on the stack (the batch below it
/// is fully settled, so its observers belong above it); the carried recipient
/// governs the no-choice stack-growth exit and is persisted before any ordering
/// or construction step can pause; and its own return is a finisher call, which
/// is Round-20 seam 7 — this wrapper must never hand back a non-prompt
/// `Priority` with the recipient still installed.
pub(crate) fn run_post_action_pipeline_from_settled_priority(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    settled_priority: &WaitingFor,
) -> Result<WaitingFor, EngineError> {
    let WaitingFor::Priority { player } = *settled_priority else {
        debug_assert!(
            false,
            "settled-Priority convergence requires the root's exact Priority wait, \
             got {settled_priority:?}",
        );
        return Ok(settled_priority.clone());
    };
    triggers::preserve_trigger_construction_priority_recipient(state, player);
    let produced = run_post_action_pipeline_from_with_policy(
        state,
        events,
        event_start,
        settled_priority,
        false,
        DeferredTriggerDrainPolicy::SettledPriority,
        Some(player),
        SpellFinalPartBoundary::AtPassTop,
        None,
        None,
    )?;
    Ok(triggers::finish_trigger_construction_action(
        state, events, produced,
    ))
}

/// CR 608.2n + CR 117.5: the pass pipeline after `stack::resolve_top`'s net
/// ran a resolution's final part in this pass. Identical to
/// `run_post_action_pipeline` except that the final part ran before the pass
/// (`SpellFinalPartBoundary::RanBeforePass`).
pub(super) fn run_post_action_pipeline_after_spell_final_part(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
) -> Result<WaitingFor, EngineError> {
    run_post_action_pipeline_from_with_policy(
        state,
        events,
        0,
        default_wf,
        skip_trigger_scan,
        DeferredTriggerDrainPolicy::ResolutionSafe,
        None,
        SpellFinalPartBoundary::RanBeforePass,
        None,
        None,
    )
}

/// The shared post-action settlement core. Every ordinary wrapper reaches it
/// through the boolean form above, with `carried_priority_recipient == None`,
/// which preserves their current stack-growth fallback. CR 117.3b + CR 117.5:
/// the one recipient an ordinary wrapper's pass can persist is the
/// synchronized active player of an exact carrier this core itself retired
/// (or, once that player has left the game, the next player in turn order
/// still in it, CR 800.4j).
///
/// `carried_priority_recipient` is `Some(player)` only for a settled-Priority
/// convergence whose activator was not the active player (CR 117.3c + CR 117.5).
/// It changes exactly two things: the wait computed at the no-choice
/// stack-growth exit, and the fact that any ordering or construction prompt the
/// pipeline opens persists that player so the construction finisher can hand
/// priority back to them once the batch is fully announced.
#[allow(clippy::too_many_arguments)]
fn run_post_action_pipeline_from_with_policy(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
    drain_policy: DeferredTriggerDrainPolicy,
    carried_priority_recipient: Option<PlayerId>,
    final_part_boundary: SpellFinalPartBoundary,
    delayed_event_start: Option<usize>,
    completed_resolution: Option<ObjectId>,
) -> Result<WaitingFor, EngineError> {
    #[cfg(feature = "test-support")]
    crate::game::perf_counters::record_post_action_pipeline_pass();
    super::announcement_triggers::stage_announcement_trigger_events(state, events, event_start);
    let settled = run_post_action_pipeline_core(
        state,
        events,
        event_start,
        default_wf,
        skip_trigger_scan,
        drain_policy,
        carried_priority_recipient,
        final_part_boundary,
        delayed_event_start,
        completed_resolution,
    );
    // The pass scanned the buffer through its end; later announcement staging
    // in this action starts after it.
    state.announcement_event_scope = state.announcement_event_scope.advanced_to(events.len());
    settled
}

#[allow(clippy::too_many_arguments)]
fn run_post_action_pipeline_core(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
    drain_policy: DeferredTriggerDrainPolicy,
    carried_priority_recipient: Option<PlayerId>,
    final_part_boundary: SpellFinalPartBoundary,
    delayed_event_start: Option<usize>,
    completed_resolution: Option<ObjectId>,
) -> Result<WaitingFor, EngineError> {
    // CR 608.2c + CR 608.2n: an instant or sorcery whose instructions paused
    // is put into its owner's graveyard as the final part of its resolution,
    // once those instructions are done. This boundary completes it, before the
    // scan below; the final part's triggers wait for this pass's batch.
    let mut effective_default = default_wf.clone();
    // CR 117.3b + CR 117.5: set only when this pass retires its exact carrier.
    let mut completion_recipient = None;
    let carrier_before_final_part = state.resolving_stack_entry.as_ref().map(|entry| entry.id);
    let final_part_ran = super::engine::run_spell_final_part_if_due(state, events)
        || final_part_boundary == SpellFinalPartBoundary::RanBeforePass;
    if carrier_before_final_part.is_some()
        && state.resolving_stack_entry.is_none()
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
    {
        effective_default = state.waiting_for.clone();
        completion_recipient = completed_priority_recipient(state);
    }
    let completed_resolution_batch = completed_resolution.is_some();
    let payment_owned = state.payment_transaction.is_some() || state.payment_transaction_replay;
    // CR 608.2n + CR 117.5 + CR 603.3: once the final part has run the resolution
    // is over, so the batch it completes goes on the stack above any spell still
    // below it (that spell has not begun to resolve, CR 405.5). The #1793 guard
    // is for a resolution still under way, which never reaches this point.
    //
    // CR 603.3b: batch ownership is read from the policy the pass was entered
    // with: `after_spell_final_part` shifts `ResolutionSafe` to `SettledPriority`
    // for the drain. The caller's exact retired-carrier handoff proves the same
    // completed boundary for an activated ability, even above an unrelated
    // unresolved spell. Skip still owns no drain; the shared owner/readiness
    // gates still refuse unfinished work under either policy.
    let seam_drain_policy = drain_policy;
    let drain_policy = if final_part_ran || completed_resolution_batch {
        drain_policy.after_spell_final_part()
    } else {
        drain_policy
    };

    // CR 117.3c + CR 117.5: the wait a completed no-choice pass hands back. With
    // no carried recipient this is exactly baseline's active-player wait.
    let settled_priority_wait = WaitingFor::Priority {
        player: carried_priority_recipient.unwrap_or(state.active_player),
    };

    // Capture stack depth before any trigger/SBA processing so we can detect
    // whether new triggered abilities were added during this pipeline pass.
    let stack_before = state.stack.len();
    // Only a batch parked by an SBA-owned player choice joins the answer's
    // trigger events. Other deferred queues are construction tails whose
    // ordinary drains must neither absorb later events nor rescan delayed
    // triggers.
    let deferred_trigger_batch_was_sba_choice_parked =
        triggers::has_sba_choice_trigger_batch(state);
    // CR 603.3b: the queue boundary between contexts that predate this pass and
    // the ones its own collectors are about to append. `take_sba_choice_trigger_batch`
    // needs it because an answer-generated trigger is collected as `Ordinary`
    // too — origin alone cannot tell it apart from a construction tail that was
    // already queued when the choice opened.
    let deferred_triggers_before_answer = state.deferred_triggers.len();
    let mut consumed_trigger_events =
        std::mem::take(&mut state.consumed_before_priority_trigger_events);
    let mut delayed_trigger_events = Vec::new();
    // Where this pass's delayed scan starts in `events`: the whole buffer,
    // unless the announced-cast batch below already scanned a prefix of it.
    // None retains old wrappers' whole-buffer Priority scan. Some(start)
    // additionally owns delivery before a further prompt, including Some(0)
    // on abort. The bound itself never proves carrier completion.
    let mut delayed_scan_start = delayed_event_start.unwrap_or(0);
    // CR 603.3 + CR 603.3b: the cast observers latched during a finished
    // resolution are put on the stack with this action's fresh observers as
    // ONE batch, above the cast spells; the fresh observers join the queue
    // instead of being dispatched ahead of it.
    let announced_cast_batch = seam_drain_policy == DeferredTriggerDrainPolicy::ResolutionSafe
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
        && triggers::deferred_triggers_hold_announced_casts(state);

    // CR 603.2: Triggered abilities trigger at the moment the event occurs.
    // Scan for triggers BEFORE SBAs so that objects still on the battlefield
    // (e.g., a creature that just took lethal damage) are found by the scan.
    // This follows the same pattern as process_combat_damage_triggers in combat_damage.rs.
    //
    // CR 614.12a + CR 707.9: Mid-entry choice deferral (`CopyTargetChoice`,
    // `ChooseOneOfBranch` enters-counter, and `NamedChoice` as-enters-choose)
    // captures the entering object's `ZoneChanged` event into
    // `state.deferred_entry_events` for replay once the choice resolves. The
    // original event remains in `events` for the frontend animation, but it
    // MUST NOT reach `process_triggers` / `collect_triggers_into_deferred`
    // here — the replay in `replay_deferred_entry_events` owns the single
    // authoritative trigger scan for those events. Without this exclusion, the
    // entry ZoneChanged is collected once here (into `deferred_triggers` via
    // `collect_triggers_into_deferred` when `waiting_for` is `NamedChoice`)
    // and fired a second time by the replay, causing double-fire for ETB
    // observers like Soul Warden (issue #830).
    // CR 603.3b + CR 608.2g: A terminal resolution completion can be installed while a
    // replacement-resume action is finalizing its deferred free cast. That
    // action may have already handled ordinary zone-event triggers, but its
    // just-emitted `SpellCast` still has to join the terminal batch before B's
    // parked trigger drains.
    if !skip_trigger_scan || state.pending_resolution_completion.is_some() {
        // A paused logical zone-change owner has already Segment-collected its
        // retained ZoneChanged occurrences into deferred trigger contexts. Do
        // not rediscover those records through the generic post-action scan;
        // its batched definitions remain reserved for final settlement.
        let mut retained_logical_zone_events = retained_logical_zone_events(state);
        if payment_owned || delayed_event_start.is_some() {
            retained_logical_zone_events.extend(retained_cost_events(state));
        }
        // A completed logical owner has already collected its segment and
        // settlement contexts into the deferred queue, and a paused owner that
        // drained may instead have claimed them in the consumed ledger.
        // `filter_already_collected_trigger_events_from` is the single authority
        // for both (CR 603.2c), shared with the search-delivery park family so
        // the two collectors cannot drift.
        let unconsumed_events = triggers::filter_already_collected_trigger_events_from(
            state,
            events,
            event_start,
            &consumed_trigger_events,
        );
        let mut filtered_events: Vec<_> = unconsumed_events
            .iter()
            .filter(|event| {
                !matches!(event, GameEvent::PhaseChanged { .. })
                    && !state.deferred_entry_events.contains(event)
                    && !retained_logical_zone_events.contains(event)
            })
            .cloned()
            .collect();
        delayed_trigger_events = events[event_start..]
            .iter()
            .filter(|event| {
                !matches!(event, GameEvent::PhaseChanged { .. })
                    && !state.deferred_entry_events.contains(event)
                    && !retained_logical_zone_events.contains(event)
            })
            .cloned()
            .collect();
        if skip_trigger_scan {
            filtered_events.retain(|event| matches!(event, GameEvent::SpellCast { .. }));
            delayed_trigger_events.retain(|event| matches!(event, GameEvent::SpellCast { .. }));
        }
        // CR 603.3b: If the resolution step that just ran paused for a player
        // resolution-choice (Scry/Surveil/Dig/Search/...), the triggered
        // abilities it generated (e.g. "whenever you scry, ...") must NOT be
        // collected and ordered now — doing so overwrites the pending choice's
        // WaitingFor (the `OrderTriggers` PromptForChoice arm clobbers
        // `ScryChoice` when 2+ same-controller triggers fire). Park them in
        // `deferred_triggers`; they are drained below once the action settles
        // back to Priority. Mirrors `batch_or_drain_observer_triggers`' B2 branch.
        // CR 603.3b: Terminal-resolution observers join the deferred batch so
        // they are ordered only after the resolving ability has completed.
        // CR 704.3 + CR 117.5 + CR 510.3a: triggered abilities waiting to be put
        // on the stack are put there only when a player WOULD receive priority,
        // and combat-damage triggers specifically go on the stack BEFORE the
        // active player gets priority (CR 510.3a). A parked replacement
        // (a CR 616.1 ordering choice, a CR 616.1b entry-controller choice, or
        // any prompt raised while answering one) is a mid-event pause: the event
        // that triggered these abilities has not finished happening and no player
        // receives priority for the choice. Park the batch instead, so it reaches
        // the stack as ONE CR 603.3b batch once the answer settles resolution
        // back to Priority — splitting it would deny the controller the ordering
        // choice over the whole batch.
        //
        // Keyed on `state.pending_replacement`, NOT on a
        // `WaitingFor::ReplacementChoice` match and NOT by admitting that variant
        // to `engine_resolution_choices::handles`:
        //   * The field is set BEFORE the wait is installed. `replacement.rs`'s
        //     pipeline_loop parks the record and returns `NeedsChoice` with no
        //     write to `waiting_for`; the caller installs the wait afterwards. A
        //     `matches!` on the variant is blind inside that window; the field is
        //     not. It also covers `EntryControllerChoice`, which `handles` does
        //     not admit either.
        //   * `handles` is consulted by four other seams (the reducer's dispatch
        //     arm in engine.rs, `replacement.rs::park_waiting_for`,
        //     `triggers.rs::resolution_completion_can_settle`, and
        //     `park_cast_during_resolution_cast_observers`). Admitting
        //     `ReplacementChoice` there would silently re-home the replacement
        //     pause's whole action-dispatch surface and would make a CHAINED
        //     replacement choice fail to install its own candidate list.
        //
        // Not over-broad: on the ANSWERING path the field is already cleared —
        // `continue_replacement_impl` `.take()`s it as its first statement — so
        // the disjunct is inert at the reducer's own pipeline call and bites only
        // at the unguarded `pass_priority_once_with_pipeline` seam.
        if super::engine_resolution_choices::handles(&state.waiting_for)
            // CR 603.3b + CR 608.2d: a resolution paused on one of its own
            // choices has not reached a priority boundary, so its earlier
            // siblings' triggers must not be ordered yet. `handles` is an
            // action-dispatch predicate and misses the direct-choice frame
            // family (`OptionalEffectChoice`, `OpponentMayChoice`,
            // `ProliferateChoice`, the resolution `PayCost`); the frame stack
            // answers structurally for all of them.
            || triggers::resolution_frame_is_live_off_priority(state)
            || state.pending_replacement.is_some()
            || state.pending_resolution_completion.is_some()
            || deferred_trigger_batch_was_sba_choice_parked
            // CR 603.3 + CR 603.3b: a pass that completed a paused resolution
            // puts everything that triggered since a player last received
            // priority — the parked triggers, its final part's and this
            // answer's — on the stack as one batch, through the drain below.
            || final_part_ran
            || payment_owned
            || completed_resolution_batch
        {
            triggers::collect_triggers_into_deferred(state, &filtered_events);
        } else if announced_cast_batch && state.game_end.is_none() {
            // The action's delayed observers join the batch in the single
            // delayed scan before the combined drain below.
            triggers::collect_triggers_into_deferred(state, &filtered_events);
        } else if state.game_end.is_none() {
            // CR 104.1: only while the game is still going. Once this action
            // has recorded a result on `GameState::game_end` (e.g. a CR 104.4b
            // draw declared mid-resolution), the game has already ended, so
            // abilities that triggered on the action's events never go on the
            // stack.
            triggers::process_triggers(state, &filtered_events);
        }
    }

    // CR 704.3: SBA/trigger loop. SBAs may generate events (e.g., ZoneChanged for
    // dying creatures) that need trigger processing. Repeat until no new SBAs fire,
    // matching the loop pattern in process_combat_damage_triggers.
    //
    // Gate on `Priority`: `process_triggers` may have paused on `OrderTriggers`
    // or a resolution-choice handler may already own `waiting_for` — running SBAs
    // in those states would clobber the open prompt (same failure mode as #2420).
    //
    // CR 704.4 + CR 616.1: this gate also covers the replacement-order-choice
    // case — a `WaitingFor::ReplacementChoice` is not `Priority`, so the loop
    // never runs SBAs while resolution is paused on one. That matters because a
    // `ReplacementChoice` is a mid-resolution pause: the triggering event (e.g. a
    // permanent's "enters with X +1/+1 counters" ETB placement, doubled/incremented
    // by two or more order-material replacements like Branching Evolution + Ozolith,
    // so CR 616.1 makes the application order the controller's choice) has not
    // finished happening — the counters are not on the object yet. CR 704.4
    // ("state-based actions pay no attention to what happens during the resolution
    // of a spell or ability") means checking SBAs now would wrongly send a
    // still-entering 0/0 to the graveyard (CR 704.5f) before its counters land. The
    // loop runs on the next pipeline pass, once the choice is answered and
    // resolution settles back to Priority.
    //
    // Player-loss SBAs remain covered mid-choice by `reconcile_terminal_result`
    // (engine.rs), which deliberately runs the SBA loop even while paused on a
    // replacement choice so the engine never waits on a player who has already
    // lost (#962). That path is safe against the 0/0-destruction described above
    // because `check_state_based_actions` itself honors the same CR 704.4
    // exemption: it returns before the object-destroying SBAs whenever
    // `pending_replacement` is set, so the mid-choice player-loss net processes
    // the loss without sending the still-entering permanent to the graveyard.
    while matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        let events_before = events.len();
        sba::check_state_based_actions(state, events);
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            // CR 603.3b + CR 903.9a: an SBA batch can open a resumable
            // non-priority choice (for example, a commander-zone return choice)
            // before the ordinary priority-path collector runs. Preserve every
            // trigger from the completed SBA batch until that choice is
            // answered; otherwise its ZoneChanged events are never scanned.
            // CR 104.1: GameOver is terminal, so elimination's cleanup must not
            // be repopulated from the events that ended the game.
            if !matches!(state.waiting_for, WaitingFor::GameOver { .. })
                && events.len() > events_before
            {
                consumed_trigger_events.extend(std::mem::take(
                    &mut state.consumed_before_priority_trigger_events,
                ));
                let raw_sba_events: Vec<_> = events[events_before..].to_vec();
                let sba_events = triggers::filter_already_collected_trigger_events_from(
                    state,
                    events,
                    events_before,
                    &consumed_trigger_events,
                );
                let sba_choice_batch =
                    sba_choice_requires_combined_trigger_batch(&state.waiting_for);
                if sba_choice_batch {
                    triggers::collect_sba_choice_triggers_into_deferred(state, &sba_events);
                } else {
                    triggers::collect_triggers_into_deferred(state, &sba_events);
                }
                // Logical zone-change owners collect ordinary observers only.
                // Delayed triggers remain entitled to the raw occurrences; the
                // ordinary queued-context witness must not suppress them.
                if sba_choice_batch {
                    triggers::collect_sba_choice_delayed_triggers_into_deferred(
                        state,
                        &raw_sba_events,
                    );
                } else {
                    triggers::collect_delayed_triggers_into_deferred(state, &raw_sba_events);
                }
            }
            break;
        }
        if events.len() > events_before {
            consumed_trigger_events.extend(std::mem::take(
                &mut state.consumed_before_priority_trigger_events,
            ));
            let sba_events = triggers::filter_already_collected_trigger_events_from(
                state,
                events,
                events_before,
                &consumed_trigger_events,
            );
            // CR 603.3b: SBA-generated triggers join the terminal batch rather
            // than being ordered before its final cast trigger is collected.
            // CR 117.5 + CR 603.3b: in the pass that ran a spell's final part,
            // they join that resolution's batch the same way.
            if state.pending_resolution_completion.is_some()
                || announced_cast_batch
                || final_part_ran
                || payment_owned
                || completed_resolution_batch
            {
                triggers::collect_triggers_into_deferred(state, &sba_events);
            } else {
                triggers::process_triggers(state, &sba_events);
            }
            // CR 603.3d: SBA-generated zone changes (e.g. lethal damage) may put
            // death triggers on the stack that need target/mode prompts before the
            // next SBA pass.
            if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
                state.waiting_for = waiting_for.clone();
                if delayed_event_start.is_some() || payment_owned {
                    let delayed_input = unclaimed_delayed_trigger_input(
                        state,
                        events,
                        delayed_scan_start,
                        &mut consumed_trigger_events,
                        delayed_event_start,
                    );
                    triggers::collect_unclaimed_delayed_triggers_into_deferred(
                        state,
                        events,
                        &delayed_input,
                    );
                }
                persist_carried_recipient_across_prompt(
                    state,
                    carried_priority_recipient,
                    completion_recipient,
                );
                state.consumed_before_priority_trigger_events.clear();
                return Ok(waiting_for);
            }
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
        } else {
            break;
        }
    }

    // CR 610.3: "until this leaves" returns are immediate one-shot effects.
    // A resolving effect can remove the source and then pause for a later
    // SearchChoice (Boseiju) or other resolution choice. Process the return
    // before that choice is surfaced; otherwise the source's ZoneChanged
    // event is lost with this pipeline pass and its exiled card never returns.
    let events_before_exile_returns = events.len();
    let deferred_trigger_count_before_exile_returns = state.deferred_triggers.len();
    // CR 104.1: only while the game is still going. Once this action has recorded
    // a result on `GameState::game_end`, the game has already ended, so a source
    // that left the battlefield must not move its linked exiled card back through
    // the zone-change pipeline. The reachable case is the game-ending elimination
    // itself: CR 800.4a sweeps every object the losing player owns off the
    // battlefield BEFORE `end_game` records the result, so the source's
    // `ZoneChanged` is already sitting in this batch when the pass runs.
    // `game_end` is the authority here, not `waiting_for`: a later step of the
    // same action can overwrite the wait (a CR 616.1 replacement-order prompt on
    // the resolving spell's own zone move), and only
    // `engine::reconcile_terminal_result` restores it at the action boundary. The
    // collection block below is keyed on the events this call appends, so it goes
    // inert with it rather than needing a second guard.
    if state.game_end.is_none() {
        check_exile_returns(state, events);
    }
    if events.len() > events_before_exile_returns {
        let exile_return_events: Vec<_> = events[events_before_exile_returns..].to_vec();
        let consumed_exile_return_events =
            std::mem::take(&mut state.consumed_before_priority_trigger_events);
        let unconsumed_exile_return_events = triggers::filter_consumed_trigger_events(
            &exile_return_events,
            triggers::TriggerCollectionRequester::Ordinary,
            &consumed_exile_return_events,
        );
        // CR 603.3b: Exile-return triggers also join a terminal batch so they
        // cannot be ordered before the final cast trigger is collected, and
        // join the batch of a resolution whose final part ran in this pass.
        if !matches!(state.waiting_for, WaitingFor::Priority { .. })
            || state.pending_resolution_completion.is_some()
            || announced_cast_batch
            || final_part_ran
            || payment_owned
            || completed_resolution_batch
        {
            triggers::collect_triggers_into_deferred(state, &unconsumed_exile_return_events);
        } else {
            let mut normal_pending = state
                .deferred_triggers
                .split_off(deferred_trigger_count_before_exile_returns);
            normal_pending.extend(triggers::collect_triggers_for_batch(
                state,
                &unconsumed_exile_return_events,
            ));
            let outcome = triggers::process_collected_triggers_with_delayed_events(
                state,
                normal_pending,
                &exile_return_events,
                events,
            );
            if let Some(waiting_for) = outcome.prompt {
                state.waiting_for = waiting_for.clone();
                persist_carried_recipient_across_prompt(
                    state,
                    carried_priority_recipient,
                    completion_recipient,
                );
                state.consumed_before_priority_trigger_events.clear();
                return Ok(waiting_for);
            }
        }
    }

    // CR 603.3b: Triggered abilities parked while a resolution choice was open
    // (e.g. "whenever you scry, ..." deferred above so it couldn't clobber the
    // choice's WaitingFor) go on the stack once resolution truly settles. The
    // drain is gated inside `drain_deferred_trigger_queue` (no mid-continuation
    // / mid-spell settles; same-controller groups get `OrderTriggers` first).
    // A drained trigger that itself needs input returns its own WaitingFor,
    // handled by the check below.
    // CR 603.3b: collect the whole completed group before the first drain or
    // emptiness decision, including a group containing only delayed observers.
    if completed_resolution_batch && !payment_owned && state.game_end.is_none() {
        let delayed_input = unclaimed_delayed_trigger_input(
            state,
            events,
            delayed_scan_start,
            &mut consumed_trigger_events,
            delayed_event_start,
        );
        triggers::collect_unclaimed_delayed_triggers_into_deferred(state, events, &delayed_input);
        delayed_scan_start = events.len();
    }
    let carrier_before_terminal = state.resolving_stack_entry.as_ref().map(|entry| entry.id);
    if settle_pending_resolution_completion(state, events) {
        if carrier_before_terminal.is_some()
            && state.resolving_stack_entry.is_none()
            && matches!(state.waiting_for, WaitingFor::Priority { .. })
        {
            effective_default = state.waiting_for.clone();
            completion_recipient = completed_priority_recipient(state);
        }
        if let Some(wf) =
            triggers::drain_deferred_triggers_after_stack_object_announcement(state, events)
        {
            state.waiting_for = wf;
        }
    } else if matches!(state.waiting_for, WaitingFor::Priority { .. })
        && !state.deferred_triggers.is_empty()
    {
        if !payment_owned
            && deferred_trigger_batch_was_sba_choice_parked
            && !delayed_trigger_events.is_empty()
        {
            // This is the one post-answer CR 603.3b ordering attempt owned by
            // the completed SBA choice. Consuming the marker before dispatch
            // ensures a trigger that pauses again re-parks as ordinary work.
            // This branch intentionally combines the answer's delayed events
            // before ordinary drain-policy routing; changing that order would
            // split one SBA/answer trigger batch across two ordering windows.
            // The converse also has to hold: only the SBA-marked partition may
            // join those delayed events. `take_sba_choice_trigger_batch` leaves
            // every ordinary context queued for the drain below, so a
            // construction tail sitting beside the batch is never merged into
            // the answer's ordering window.
            let pending =
                triggers::take_sba_choice_trigger_batch(state, deferred_triggers_before_answer);
            let outcome = triggers::process_collected_triggers_with_delayed_events(
                state,
                pending,
                &delayed_trigger_events,
                events,
            );
            if let Some(wf) = outcome.prompt {
                state.waiting_for = wf;
            }
            // CR 603.3b: `take_sba_choice_trigger_batch` partitions, so any
            // ordinary context queued beside the SBA batch is still here. It
            // belongs to its own ordering window, not the answer's, but it must
            // not be stranded either — hand it to the regular drain, which
            // self-gates on `can_drain_deferred_triggers` and simply declines
            // when this pass is not an eligible drain point.
            if matches!(state.waiting_for, WaitingFor::Priority { .. })
                && !state.deferred_triggers.is_empty()
            {
                if let Some(wf) =
                    triggers::drain_deferred_trigger_queue_with_policy(state, events, drain_policy)
                {
                    state.waiting_for = wf;
                }
            }
        } else {
            // CR 603.3: the observers of spells cast and announced during a
            // finished resolution go on the stack above those spells.
            let policy = if announced_cast_batch || final_part_ran || completed_resolution_batch {
                // CR 603.2 + CR 603.3b: the action's delayed observers (of its
                // own events, its state-based actions and its exile returns)
                // join the announced-cast or final-part batch, through the one
                // delayed scan the bottom of this pass would otherwise make.
                // Scanned here, before the drain can stop on an ordering
                // prompt; the bottom scan then reads only what the drain
                // appends, so each occurrence is scanned once.
                let delayed_input = unclaimed_delayed_trigger_input(
                    state,
                    events,
                    delayed_scan_start,
                    &mut consumed_trigger_events,
                    delayed_event_start,
                );
                triggers::collect_unclaimed_delayed_triggers_into_deferred(
                    state,
                    events,
                    &delayed_input,
                );
                delayed_scan_start = events.len();
                if announced_cast_batch {
                    DeferredTriggerDrainPolicy::SettledPriority
                } else {
                    drain_policy
                }
            } else {
                drain_policy
            };
            if let Some(wf) =
                triggers::drain_deferred_trigger_queue_with_policy(state, events, policy)
            {
                state.waiting_for = wf;
            }
        }
    }

    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        // CR 603.2c + CR 603.7b: a further canonical prompt retains fresh
        // delayed occurrences now; its later answer has a different buffer.
        if (delayed_event_start.is_some() || payment_owned) && state.game_end.is_none() {
            let delayed_input = unclaimed_delayed_trigger_input(
                state,
                events,
                delayed_scan_start,
                &mut consumed_trigger_events,
                delayed_event_start,
            );
            triggers::collect_unclaimed_delayed_triggers_into_deferred(
                state,
                events,
                &delayed_input,
            );
        }
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            match_flow::handle_game_over_transition(state);
        }
        persist_carried_recipient_across_prompt(
            state,
            carried_priority_recipient,
            completion_recipient,
        );
        state.consumed_before_priority_trigger_events.clear();
        return Ok(state.waiting_for.clone());
    }

    // CR 800.4: If SBAs eliminated the player who was about to receive priority,
    // respect the reassignment that eliminate_player() already performed.
    if let Some(player) = effective_default.acting_player() {
        if !players::is_alive(state, player) {
            state.consumed_before_priority_trigger_events.clear();
            return Ok(state.waiting_for.clone());
        }
    }

    let delayed_input = unclaimed_delayed_trigger_input(
        state,
        events,
        delayed_scan_start,
        &mut consumed_trigger_events,
        delayed_event_start,
    );
    if payment_owned {
        triggers::collect_unclaimed_delayed_triggers_into_deferred(state, events, &delayed_input);
        state.consumed_before_priority_trigger_events.clear();
        return Ok(state.waiting_for.clone());
    }
    let delayed_events = triggers::check_delayed_triggers(state, &delayed_input.events);
    events.extend(delayed_events);
    state.consumed_before_priority_trigger_events.clear();

    // CR 603.3b: check_delayed_triggers may have paused the batch on a same-controller
    // ordering choice; surface it before check_state_triggers / the priority fallthrough
    // clobber it. Scoped to OrderTriggers so the Breeches target-selection pause (which
    // sets pending_trigger and is re-derived at begin_pending_trigger_target_selection)
    // is untouched.
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        persist_carried_recipient_across_prompt(
            state,
            carried_priority_recipient,
            completion_recipient,
        );
        return Ok(state.waiting_for.clone());
    }

    // CR 603.8: Check state triggers after event-based triggers.
    // State triggers fire when a condition is true, checked whenever a player
    // would receive priority.
    triggers::check_state_triggers(state);

    if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
        state.waiting_for = waiting_for.clone();
        persist_carried_recipient_across_prompt(
            state,
            carried_priority_recipient,
            completion_recipient,
        );
        return Ok(waiting_for);
    }

    if state.stack.len() > stack_before {
        let outgoing = flush_pending_priority_intercepts(
            state,
            settled_priority_wait.clone(),
            effective_default.acting_player(),
        );
        return Ok(outgoing);
    }

    super::layers::flush_layers(state);

    Ok(flush_pending_priority_intercepts(
        state,
        effective_default.clone(),
        effective_default.acting_player(),
    ))
}

/// CR 603.2 + CR 603.2c + CR 603.7: the input of a post-action pass's delayed
/// scan: `events[start..]` minus every occurrence a collector has claimed. The
/// claims still in the pass ledger are folded into `consumed` first. The
/// announced-cast batch and the final scan both read through here; the final
/// scan starts where the batch's scan ended, so no occurrence is scanned twice
/// whatever ordinals the batch's own claims carry. The kept events remember
/// their buffer positions, so a claim the batch's scan publishes is the
/// full-buffer occurrence it matched (CR 603.2c).
pub(super) fn unclaimed_delayed_trigger_input(
    state: &mut GameState,
    events: &[GameEvent],
    start: usize,
    consumed: &mut Vec<triggers::ConsumedTriggerEventOccurrence>,
    delivery_start: Option<usize>,
) -> triggers::UnclaimedTriggerEvents {
    consumed.extend(std::mem::take(
        &mut state.consumed_before_priority_trigger_events,
    ));
    let mut input = triggers::unclaimed_trigger_events_from(
        events,
        start,
        triggers::TriggerCollectionRequester::Delayed,
        consumed,
    );
    if delivery_start.is_some()
        || state.payment_transaction.is_some()
        || state.payment_transaction_replay
    {
        let mut retained = retained_logical_zone_events(state);
        retained.extend(retained_cost_events(state));
        input.retain(|event| {
            !matches!(event, GameEvent::PhaseChanged { .. })
                && !state.deferred_entry_events.contains(event)
                && !retained.contains(event)
        });
    }
    input
}

/// CR 603.2c: exact cost occurrences still retained by their typed payment
/// owner, including suspended parent mana cursors, await that owner's scan.
pub(crate) fn retained_cost_events(state: &GameState) -> Vec<GameEvent> {
    use crate::types::game_state::PendingCostMoveResume;
    let mut retained = Vec::new();
    if let Some(PendingCostMoveResume::SacrificeForCost {
        deferred_cost_events,
        ..
    }) = state.pending_cost_move_resume.as_ref()
    {
        retained.extend(deferred_cost_events.iter().cloned());
    }
    if let Some(PendingCostMoveResume::ManaAbilityPayment { cursor, .. }) =
        state.pending_cost_move_resume.as_ref()
    {
        let mut cursor = cursor;
        loop {
            retained.extend(cursor.deferred_cost_events.iter().cloned());
            let Some(parent) = cursor.parent.as_ref() else {
                break;
            };
            cursor = parent.cursor.as_ref();
        }
    }
    retained
}

/// CR 903.9a + CR 704.5j + CR 310.11: SBA-owned commander, legend, and
/// battle-protector choices park a trigger batch that must merge with answer
/// events before CR 603.3b ordering. Replacement choices are instead owned by
/// `pending_replacement`'s resolution path and must not join this batch.
fn sba_choice_requires_combined_trigger_batch(waiting_for: &WaitingFor) -> bool {
    matches!(
        waiting_for,
        WaitingFor::CommanderZoneChoice { .. }
            | WaitingFor::ChooseLegend { .. }
            | WaitingFor::BattleProtectorChoice { .. }
    )
}

/// CR 117.3c + CR 117.5: persist a carried priority recipient across any
/// ordering or trigger-construction prompt this pipeline pass just installed, so
/// the construction finisher can hand priority back to that player once the
/// batch has finished announcing.
///
/// CR 117.3b + CR 117.5: a pass that retired its exact resolving carrier
/// supplies that completion's synchronized active-player recipient here as
/// well, so constructing the batch it completes cannot hand priority to the
/// trigger controller instead. A carried recipient takes precedence.
///
/// CR 800.4a + CR 800.4j: the completion recipient can name a player who has
/// left the game: the early site reads it before this pass's state-based
/// actions, and elimination re-points only an already installed recipient.
/// If that player has since left the game, the next player in turn order
/// still in the game receives priority instead, so it is the one persisted.
///
/// With no recipient — every ordinary wrapper that retired no carrier — this is
/// a no-op, and the existing active-player/controller fallbacks at each
/// construction seam are untouched. When ordering opens, the same player is
/// stored in both authorities: `PendingTriggerOrder::resume_after_ordering`
/// (which `handle_order_triggers` consumes and then drops) and the durable
/// recipient (which stays authoritative across every early return after that
/// order carrier is taken).
fn persist_carried_recipient_across_prompt(
    state: &mut GameState,
    carried_priority_recipient: Option<PlayerId>,
    completion_recipient: Option<PlayerId>,
) {
    let Some(player) = carried_priority_recipient.or_else(|| {
        completion_recipient.map(|player| {
            if players::is_alive(state, player) {
                player
            } else {
                players::next_player_in_turn_order(state, player)
            }
        })
    }) else {
        return;
    };
    if !triggers::is_trigger_construction_prompt(&state.waiting_for) {
        return;
    }
    if matches!(state.waiting_for, WaitingFor::OrderTriggers { .. }) {
        triggers::preserve_order_triggers_resume(state, WaitingFor::Priority { player });
    }
    triggers::preserve_trigger_construction_priority_recipient(state, player);
}

/// CR 117.3b: the player a completed resolution's synchronized wait gives
/// priority to. CR 117.4: none when the settlement granted no priority because
/// a player had already passed in that window; the pass succession, not the
/// completion, owns that window's recipient.
fn completed_priority_recipient(state: &GameState) -> Option<PlayerId> {
    if !super::engine::completed_resolution_grants_priority(state) {
        return None;
    }
    match &state.waiting_for {
        WaitingFor::Priority { player } => Some(*player),
        _ => None,
    }
}

/// The zone-change occurrences a paused logical zone-change owner retains for
/// its own final settlement (CR 603.2c: collected once, by that owner).
pub(crate) fn retained_logical_zone_events(state: &GameState) -> Vec<GameEvent> {
    let mut retained = Vec::new();
    if let Some(pending) = state
        .active_change_zone_frame()
        .and_then(|frame| frame.pending.as_ref())
    {
        retained.extend(
            pending
                .logical_zone_change_group
                .all_origin_occurrences
                .iter()
                .map(|occurrence| occurrence.event.clone()),
        );
        if let Some(paused_current) = pending.paused_current.as_ref() {
            retained.extend(paused_current.delivery_events.iter().cloned());
        }
    }
    if let Some(pending) = state.active_batch_delivery() {
        retained.extend(
            pending
                .logical_zone_change_group
                .all_origin_occurrences
                .iter()
                .map(|occurrence| occurrence.event.clone()),
        );
        if let Some(paused_current) = pending.paused_current.as_ref() {
            retained.extend(paused_current.delivery_events.iter().cloned());
        }
    }
    retained
}

/// CR 603.3b + CR 608.2g: settles a terminal resolution marker only after its
/// final free cast has actually been announced. The drain uses the
/// stack-announcement boundary so B/C's triggers may be ordered above their
/// still-stacked spells, rather than the ordinary resolution drain's spell guard.
pub(super) fn settle_pending_resolution_completion(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> bool {
    let Some(completion) = state.pending_resolution_completion.as_ref() else {
        return false;
    };
    let final_cast = completion.final_cast;
    if !matches!(state.waiting_for, WaitingFor::Priority { .. })
        || !triggers::resolution_completion_can_settle(state)
    {
        return false;
    }
    if final_cast.is_some_and(|object_id| {
        !state
            .objects
            .get(&object_id)
            .is_some_and(|object| object.zone == crate::types::zones::Zone::Stack)
    }) {
        return false;
    }
    let Some(carrier) = state.resolving_stack_entry.as_ref().map(|entry| entry.id) else {
        return false;
    };
    // CR 608.2c: remove only this terminal marker while checking every other
    // instruction owner. Refusal leaves the terminal obligation installed.
    let completion = state.pending_resolution_completion.take();
    if !super::engine::resolution_instructions_are_done(state) {
        state.pending_resolution_completion = completion;
        return false;
    }
    ensure_terminal_cast_spell_triggers_collected(state, final_cast);
    // CR 608.2n: the rippled spell's own final part runs first, through the
    // single final-part authority; should it park a question of its own, the
    // carrier stays and the parked triggers wait for the ordinary
    // priority-boundary drain instead of the post-announcement one. A carrier
    // that owes no final part is retired here as at any completed resolution.
    if super::stack::resolving_spell_owes_final_part(state) {
        super::engine::run_spell_final_part_if_due(state, events);
    } else {
        super::engine::settle_resolving_stack_entry_after_continuation_resume(state);
    }
    state
        .resolving_stack_entry
        .as_ref()
        .is_none_or(|entry| entry.id != carrier)
}

/// CR 603.2 + CR 603.3b + CR 608.2g: replacement-resume casts can complete
/// below the ordinary event-scan seam. At terminal Ripple settlement, recover
/// the one authoritative SpellCast event from the fully announced spell if it
/// has not already been collected into this ordering batch.
fn ensure_terminal_cast_spell_triggers_collected(
    state: &mut GameState,
    final_cast: Option<ObjectId>,
) {
    let Some(object_id) = final_cast else {
        return;
    };
    let already_collected = state.deferred_triggers.iter().any(|context| {
        matches!(
            context.pending.trigger_event.as_ref(),
            Some(GameEvent::SpellCast { object_id: event_id, .. }) if *event_id == object_id
        )
    });
    if already_collected {
        return;
    }
    let Some(object) = state.objects.get(&object_id) else {
        return;
    };
    let event = GameEvent::SpellCast {
        card_id: object.card_id,
        controller: object.controller,
        object_id,
        cast_mana_value: Some(object.spell_mana_value()),
    };
    triggers::collect_triggers_into_deferred(state, &[event]);
}

fn flush_pending_priority_intercepts(
    state: &mut GameState,
    outgoing: WaitingFor,
    semantic_caster: Option<crate::types::player::PlayerId>,
) -> WaitingFor {
    let outgoing = super::effects::paradigm::flush_pending_remaining_offers(state, outgoing);
    let outgoing = flush_pending_miracle_offer(state, outgoing);
    match semantic_caster {
        Some(caster) => {
            super::precast_copy_shortcut::maybe_offer_after_cast_triggers(state, caster, outgoing)
        }
        None => outgoing,
    }
}

/// CR 702.94a + CR 603.11: Intercept a `WaitingFor::Priority` and replace it
/// with the head of `pending_miracle_offers` as `WaitingFor::MiracleReveal`,
/// dropping the queued offer so a subsequent Priority flush picks up the next
/// one (or returns the original Priority if the queue is empty).
///
/// Pass-through for any non-Priority `WaitingFor`: miracle prompts only
/// interrupt the normal priority window, not nested choices (mana payment,
/// target selection, etc.) that must complete before priority is granted.
///
/// Stale-offer filtering: offers whose `object_id` is no longer in the offer
/// player's hand (moved/exiled/destroyed between queue time and flush) are
/// discarded without prompting — the reveal is offered "as you draw it" per
/// CR 702.94a, and the card can no longer be revealed from hand.
fn flush_pending_miracle_offer(state: &mut GameState, outgoing: WaitingFor) -> WaitingFor {
    if !matches!(outgoing, WaitingFor::Priority { .. }) {
        return outgoing;
    }
    // `pop_next_live_miracle_offer` already drains stale entries internally,
    // so a single pop is sufficient here. Consume the offer regardless of the
    // player's eventual accept/decline so the queue progresses even if the
    // same spell's resolution queued multiple offers for the same player.
    match pop_next_live_miracle_offer(state) {
        Some(offer) => WaitingFor::MiracleReveal {
            player: offer.player,
            object_id: offer.object_id,
            cost: offer.cost,
        },
        None => outgoing,
    }
}

/// Pop the next `MiracleOffer` whose `object_id` is still in the player's
/// hand. Stale offers (card left the hand) are discarded. Returns `None`
/// when the queue is empty or contains only stale entries.
fn pop_next_live_miracle_offer(
    state: &mut GameState,
) -> Option<crate::types::game_state::MiracleOffer> {
    while !state.pending_miracle_offers.is_empty() {
        let offer = state.pending_miracle_offers.remove(0);
        let still_in_hand = state.objects.get(&offer.object_id).is_some_and(|obj| {
            obj.zone == crate::types::zones::Zone::Hand && obj.owner == offer.player
        });
        if still_in_hand {
            return Some(offer);
        }
    }
    None
}
