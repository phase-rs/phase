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

    state
        .scheduled_turn_controls
        .retain(|scheduled| scheduled.target_player != target_player);
    state.scheduled_turn_controls.push(ScheduledTurnControl {
        target_player,
        controller: ability.controller,
        grant_extra_turn_after: *grant_extra_turn_after,
        // CR 723.1 / CR 723.2: schedule under the parsed window regardless of
        // window — the dedup `retain` above keeps one entry per target (CR 723.1a).
        window: *window,
    });

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ControlNextTurn,
        source_id: ability.source_id,
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
    fn resolve_overwrites_prior_scheduled_control_for_same_target() {
        let mut state = GameState::new_two_player(42);
        state.scheduled_turn_controls.push(ScheduledTurnControl {
            target_player: PlayerId(1),
            controller: PlayerId(0),
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

        assert_eq!(state.scheduled_turn_controls.len(), 1);
        assert_eq!(
            state.scheduled_turn_controls[0],
            ScheduledTurnControl {
                target_player: PlayerId(1),
                controller: PlayerId(1),
                grant_extra_turn_after: true,
                window: ControlWindow::NextTurn,
            }
        );
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
