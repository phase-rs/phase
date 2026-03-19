use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::restrictions;
use crate::game::zones;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
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
}

/// CR 701.17 + CR 118.3: Sacrifice a permanent — move to graveyard as a cost or effect.
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

    let proposed = ProposedEvent::Sacrifice {
        object_id,
        player_id: player,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => match event {
            ProposedEvent::Sacrifice {
                object_id: oid,
                player_id: pid,
                ..
            } => {
                zones::move_to_zone(state, oid, Zone::Graveyard, events);
                state.layers_dirty = true;
                restrictions::record_sacrifice(state, oid, pid);
                events.push(GameEvent::PermanentSacrificed {
                    object_id: oid,
                    player_id: pid,
                });
            }
            ProposedEvent::ZoneChange {
                object_id: oid, to, ..
            } => {
                // Replacement redirected (e.g., exile instead of graveyard)
                zones::move_to_zone(state, oid, to, events);
                state.layers_dirty = true;
            }
            _ => {}
        },
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(choice_player) => {
            return Ok(SacrificeOutcome::NeedsReplacementChoice(choice_player));
        }
    }

    Ok(SacrificeOutcome::Complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::CardId;

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
