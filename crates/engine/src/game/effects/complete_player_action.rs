use crate::types::ability::{Effect, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 608.2c: A player action completes only when the immediately preceding
/// operation produced the exact typed result required by this continuation.
pub(crate) fn succeeded(ability: &ResolvedAbility) -> bool {
    let Effect::CompletePlayerAction {
        required_result, ..
    } = &ability.effect
    else {
        return false;
    };
    ability.context.prior_effect_result.as_ref() == Some(required_result)
}

/// CR 603.2 + CR 608.2c: Publish the completed action after its final
/// operation result is known. The `EffectResolved` event is deliberately
/// adjacent and first, matching the engine's trigger ordering contract.
pub(crate) fn resolve(
    _state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::CompletePlayerAction {
        parent_kind,
        action,
        ..
    } = &ability.effect
    else {
        return Err(EffectError::InvalidParam(
            "complete_player_action resolver requires CompletePlayerAction".to_string(),
        ));
    };

    events.push(GameEvent::EffectResolved {
        kind: *parent_kind,
        source_id: ability.source_id,
        subject: None,
    });
    if succeeded(ability) {
        events.push(GameEvent::PlayerPerformedAction {
            player_id: ability.controller,
            action: *action,
            look_count: None,
            scry_bottom_count: None,
            scry_top_count: None,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{EffectKind, EffectResolutionResult, ThisWayCause};
    use crate::types::events::PlayerActionKind;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    fn completion() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::CompletePlayerAction {
                parent_kind: EffectKind::Forage,
                action: PlayerActionKind::Forage,
                required_result: EffectResolutionResult {
                    cause: ThisWayCause::Sacrificed,
                    count: 1,
                },
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        )
    }

    #[test]
    fn publishes_action_only_for_exact_direct_result() {
        let mut state = GameState::new_two_player(1);
        let mut ability = completion();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(events.len(), 1);

        ability.context.prior_effect_result = Some(EffectResolutionResult {
            cause: ThisWayCause::Sacrificed,
            count: 1,
        });
        events.clear();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(matches!(
            events.as_slice(),
            [
                GameEvent::EffectResolved {
                    kind: EffectKind::Forage,
                    ..
                },
                GameEvent::PlayerPerformedAction {
                    action: PlayerActionKind::Forage,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn direct_result_round_trips_only_on_completion_node() {
        let mut ability = completion();
        ability.sub_ability = Some(Box::new(ResolvedAbility::new(
            Effect::NoOp,
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        )));
        ability.set_prior_effect_result_for_immediate_node(EffectResolutionResult {
            cause: ThisWayCause::Sacrificed,
            count: 1,
        });

        let serialized = serde_json::to_string(&ability).expect("completion node serializes");
        let restored: ResolvedAbility =
            serde_json::from_str(&serialized).expect("completion node deserializes");

        assert!(succeeded(&restored));
        assert_eq!(
            restored
                .sub_ability
                .as_deref()
                .expect("grandchild retained")
                .context
                .prior_effect_result,
            None,
            "the one-hop result must not leak into a grandchild"
        );
    }
}
