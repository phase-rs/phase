use crate::types::ability::{EffectError, PtValue, ResolvedAbility, TokenAttackDestination};
use crate::types::events::{GameEvent, PlayerActionKind};
use crate::types::game_state::GameState;

/// CR 701.16a: Investigate — create a Clue artifact token.
///
/// A Clue token is a colorless Artifact — Clue with "{2}, Sacrifice this
/// artifact: Draw a card." The token creation reuses the existing token
/// resolver by constructing a synthetic Token effect.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 111.10f: A Clue token is a colorless Clue artifact token with
    // "{2}, Sacrifice this artifact: Draw a card."
    // Build a synthetic Token effect and resolve through the standard token pipeline.
    let mut clue_ability = ResolvedAbility::new(
        crate::types::ability::Effect::Token {
            name: "Clue".to_string(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: vec!["Artifact".to_string(), "Clue".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
            owner: crate::types::ability::TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            attack_destination: TokenAttackDestination::ControllerChooses,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        },
        ability.targets.clone(),
        ability.source_id,
        ability.controller,
    );
    clue_ability.context = ability.context.clone();
    clue_ability.sub_ability = ability.sub_ability.clone();
    super::token::begin_token_creation(
        state,
        &clue_ability,
        crate::types::game_state::TokenCreationCallerState::Investigate {
            player: ability.controller,
        },
        events,
    );

    Ok(())
}

pub(crate) fn complete_creation(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    events: &mut Vec<GameEvent>,
) {
    // CR 608.2c + CR 109.5: Record the investigating player so downstream
    // "the number of opponents who investigated this way" references resolve.
    // During an `Effect`-level `player_scope: Opponent` fan-out the driver
    // rebinds `ability.controller` to the scoped opponent (effects/mod.rs:2891),
    // so each opponent's investigate records THAT opponent. Publish both ledgers
    // at completion, before the creation-owned tail can read them. A resumed
    // creation need not have an enclosing generic event scan.
    state
        .player_actions_this_way
        .insert((player, PlayerActionKind::Investigate));
    super::record_player_action_this_turn(state, player, PlayerActionKind::Investigate);
    events.push(GameEvent::PlayerPerformedAction {
        player_id: player,
        action: PlayerActionKind::Investigate,
        look_count: None,
        scry_bottom_count: None,
        scry_top_count: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_completed_action_ledgers(state: &GameState) {
        let action = (
            crate::types::player::PlayerId(0),
            PlayerActionKind::Investigate,
        );
        assert!(state.player_actions_this_way.contains(&action));
        assert_eq!(
            state
                .player_actions_this_turn
                .iter()
                .filter(|entry| **entry == action)
                .count(),
            1,
            "completion publishes the turn action once, including after resume"
        );
    }

    #[test]
    fn investigate_completion_synchronous() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            crate::types::ability::Effect::Investigate,
            super::super::token_copy::tests::CreationPauseCase::Synchronous,
        );
        assert_completed_action_ledgers(&state);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::Investigate,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Token,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Investigate,
                ..
            }
        )));
    }
    #[test]
    fn investigate_completion_queued_child() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            crate::types::ability::Effect::Investigate,
            super::super::token_copy::tests::CreationPauseCase::Queued,
        );
        assert_completed_action_ledgers(&state);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::Investigate,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Token,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Investigate,
                ..
            }
        )));
    }
    #[test]
    fn investigate_completion_counter_pause() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            crate::types::ability::Effect::Investigate,
            super::super::token_copy::tests::CreationPauseCase::Counter,
        );
        assert_completed_action_ledgers(&state);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::Investigate,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Token,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Investigate,
                ..
            }
        )));
    }
    #[test]
    fn investigate_completion_queued_child_current_reload() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            crate::types::ability::Effect::Investigate,
            super::super::token_copy::tests::CreationPauseCase::ReloadQueued,
        );
        assert_completed_action_ledgers(&state);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::Investigate,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Token,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Investigate,
                ..
            }
        )));
    }
    #[test]
    fn investigate_completion_counter_current_reload() {
        let (state, events) = super::super::token_copy::tests::creation_wrapper_case(
            crate::types::ability::Effect::Investigate,
            super::super::token_copy::tests::CreationPauseCase::ReloadCounter,
        );
        assert_completed_action_ledgers(&state);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::PlayerPerformedAction {
                        action: crate::types::events::PlayerActionKind::Investigate,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: crate::types::ability::EffectKind::Token,
                        ..
                    }
                ))
                .count(),
            1
        );
        assert!(!events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: crate::types::ability::EffectKind::Investigate,
                ..
            }
        )));
    }
}
