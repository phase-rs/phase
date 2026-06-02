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
    // Capture stack depth before any trigger/SBA processing so we can detect
    // whether new triggered abilities were added during this pipeline pass.
    let stack_before = state.stack.len();

    // CR 603.2: Triggered abilities trigger at the moment the event occurs.
    // Scan for triggers BEFORE SBAs so that objects still on the battlefield
    // (e.g., a creature that just took lethal damage) are found by the scan.
    // This follows the same pattern as process_combat_damage_triggers in combat_damage.rs.
    //
    // CR 614.12a + CR 707.9: Mid-entry `CopyTargetChoice` deferral happens at
    // the producer site (`apply_pending_post_replacement_effect`), which both
    // emits the entering object's `ZoneChanged` and decides whether to pause
    // for a copy choice. By the time the pipeline reaches this trigger scan,
    // events that should be deferred have already been moved into
    // `state.deferred_entry_events` for replay by `handle_copy_target_choice`.
    if !skip_trigger_scan {
        let filtered_events: Vec<_> = events
            .iter()
            .filter(|event| !matches!(event, GameEvent::PhaseChanged { .. }))
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
    loop {
        let events_before = events.len();
        sba::check_state_based_actions(state, events);
        if events.len() > events_before {
            let sba_events: Vec<_> = events[events_before..].to_vec();
            triggers::process_triggers(state, &sba_events);
        } else {
            break;
        }
    }

    // CR 603.3b: Triggered abilities parked while a resolution choice was open
    // (e.g. "whenever you scry, ..." deferred above so it couldn't clobber the
    // choice's WaitingFor) go on the stack now that the action has settled and a
    // player is about to receive priority. Gated on `Priority` so it never fires
    // mid-trigger-pause (where `dispatch_collected_triggers` parks remaining
    // contexts under a non-Priority WaitingFor and a dedicated site drains them
    // after the active trigger resolves). A drained trigger that itself needs
    // input returns its own WaitingFor, handled by the check below.
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
        return Ok(flush_pending_miracle_offer(
            state,
            WaitingFor::Priority {
                player: state.active_player,
            },
        ));
    }

    super::layers::flush_layers(state);

    Ok(flush_pending_miracle_offer(state, default_wf.clone()))
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
