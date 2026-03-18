use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 716.5: Set the class level on the source Class enchantment.
/// Setting the new level happens as part of the level ability's resolution.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let level = match &ability.effect {
        Effect::SetClassLevel { level } => *level,
        _ => return Ok(()),
    };

    let source_id = ability.source_id;
    if let Some(obj) = state.objects.get_mut(&source_id) {
        obj.class_level = Some(level);
        events.push(GameEvent::ClassLevelGained {
            object_id: source_id,
            level,
        });
        // CR 716.6: New abilities become active at the new level — recompute layers.
        state.layers_dirty = true;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::SetClassLevel,
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::printed_cards::apply_card_face_to_object;
    use crate::game::zones::create_object;
    use crate::types::ability::{ActivationRestriction, StaticCondition};
    use crate::types::card::CardFace;
    use crate::types::card_type::CardType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn create_class_face() -> CardFace {
        let mut card_type = CardType::default();
        card_type.subtypes.push("Class".to_string());
        CardFace {
            name: "Test Class".to_string(),
            card_type,
            ..CardFace::default()
        }
    }

    #[test]
    fn class_enters_at_level_1() {
        // CR 716.3: Each Class enchantment enters the battlefield at level 1.
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Class".to_string(),
            Zone::Battlefield,
        );

        let face = create_class_face();
        let obj = state.objects.get_mut(&obj_id).unwrap();
        apply_card_face_to_object(obj, &face);

        assert_eq!(obj.class_level, Some(1));
    }

    #[test]
    fn set_class_level_updates_level_and_marks_dirty() {
        // CR 716.5: SetClassLevel sets the level and marks layers dirty.
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Class".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().class_level = Some(1);
        state.layers_dirty = false;

        let ability = ResolvedAbility::new(
            Effect::SetClassLevel { level: 2 },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();
        let result = resolve(&mut state, &ability, &mut events);

        assert!(result.is_ok());
        assert_eq!(state.objects.get(&obj_id).unwrap().class_level, Some(2));
        assert!(state.layers_dirty);
        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ClassLevelGained {
                object_id,
                level: 2
            } if *object_id == obj_id
        )));
    }

    #[test]
    fn class_level_is_restriction_permits_correct_level() {
        // CR 716.4: ClassLevelIs restriction permits at exactly the specified level.
        let restriction_level_1 = ActivationRestriction::ClassLevelIs { level: 1 };
        let restriction_level_2 = ActivationRestriction::ClassLevelIs { level: 2 };

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Class".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().class_level = Some(1);

        // At level 1: ClassLevelIs { level: 1 } should pass
        assert!(crate::game::restrictions::check_activation_restrictions(
            &state,
            PlayerId(0),
            obj_id,
            0,
            std::slice::from_ref(&restriction_level_1),
        )
        .is_ok());
        // At level 1: ClassLevelIs { level: 2 } should fail
        assert!(crate::game::restrictions::check_activation_restrictions(
            &state,
            PlayerId(0),
            obj_id,
            0,
            std::slice::from_ref(&restriction_level_2),
        )
        .is_err());

        // Advance to level 2
        state.objects.get_mut(&obj_id).unwrap().class_level = Some(2);

        // At level 2: ClassLevelIs { level: 2 } should pass
        assert!(crate::game::restrictions::check_activation_restrictions(
            &state,
            PlayerId(0),
            obj_id,
            0,
            &[restriction_level_2],
        )
        .is_ok());
        // At level 2: ClassLevelIs { level: 1 } should fail
        assert!(crate::game::restrictions::check_activation_restrictions(
            &state,
            PlayerId(0),
            obj_id,
            0,
            &[restriction_level_1],
        )
        .is_err());
    }

    #[test]
    fn class_level_ge_condition_evaluates_correctly() {
        // CR 716.6: ClassLevelGE evaluates >= for level-gated abilities.
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Class".to_string(),
            Zone::Battlefield,
        );

        let cond_ge2 = StaticCondition::ClassLevelGE { level: 2 };
        let cond_ge1 = StaticCondition::ClassLevelGE { level: 1 };

        // At level 1: GE(2) is false, GE(1) is true
        state.objects.get_mut(&obj_id).unwrap().class_level = Some(1);
        assert!(!crate::game::layers::evaluate_condition_for_test(
            &state,
            &cond_ge2,
            PlayerId(0),
            obj_id
        ));
        assert!(crate::game::layers::evaluate_condition_for_test(
            &state,
            &cond_ge1,
            PlayerId(0),
            obj_id
        ));

        // At level 2: GE(2) is true, GE(1) is true
        state.objects.get_mut(&obj_id).unwrap().class_level = Some(2);
        assert!(crate::game::layers::evaluate_condition_for_test(
            &state,
            &cond_ge2,
            PlayerId(0),
            obj_id
        ));

        // At level 3: GE(2) is still true
        state.objects.get_mut(&obj_id).unwrap().class_level = Some(3);
        assert!(crate::game::layers::evaluate_condition_for_test(
            &state,
            &cond_ge2,
            PlayerId(0),
            obj_id
        ));
    }

    #[test]
    fn class_resets_to_level_1_on_zone_reentry() {
        // CR 400.7 + CR 716.3: A Class that leaves and re-enters the battlefield
        // is a new object at level 1.
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Class".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&obj_id).unwrap().class_level = Some(3);

        // Move to exile
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, obj_id, Zone::Exile, &mut events);
        assert_eq!(state.objects.get(&obj_id).unwrap().zone, Zone::Exile);
        // Level is preserved in exile (not on battlefield)
        assert_eq!(state.objects.get(&obj_id).unwrap().class_level, Some(3));

        // Move back to battlefield — should reset to level 1
        crate::game::zones::move_to_zone(&mut state, obj_id, Zone::Battlefield, &mut events);
        assert_eq!(
            state.objects.get(&obj_id).unwrap().class_level,
            Some(1),
            "Class should reset to level 1 on re-entering the battlefield"
        );
    }
}
