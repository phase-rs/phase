//! Bounds for engine snapshot components before clone-heavy `StateUpdate` fan-out.
//!
//! Inbound `GameAction` payloads are capped by `game_action_payload_guard`, but a
//! single reducer step can still produce very large `GameState` / log / legal-action
//! snapshots. Broadcasting those to every seat and spectator clones them per
//! recipient; this module rejects pathological snapshots before filtering.

use std::collections::HashMap;

use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::log::GameLogEntry;
use engine::types::mana::ManaCost;

use crate::game_action_payload_guard::MAX_ACTION_LIST_LEN;

/// Max permanents/objects in a snapshot eligible for wire fan-out.
pub const MAX_SNAPSHOT_OBJECTS: usize = MAX_ACTION_LIST_LEN;
/// Max events attached to a single `StateUpdate`.
pub const MAX_SNAPSHOT_EVENTS: usize = 2_000;
/// Max log lines attached to a single `StateUpdate`.
pub const MAX_SNAPSHOT_LOG_ENTRIES: usize = 5_000;
/// Max legal actions in a single `StateUpdate` (aligned with action list cap).
pub const MAX_SNAPSHOT_LEGAL_ACTIONS: usize = MAX_ACTION_LIST_LEN;
/// Max spell-cost entries in a single `StateUpdate`.
pub const MAX_SNAPSHOT_SPELL_COSTS: usize = 1_000;
/// Max total legal actions across all per-object buckets in one update.
pub const MAX_SNAPSHOT_LEGAL_ACTIONS_BY_OBJECT_TOTAL: usize = MAX_ACTION_LIST_LEN;

/// Borrowed snapshot components from an [`crate::session::ActionResult`]-shaped tuple.
pub struct StateSnapshotParts<'a> {
    pub state: &'a GameState,
    pub events: &'a [GameEvent],
    pub log_entries: &'a [GameLogEntry],
    pub legal_actions: &'a [GameAction],
    pub legal_actions_by_object: &'a HashMap<ObjectId, Vec<GameAction>>,
    pub spell_costs: &'a HashMap<ObjectId, ManaCost>,
}

fn bound_count(field: &str, len: usize, max: usize) -> Result<(), String> {
    if len > max {
        Err(format!(
            "{field} has {len} entries; at most {max} allowed for broadcast"
        ))
    } else {
        Ok(())
    }
}

fn legal_actions_by_object_total(map: &HashMap<ObjectId, Vec<GameAction>>) -> usize {
    map.values().map(Vec::len).sum()
}

/// Validate snapshot sizes before `filter_state_for_player` and per-connection clones.
pub fn guard_state_snapshot_broadcast(parts: StateSnapshotParts<'_>) -> Result<(), String> {
    bound_count(
        "state.objects",
        parts.state.objects.len(),
        MAX_SNAPSHOT_OBJECTS,
    )?;
    bound_count("events", parts.events.len(), MAX_SNAPSHOT_EVENTS)?;
    bound_count(
        "log_entries",
        parts.log_entries.len(),
        MAX_SNAPSHOT_LOG_ENTRIES,
    )?;
    bound_count(
        "legal_actions",
        parts.legal_actions.len(),
        MAX_SNAPSHOT_LEGAL_ACTIONS,
    )?;
    bound_count(
        "spell_costs",
        parts.spell_costs.len(),
        MAX_SNAPSHOT_SPELL_COSTS,
    )?;
    bound_count(
        "legal_actions_by_object",
        legal_actions_by_object_total(parts.legal_actions_by_object),
        MAX_SNAPSHOT_LEGAL_ACTIONS_BY_OBJECT_TOTAL,
    )?;
    Ok(())
}

