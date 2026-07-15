use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
/// CR 401.4: Target's owner puts it on top or bottom of their library.
/// The owner (not the controller) makes the choice.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // CR 608.2c + CR 603.10: Delegate target resolution to the unified 3-tier
    // dispatch (`resolved_targets`) so this resolver picks up the same self-ref
    // handling `Effect::Bounce` and the other zone-change resolvers use.
    // `resolved_targets` short-circuits `SelfRef` to `ability.source_id`
    // regardless of `ability.targets`, so a self-tuck (Arashin Sovereign: "When
    // ~ dies, put it on your choice of the top or bottom of its owner's library")
    // resolves to the source; a chosen/parent target is unchanged (tier 3
    // returns the pre-selected targets satisfying the filter).
    let target_filter = match &ability.effect {
        Effect::PutOnTopOrBottom { target } => target,
        _ => &TargetFilter::None,
    };
    let object_id = crate::game::targeting::resolved_targets(ability, target_filter, state)
        .iter()
        .find_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .ok_or(EffectError::InvalidParam(
            "PutOnTopOrBottom requires a target".to_string(),
        ))?;

    let obj = state
        .objects
        .get(&object_id)
        .ok_or(EffectError::ObjectNotFound(object_id))?;

    // CR 401.4: The owner makes the choice, not the controller.
    let owner = obj.owner;

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    // CR 400.5: The order of objects in a library can't be changed except when effects allow it.
    state.waiting_for = WaitingFor::TopOrBottomChoice {
        player: owner,
        object_id,
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    #[test]
    fn test_resolve_sets_waiting_for_owner() {
        let mut state = GameState::new_two_player(42);
        // Create a creature owned by player 1 but controlled by player 0
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        // Override controller to player 0 (simulating gain control)
        state.objects.get_mut(&obj_id).unwrap().controller = PlayerId(0);

        let ability = ResolvedAbility::new(
            Effect::PutOnTopOrBottom {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        // Owner (player 1) should be the one choosing, not controller (player 0)
        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::TopOrBottomChoice {
                    player: PlayerId(1),
                    object_id: oid,
                } if oid == obj_id
            ),
            "Expected TopOrBottomChoice for owner (P1), got {:?}",
            state.waiting_for
        );
    }

    #[test]
    fn test_resolve_stack_spell_prompts_owner() {
        let mut state = GameState::new_two_player(42);
        // CR 108.3 + CR 401.4: a stack spell is moved by its owner, and that owner chooses.
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Stack Spell".to_string(),
            Zone::Stack,
        );
        state.objects.get_mut(&obj_id).unwrap().controller = PlayerId(0);

        let ability = ResolvedAbility::new(
            Effect::PutOnTopOrBottom {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(101),
            PlayerId(0),
        );

        let mut events = vec![];
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(
            matches!(
                state.waiting_for,
                WaitingFor::TopOrBottomChoice {
                    player: PlayerId(1),
                    object_id: oid,
                } if oid == obj_id
            ),
            "Expected TopOrBottomChoice for stack spell owner (P1), got {:?}",
            state.waiting_for
        );
    }
}
