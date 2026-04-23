use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 400.6: Zone change — permanent moves from battlefield to its owner's hand.
///
/// Also handles LTB self-return triggers (CR 603.10) such as Rancor: when the
/// trigger resolves, the source is already in its owner's graveyard, so the
/// resolver must accept graveyard as a valid from-zone in addition to the
/// battlefield.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Determine targets using typed Effect::Bounce target field.
    // CR 608.2c + 603.10: An anaphoric "it" in a top-level trigger effect (e.g.,
    // Rancor's "return it to its owner's hand") has no parent target to inherit
    // from — it refers to the source object itself. `SelfRef` collapses to the
    // same thing. `TriggeringSource` is deliberately excluded: it resolves via
    // `state.current_trigger_event`, and conflating it with the ability source
    // would be wrong for "Whenever a creature dies, return it ..." patterns
    // where the source is the ability's host, not the triggering object.
    let use_self = match &ability.effect {
        Effect::Bounce { target, .. } => {
            matches!(
                target,
                TargetFilter::None | TargetFilter::SelfRef | TargetFilter::ParentTarget
            ) && ability.targets.is_empty()
        }
        _ => false,
    };

    let targets: Vec<_> = if use_self {
        vec![ability.source_id]
    } else {
        ability
            .targets
            .iter()
            .filter_map(|t| {
                if let TargetRef::Object(id) = t {
                    Some(*id)
                } else {
                    None
                }
            })
            .collect()
    };

    for obj_id in targets {
        // CR 114.5: Emblems cannot be bounced
        if state.objects.get(&obj_id).is_some_and(|o| o.is_emblem) {
            continue;
        }

        // CR 400.3 + CR 603.10: Bounce moves the object from its current zone to
        // its owner's hand. Battlefield is the usual case; graveyard covers LTB
        // self-return triggers (Rancor class) where the source has already moved
        // to the graveyard by the time the trigger resolves.
        let current_zone = state.objects.get(&obj_id).map(|o| o.zone);
        if matches!(current_zone, Some(Zone::Battlefield | Zone::Graveyard)) {
            zones::move_to_zone(state, obj_id, Zone::Hand, events);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    #[test]
    fn test_bounce_moves_permanent_to_hand() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[1].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_self() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ninja".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::None,
                destination: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_emits_zone_changed() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Battlefield,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::Any,
                destination: None,
            },
            vec![TargetRef::Object(obj_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::ZoneChanged {
                from: Some(Zone::Battlefield),
                to: Zone::Hand,
                ..
            }
        )));
    }

    /// CR 603.10 / Rancor class: LTB self-return triggers fire after the source
    /// has moved to the graveyard. The parsed effect is
    /// `Bounce { target: ParentTarget }` with empty `ability.targets`; the
    /// resolver must treat that as "return the source object from the graveyard
    /// to its owner's hand."
    #[test]
    fn test_bounce_ltb_self_return_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
            },
            vec![],
            obj_id,
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[0].graveyard.contains(&obj_id));
        assert!(state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn test_bounce_ltb_self_ref_from_graveyard() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Spirit Loop".to_string(),
            Zone::Graveyard,
        );

        let ability = ResolvedAbility::new(
            Effect::Bounce {
                target: TargetFilter::SelfRef,
                destination: None,
            },
            vec![],
            obj_id,
            PlayerId(1),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.players[1].graveyard.contains(&obj_id));
        assert!(state.players[1].hand.contains(&obj_id));
    }

    /// End-to-end Rancor-class pipeline test: battlefield → graveyard emits
    /// `ZoneChanged`, `process_triggers` picks up the graveyard-zone trigger,
    /// the triggered ability resolves, and the Aura ends up in its owner's hand.
    #[test]
    fn test_rancor_ltb_pipeline_returns_to_owner_hand() {
        use crate::game::stack::resolve_top;
        use crate::game::triggers::process_triggers;
        use crate::types::ability::{AbilityDefinition, AbilityKind, TriggerDefinition};
        use crate::types::triggers::TriggerMode;

        let mut state = GameState::new_two_player(42);
        let rancor_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Rancor".to_string(),
            Zone::Battlefield,
        );

        // Mirror the shape emitted by the parser for Rancor's LTB trigger.
        let mut trigger = TriggerDefinition::new(TriggerMode::ChangesZone);
        trigger.origin = Some(Zone::Battlefield);
        trigger.destination = Some(Zone::Graveyard);
        trigger.valid_card = Some(TargetFilter::SelfRef);
        trigger.trigger_zones = vec![Zone::Graveyard];
        trigger.execute = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Bounce {
                target: TargetFilter::ParentTarget,
                destination: None,
            },
        )));
        state
            .objects
            .get_mut(&rancor_id)
            .unwrap()
            .trigger_definitions
            .push(trigger);

        // Destroy Rancor (move battlefield → graveyard), then run the trigger pipeline.
        let mut events = Vec::new();
        crate::game::zones::move_to_zone(&mut state, rancor_id, Zone::Graveyard, &mut events);
        assert!(state.players[0].graveyard.contains(&rancor_id));

        process_triggers(&mut state, &events);
        assert_eq!(
            state.stack.len(),
            1,
            "Rancor LTB trigger did not reach stack"
        );

        // Resolve the triggered ability and confirm Rancor landed in its owner's hand.
        let mut resolve_events = Vec::new();
        resolve_top(&mut state, &mut resolve_events);
        assert!(
            state.players[0].hand.contains(&rancor_id),
            "Rancor should return to owner's hand; actual zones: hand={:?} graveyard={:?}",
            state.players[0].hand,
            state.players[0].graveyard
        );
        assert!(!state.players[0].graveyard.contains(&rancor_id));
    }
}
