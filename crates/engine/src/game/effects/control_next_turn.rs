use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, ScheduledTurnControl};

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ControlNextTurn {
        grant_extra_turn_after,
        window,
        ..
    } = &ability.effect
    else {
        return Err(EffectError::MissingParam(
            "expected ControlNextTurn effect".into(),
        ));
    };

    let Some(TargetRef::Player(target_player)) = ability.targets.first() else {
        return Err(EffectError::InvalidParam(
            "ControlNextTurn requires a player target".into(),
        ));
    };

    // CR 805.8: With shared team turns, controlling a player means controlling
    // that player's team; store the team's seat-order representative as anchor.
    let target_player =
        crate::game::topology::normalize_shared_turn_recipient(state, *target_player);
    // CR 723.1a: player-controlling effects overwrite one another by creation
    // time, so retain provenance when this resolved effect is scheduled.
    let timestamp = state.next_timestamp();

    // CR 723.1a: Deduplicate inactive future effects for this target, but keep
    // the exact entry that created a control effect already governing the
    // current turn/phase. A newly resolved "next turn" effect is future-facing
    // and must not erase the active effect's release identity.
    let active_target =
        crate::game::topology::normalize_shared_turn_recipient(state, state.active_player);
    let active_identities = if active_target == target_player {
        [
            crate::game::turn_control::active_control_identity(
                state,
                target_player,
                crate::types::ability::ControlWindow::NextTurn,
            ),
            crate::game::turn_control::active_control_identity(
                state,
                target_player,
                crate::types::ability::ControlWindow::NextCombatPhase,
            ),
        ]
    } else {
        [None, None]
    };
    state.scheduled_turn_controls.retain(|scheduled| {
        scheduled.target_player != target_player
            || scheduled.window != *window
            || active_identities
                .into_iter()
                .flatten()
                .any(|identity| crate::game::turn_control::control_identity(*scheduled) == identity)
    });
    state.scheduled_turn_controls.push(ScheduledTurnControl {
        target_player,
        controller: ability.controller,
        timestamp,
        grant_extra_turn_after: *grant_extra_turn_after,
        // CR 723.1 / CR 723.2: schedule under the parsed window. Deduplication
        // is window-scoped so full-turn and combat-phase controls can coexist.
        window: *window,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ControlNextTurn,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::ControlWindow;
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    #[test]
    fn resolve_preserves_active_control_while_replacing_future_control() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(1);
        state.turn_decision_controller = Some(PlayerId(0));
        state.turn_decision_control_timestamp = Some(0);
        state.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: PlayerId(1),
            controller: PlayerId(0),
            timestamp: 0,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        });

        let ability = ResolvedAbility::new(
            Effect::ControlNextTurn {
                target: crate::types::ability::TargetFilter::Player,
                grant_extra_turn_after: true,
                window: ControlWindow::NextTurn,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.scheduled_turn_controls.len(), 2);
        assert_eq!(state.scheduled_turn_controls[0].timestamp, 0);
        assert_eq!(
            state.scheduled_turn_controls[1],
            ScheduledTurnControl {
                target_player: PlayerId(1),
                controller: PlayerId(1),
                timestamp: 1,
                grant_extra_turn_after: true,
                window: ControlWindow::NextTurn,
            }
        );
        assert_eq!(state.turn_decision_controller, Some(PlayerId(0)));
        assert_eq!(state.turn_decision_control_timestamp, Some(0));
    }

    #[test]
    fn resolve_deduplicates_only_within_the_same_control_window() {
        let mut state = GameState::new_two_player(42);
        state.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: PlayerId(1),
            controller: PlayerId(0),
            timestamp: 7,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        });
        let ability = ResolvedAbility::new(
            Effect::ControlNextTurn {
                target: crate::types::ability::TargetFilter::Player,
                grant_extra_turn_after: false,
                window: ControlWindow::NextCombatPhase,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert_eq!(state.scheduled_turn_controls.len(), 2);
        assert!(state
            .scheduled_turn_controls
            .iter()
            .any(|control| control.window == ControlWindow::NextTurn));
        assert!(state
            .scheduled_turn_controls
            .iter()
            .any(|control| control.window == ControlWindow::NextCombatPhase));
    }

    #[test]
    fn scheduled_control_legacy_payload_defaults_creation_timestamp_to_zero() {
        let current = ScheduledTurnControl {
            target_player: PlayerId(1),
            controller: PlayerId(0),
            timestamp: 17,
            grant_extra_turn_after: false,
            window: ControlWindow::NextTurn,
        };
        let mut legacy = serde_json::to_value(current).expect("serialize scheduled control");
        legacy
            .as_object_mut()
            .expect("scheduled control serializes as an object")
            .remove("timestamp");

        let restored: ScheduledTurnControl =
            serde_json::from_value(legacy).expect("deserialize pre-timestamp payload");
        assert_eq!(restored.timestamp, 0);
        assert_eq!(restored.controller, PlayerId(0));
        assert_eq!(restored.target_player, PlayerId(1));
    }

    #[test]
    fn game_state_legacy_payload_defaults_active_control_timestamp_to_none() {
        let mut current = GameState::new_two_player(42);
        current.turn_decision_controller = Some(PlayerId(0));
        current.turn_decision_control_timestamp = Some(17);
        let mut legacy = serde_json::to_value(current).expect("serialize game state");
        legacy
            .as_object_mut()
            .expect("game state serializes as an object")
            .remove("turn_decision_control_timestamp");

        let restored: GameState =
            serde_json::from_value(legacy).expect("deserialize pre-provenance game state");
        assert_eq!(restored.turn_decision_controller, Some(PlayerId(0)));
        assert_eq!(restored.turn_decision_control_timestamp, None);
    }

    #[test]
    fn two_hg_control_next_turn_targets_team_anchor() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
        let ability = ResolvedAbility::new(
            Effect::ControlNextTurn {
                target: crate::types::ability::TargetFilter::Player,
                grant_extra_turn_after: false,
                window: ControlWindow::NextTurn,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(2),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.scheduled_turn_controls.len(), 1);
        assert_eq!(state.scheduled_turn_controls[0].target_player, PlayerId(0));
        assert_eq!(state.scheduled_turn_controls[0].controller, PlayerId(2));
    }

    #[test]
    fn standard_control_next_turn_target_is_not_normalized() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 42);
        let ability = ResolvedAbility::new(
            Effect::ControlNextTurn {
                target: crate::types::ability::TargetFilter::Player,
                grant_extra_turn_after: false,
                window: ControlWindow::NextTurn,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.scheduled_turn_controls.len(), 1);
        assert_eq!(state.scheduled_turn_controls[0].target_player, PlayerId(1));
    }
}
