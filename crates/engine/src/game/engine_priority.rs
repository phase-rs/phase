use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

use super::engine::{begin_pending_trigger_target_selection, check_exile_returns, EngineError};
use super::match_flow;
use super::players;
use super::sba;
use super::triggers;

pub(super) fn run_post_action_pipeline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
) -> Result<WaitingFor, EngineError> {
    run_post_action_pipeline_from(state, events, 0, default_wf, skip_trigger_scan)
}

/// Run the normal post-action settlement while scanning only events produced at
/// or after `event_start`. Use for nested resume paths that carry earlier
/// payment/choice events in the same output buffer.
pub(super) fn run_post_action_pipeline_from(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
    event_start: usize,
    default_wf: &WaitingFor,
    skip_trigger_scan: bool,
) -> Result<WaitingFor, EngineError> {
    // Capture stack depth before any trigger/SBA processing so we can detect
    // whether new triggered abilities were added during this pipeline pass.
    let stack_before = state.stack.len();

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
    if !skip_trigger_scan {
        let filtered_events: Vec<_> = events[event_start..]
            .iter()
            .filter(|event| {
                !matches!(event, GameEvent::PhaseChanged { .. })
                    && !state.deferred_entry_events.contains(event)
            })
            .cloned()
            .collect();
        // CR 603.3b: If the resolution step that just ran paused for a player
        // resolution-choice (Scry/Surveil/Dig/Search/...), the triggered
        // abilities it generated (e.g. "whenever you scry, ...") must NOT be
        // collected and ordered now — doing so overwrites the pending choice's
        // WaitingFor (the `OrderTriggers` PromptForChoice arm clobbers
        // `ScryChoice` when 2+ same-controller triggers fire). Park them in
        // `deferred_triggers`; they are drained below once the action settles
        // back to Priority. Mirrors `batch_or_drain_observer_triggers`' B2 branch.
        if super::engine_resolution_choices::handles(&state.waiting_for) {
            triggers::collect_triggers_into_deferred(state, &filtered_events);
        } else {
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
            break;
        }
        if events.len() > events_before {
            let sba_events: Vec<_> = events[events_before..].to_vec();
            triggers::process_triggers(state, &sba_events);
            // CR 603.3d: SBA-generated zone changes (e.g. lethal damage) may put
            // death triggers on the stack that need target/mode prompts before the
            // next SBA pass.
            if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
                state.waiting_for = waiting_for.clone();
                return Ok(waiting_for);
            }
            if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
        } else {
            break;
        }
    }

    // CR 603.3b: Triggered abilities parked while a resolution choice was open
    // (e.g. "whenever you scry, ..." deferred above so it couldn't clobber the
    // choice's WaitingFor) go on the stack once resolution truly settles. The
    // drain is gated inside `drain_deferred_trigger_queue` (no mid-continuation
    // / mid-spell settles; same-controller groups get `OrderTriggers` first).
    // A drained trigger that itself needs input returns its own WaitingFor,
    // handled by the check below.
    if matches!(state.waiting_for, WaitingFor::Priority { .. })
        && !state.deferred_triggers.is_empty()
    {
        if let Some(wf) = triggers::drain_deferred_trigger_queue(state, events) {
            state.waiting_for = wf;
        }
    }

    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            match_flow::handle_game_over_transition(state);
        }
        return Ok(state.waiting_for.clone());
    }

    // CR 800.4: If SBAs eliminated the player who was about to receive priority,
    // respect the reassignment that eliminate_player() already performed.
    if let Some(player) = default_wf.acting_player() {
        if !players::is_alive(state, player) {
            return Ok(state.waiting_for.clone());
        }
    }

    check_exile_returns(state, events);

    let delayed_events = triggers::check_delayed_triggers(state, events);
    events.extend(delayed_events);

    // CR 603.8: Check state triggers after event-based triggers.
    // State triggers fire when a condition is true, checked whenever a player
    // would receive priority.
    triggers::check_state_triggers(state);

    if let Some(waiting_for) = begin_pending_trigger_target_selection(state)? {
        state.waiting_for = waiting_for.clone();
        return Ok(waiting_for);
    }

    if state.stack.len() > stack_before {
        return Ok(flush_pending_priority_intercepts(
            state,
            WaitingFor::Priority {
                player: state.active_player,
            },
        ));
    }

    super::layers::flush_layers(state);

    Ok(flush_pending_priority_intercepts(state, default_wf.clone()))
}

fn flush_pending_priority_intercepts(state: &mut GameState, outgoing: WaitingFor) -> WaitingFor {
    let outgoing = super::effects::paradigm::flush_pending_remaining_offers(state, outgoing);
    flush_pending_miracle_offer(state, outgoing)
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
