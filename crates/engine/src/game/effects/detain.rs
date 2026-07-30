use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 701.35a: Detain a permanent — until the controller's next turn, that permanent
/// can't attack or block and its activated abilities can't be activated.
///
/// Uses the same per-player tracking pattern as Goad: `detained_by` on `GameObject`
/// records which players have detained this permanent. The set is pruned at the
/// detaining player's next turn in `prune_until_next_turn_effects`.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for target in &ability.targets {
        if let TargetRef::Object(obj_id) = target {
            let Some(obj) = state.objects.get_mut(obj_id) else {
                continue;
            };

            // CR 701.35a: Only detain permanents on the battlefield.
            if obj.zone != Zone::Battlefield {
                continue;
            }

            // CR 701.35a: Mark the permanent as detained by the controller of this effect.
            obj.detained_by.insert(ability.controller);

            events.push(GameEvent::Detained { object_id: *obj_id });
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Detain,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, TargetFilter, TargetRef};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_detain_ability(target: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Detain {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            controller,
        )
    }

    #[test]
    fn detain_marks_permanent_with_detaining_player() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_detain_ability(target_id, PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&target_id).unwrap();
        assert!(obj.detained_by.contains(&PlayerId(0)));
        assert_eq!(obj.detained_by.len(), 1);
    }

    #[test]
    fn detain_same_player_twice_is_idempotent() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_detain_ability(target_id, PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&target_id).unwrap();
        assert_eq!(obj.detained_by.len(), 1);
    }

    #[test]
    fn detain_nonexistent_target_is_skipped() {
        let mut state = GameState::new_two_player(42);
        let ability = make_detain_ability(ObjectId(999), PlayerId(0));
        let mut events = Vec::new();

        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
    }

    #[test]
    fn detain_emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_detain_ability(target_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Detain,
                ..
            }
        )));
    }
}
