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

    // CR 701.21: "Can't be sacrificed" prevents this action. The effect/cost
    // invoking sacrifice resolves as if no permanent was sacrificed — no
    // graveyard move, no leaves-the-battlefield triggers, no events emitted.
    if crate::game::static_abilities::object_has_static_other(state, object_id, "CantBeSacrificed")
    {
        return Ok(SacrificeOutcome::Complete);
    }

    let proposed = ProposedEvent::Sacrifice {
        object_id,
        player_id: player,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_sacrifice_after_replacement(state, event, events)
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(choice_player) => {
            return Ok(SacrificeOutcome::NeedsReplacementChoice(choice_player));
        }
    }

    Ok(SacrificeOutcome::Complete)
}

/// CR 701.21a + CR 614: Apply an accepted Sacrifice proposed event.
///
/// Moves the permanent to its owner's graveyard (or the replaced destination
/// if a `Moved` replacement redirected the inner zone change), records the
/// sacrifice for restriction tracking (CR 701.21), and emits the
/// `PermanentSacrificed` event.
///
/// Shared by the cost/effect path (`sacrifice_permanent`) and the
/// post-replacement-choice delivery path (`handle_replacement_choice`).
pub(crate) fn apply_sacrifice_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) {
    match event {
        ProposedEvent::Sacrifice {
            object_id: oid,
            player_id: pid,
            ..
        } => {
            restrictions::record_sacrifice(state, oid, pid);
            zones::move_to_zone(state, oid, Zone::Graveyard, events);
            crate::game::layers::mark_layers_full(state);
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
            crate::game::layers::mark_layers_full(state);
        }
        _ => {}
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

        assert!(matches!(result, Ok(SacrificeOutcome::Complete)));
        // Permanent is still on the battlefield — sacrifice was a no-op.
        assert!(state.battlefield.contains(&victim));
        assert!(!state.players[0].graveyard.contains(&victim));
        // No PermanentSacrificed event was emitted.
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PermanentSacrificed { object_id, .. } if *object_id == victim
        )));
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
