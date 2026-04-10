use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};

use super::engine::{begin_pending_trigger_target_selection, check_exile_returns, EngineError};
use super::match_flow;
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
    if !skip_trigger_scan {
        let filtered_events: Vec<_> = events
            .iter()
            .filter(|event| !matches!(event, GameEvent::PhaseChanged { .. }))
            .cloned()
            .collect();
        triggers::process_triggers(state, &filtered_events);
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

    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            match_flow::handle_game_over_transition(state);
        }
        return Ok(state.waiting_for.clone());
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
        return Ok(WaitingFor::Priority {
            player: state.active_player,
        });
    }

    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    Ok(default_wf.clone())
}
