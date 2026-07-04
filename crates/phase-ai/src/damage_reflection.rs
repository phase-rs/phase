//! Detection helpers for damage-reflection triggered abilities (Jackal Pup,
//! Boros Reckoner, Spiteful Sliver, Spitemare, etc.).

use engine::game::game_object::GameObject;
use engine::types::ability::{
    ControllerRef, Effect, QuantityExpr, QuantityRef, TargetFilter, TriggerDefinition, TypeFilter,
};
use engine::types::card_type::CoreType;
use engine::types::triggers::TriggerMode;

/// True when the trigger is a self-scoped `DamageReceived` ability that deals
/// `EventContextAmount` damage to a player or planeswalker (Spiteful Sliver /
/// Spitemare pattern), as opposed to damage back to its controller (Jackal Pup).
pub fn has_damage_reflection_to_player(object: &GameObject) -> bool {
    object
        .trigger_definitions
        .iter_unchecked()
        .any(damage_reflection_to_player_trigger)
}

/// True when the trigger deals reflected damage to its controller (Jackal Pup /
/// Boros Reckoner to self).
pub fn has_damage_reflection_to_controller(object: &GameObject) -> bool {
    object
        .trigger_definitions
        .iter_unchecked()
        .any(damage_reflection_to_controller_trigger)
}

/// True when `effect` is a `DealDamage` that uses the received-damage amount and
/// can hit an opponent player or planeswalker.
pub fn is_event_context_damage_to_player(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            },
            target,
            ..
        } if deal_damage_can_target_opponent_player(target)
    )
}

fn damage_reflection_to_player_trigger(trigger: &TriggerDefinition) -> bool {
    if trigger.mode != TriggerMode::DamageReceived {
        return false;
    }
    let self_scoped = trigger
        .valid_card
        .as_ref()
        .is_none_or(|f| matches!(f, TargetFilter::SelfRef));
    if !self_scoped {
        return false;
    }
    let Some(execute) = &trigger.execute else {
        return false;
    };
    is_event_context_damage_to_player(&execute.effect)
        && !matches!(
            extract_deal_damage_target(&execute.effect),
            Some(TargetFilter::Controller)
        )
}

fn damage_reflection_to_controller_trigger(trigger: &TriggerDefinition) -> bool {
    if trigger.mode != TriggerMode::DamageReceived {
        return false;
    }
    let self_scoped = trigger
        .valid_card
        .as_ref()
        .is_none_or(|f| matches!(f, TargetFilter::SelfRef));
    if !self_scoped {
        return false;
    }
    let Some(execute) = &trigger.execute else {
        return false;
    };
    matches!(
        &*execute.effect,
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount
            },
            target: TargetFilter::Controller,
            ..
        }
    )
}

fn extract_deal_damage_target(effect: &Effect) -> Option<&TargetFilter> {
    match effect {
        Effect::DealDamage { target, .. } => Some(target),
        _ => None,
    }
}

fn deal_damage_can_target_opponent_player(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Player | TargetFilter::Any => true,
        TargetFilter::Typed(tf) => {
            tf.type_filters
                .iter()
                .any(|t| matches!(t, TypeFilter::Planeswalker))
                || (tf.type_filters.is_empty()
                    && matches!(tf.controller, Some(ControllerRef::Opponent)))
        }
        TargetFilter::Or { filters } => filters.iter().any(deal_damage_can_target_opponent_player),
        _ => false,
    }
}

/// Non-lethal damage to an opponent's creature that reflects to players is
/// usually a gift — the controller will redirect the damage.
pub fn opponent_creature_reflection_penalty(
    state: &engine::types::game_state::GameState,
    object_id: engine::types::identifiers::ObjectId,
    ai_player: engine::types::player::PlayerId,
    damage: i32,
) -> f64 {
    let Some(object) = state.objects.get(&object_id) else {
        return 0.0;
    };
    if object.controller == ai_player
        || !object.card_types.core_types.contains(&CoreType::Creature)
        || !has_damage_reflection_to_player(object)
    {
        return 0.0;
    }
    let remaining = object
        .toughness
        .map(|t| t - object.damage_marked as i32)
        .unwrap_or(0);
    if damage >= remaining.max(0) {
        // Lethal — killing the creature stops future triggers.
        return 0.0;
    }
    -12.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, TargetFilter, TriggerDefinition, TypedFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::game_state::GameState;
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    fn spiteful_trigger() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::DamageReceived)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Or {
                        filters: vec![
                            TargetFilter::Player,
                            TargetFilter::Typed(TypedFilter::new(TypeFilter::Planeswalker)),
                        ],
                    },
                    damage_source: None,
                    excess: None,
                },
            ))
    }

    fn pup_trigger() -> TriggerDefinition {
        TriggerDefinition::new(TriggerMode::DamageReceived)
            .valid_card(TargetFilter::SelfRef)
            .execute(AbilityDefinition::new(
                AbilityKind::Database,
                Effect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::EventContextAmount,
                    },
                    target: TargetFilter::Controller,
                    damage_source: None,
                    excess: None,
                },
            ))
    }

    #[test]
    fn detects_spiteful_pattern() {
        let mut state = GameState::new_two_player(1);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Sliver".into(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.trigger_definitions.push(spiteful_trigger());
        assert!(has_damage_reflection_to_player(obj));
        assert!(!has_damage_reflection_to_controller(obj));
    }

    #[test]
    fn detects_jackal_pup_pattern() {
        let mut state = GameState::new_two_player(1);
        let id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Pup".into(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.trigger_definitions.push(pup_trigger());
        assert!(has_damage_reflection_to_controller(obj));
        assert!(!has_damage_reflection_to_player(obj));
    }

    #[test]
    fn reflection_penalty_skips_lethal_damage() {
        let mut state = GameState::new_two_player(1);
        let id = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Sliver".into(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(2);
        obj.toughness = Some(2);
        obj.trigger_definitions.push(spiteful_trigger());
        assert_eq!(
            opponent_creature_reflection_penalty(&state, id, PlayerId(0), 1),
            -12.0
        );
        assert_eq!(
            opponent_creature_reflection_penalty(&state, id, PlayerId(0), 2),
            0.0
        );
    }
}