/// Validate a bare `GameState` before spectator `GameStarted` filtering.
pub fn guard_game_state_for_broadcast(state: &GameState) -> Result<(), String> {
    bound_count("state.objects", state.objects.len(), MAX_SNAPSHOT_OBJECTS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::log::{LogCategory, LogSegment};
    use engine::types::phase::Phase;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn sample_log_entry(seq: u32) -> GameLogEntry {
        GameLogEntry {
            seq,
            turn: 1,
            phase: Phase::PreCombatMain,
            category: LogCategory::Game,
            segments: vec![LogSegment::Text("test".to_string())],
        }
    }

    fn empty_parts<'a>(
        state: &'a GameState,
        legal_actions_by_object: &'a HashMap<ObjectId, Vec<GameAction>>,
        spell_costs: &'a HashMap<ObjectId, ManaCost>,
    ) -> StateSnapshotParts<'a> {
        StateSnapshotParts {
            state,
            events: &[],
            log_entries: &[],
            legal_actions: &[],
            legal_actions_by_object,
            spell_costs,
        }
    }

    #[test]
    fn snapshot_accepts_empty_board() {
        let state = GameState::new_two_player(1);
        let legal_actions_by_object = HashMap::new();
        let spell_costs = HashMap::new();
        assert!(guard_state_snapshot_broadcast(empty_parts(
            &state,
            &legal_actions_by_object,
            &spell_costs
        ))
        .is_ok());
    }

    #[test]
    fn snapshot_rejects_oversized_object_map() {
        let mut state = GameState::new_two_player(1);
        for i in 0..=MAX_SNAPSHOT_OBJECTS {
            create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("Card{i}"),
                Zone::Hand,
            );
        }
        let legal_actions_by_object = HashMap::new();
        let spell_costs = HashMap::new();
        let err = guard_state_snapshot_broadcast(empty_parts(
            &state,
            &legal_actions_by_object,
            &spell_costs,
        ))
        .unwrap_err();
        assert!(err.contains("state.objects"));
    }

    #[test]
    fn snapshot_rejects_oversized_event_batch() {
        let state = GameState::new_two_player(1);
        let events = vec![
            GameEvent::PriorityPassed {
                player_id: PlayerId(0),
            };
            MAX_SNAPSHOT_EVENTS + 1
        ];
        let legal_actions_by_object = HashMap::new();
        let spell_costs = HashMap::new();
        let parts = StateSnapshotParts {
            state: &state,
            events: &events,
            log_entries: &[],
            legal_actions: &[],
            legal_actions_by_object: &legal_actions_by_object,
            spell_costs: &spell_costs,
        };
        let err = guard_state_snapshot_broadcast(parts).unwrap_err();
        assert!(err.contains("events"));
    }

    #[test]
    fn snapshot_rejects_oversized_log_batch() {
        let state = GameState::new_two_player(1);
        let log_entries: Vec<_> = (0..=MAX_SNAPSHOT_LOG_ENTRIES)
            .map(|seq| sample_log_entry(seq as u32))
            .collect();
        let legal_actions_by_object = HashMap::new();
        let spell_costs = HashMap::new();
        let parts = StateSnapshotParts {
            state: &state,
            events: &[],
            log_entries: &log_entries,
            legal_actions: &[],
            legal_actions_by_object: &legal_actions_by_object,
            spell_costs: &spell_costs,
        };
        let err = guard_state_snapshot_broadcast(parts).unwrap_err();
        assert!(err.contains("log_entries"));
    }

    #[test]
    fn snapshot_rejects_oversized_legal_action_list() {
        let state = GameState::new_two_player(1);
        let legal_actions = vec![GameAction::PassPriority; MAX_SNAPSHOT_LEGAL_ACTIONS + 1];
        let legal_actions_by_object = HashMap::new();
        let spell_costs = HashMap::new();
        let parts = StateSnapshotParts {
            state: &state,
            events: &[],
            log_entries: &[],
            legal_actions: &legal_actions,
            legal_actions_by_object: &legal_actions_by_object,
            spell_costs: &spell_costs,
        };
        let err = guard_state_snapshot_broadcast(parts).unwrap_err();
        assert!(err.contains("legal_actions"));
    }

    #[test]
    fn game_state_guard_rejects_oversized_objects() {
        let mut state = GameState::new_two_player(1);
        for i in 0..=MAX_SNAPSHOT_OBJECTS {
            create_object(
                &mut state,
                CardId(i as u64 + 1),
                PlayerId(0),
                format!("Card{i}"),
                Zone::Hand,
            );
        }
        let err = guard_game_state_for_broadcast(&state).unwrap_err();
        assert!(err.contains("state.objects"));
    }
}
