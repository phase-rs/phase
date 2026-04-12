use crate::game::sacrifice::{self, SacrificeOutcome};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.21a: To sacrifice a permanent, its controller moves it to its owner's graveyard.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (filter, up_to) = match &ability.effect {
        Effect::Sacrifice { target, up_to } => (target, *up_to),
        _ => (&TargetFilter::Any, false),
    };

    let targeted_objects: Vec<ObjectId> = ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(obj_id) => Some(*obj_id),
            _ => None,
        })
        .collect();

    if targeted_objects.is_empty() {
        // CR 107.3a + CR 601.2b: ability-context filter evaluation.
        let ctx = crate::game::filter::FilterContext::from_ability(ability);
        let eligible: Vec<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state.objects.get(id).is_some_and(|obj| {
                    obj.controller == ability.controller
                        && !obj.is_emblem
                        && crate::game::filter::matches_target_filter(state, *id, filter, &ctx)
                })
            })
            .collect();

        if eligible.is_empty() {
            if !up_to {
                state.cost_payment_failed_flag = true;
            }
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        if eligible.len() == 1 && !up_to {
            match sacrifice::sacrifice_permanent(state, eligible[0], ability.controller, events) {
                Ok(SacrificeOutcome::Complete) => {
                    state.last_effect_count = Some(1);
                }
                Ok(SacrificeOutcome::NeedsReplacementChoice(player)) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    return Ok(());
                }
                Err(_) => {}
            }

            events.push(GameEvent::EffectResolved {
                kind: EffectKind::from(&ability.effect),
                source_id: ability.source_id,
            });
            return Ok(());
        }

        state.waiting_for = WaitingFor::EffectZoneChoice {
            player: ability.controller,
            cards: eligible,
            count: 1,
            up_to,
            source_id: ability.source_id,
            effect_kind: EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            under_your_control: false,
            enters_attacking: false,
            owner_library: false,
        };

        // EffectResolved is emitted by the EffectZoneChoice handler after the player chooses
        // (matching the DiscardChoice pattern — single authority for the event).
        return Ok(());
    }

    for obj_id in targeted_objects {
        let obj = state
            .objects
            .get(&obj_id)
            .ok_or(EffectError::ObjectNotFound(obj_id))?;

        // CR 114.5: Emblems cannot be sacrificed
        if obj.is_emblem {
            continue;
        }

        // CR 701.21a: A player can't sacrifice something that isn't a permanent.
        if obj.zone != Zone::Battlefield {
            continue;
        }

        let player_id = obj.controller;

        match sacrifice::sacrifice_permanent(state, obj_id, player_id, events) {
            Ok(SacrificeOutcome::Complete) => {}
            Ok(SacrificeOutcome::NeedsReplacementChoice(player)) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
            Err(_) => {
                // Object may have left the battlefield between check and sacrifice;
                // skip silently (same as the zone check above).
                continue;
            }
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
    use crate::types::ability::{Effect, TargetFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_sacrifice_ability(target: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                up_to: false,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn make_choice_sacrifice_ability(up_to: bool) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Sacrifice {
                target: TargetFilter::Any,
                up_to,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

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
        let ability = make_sacrifice_ability(obj_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
    }

    #[test]
    fn sacrifice_emits_permanent_sacrificed_event() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        let ability = make_sacrifice_ability(obj_id);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(e, GameEvent::PermanentSacrificed { object_id, player_id } if *object_id == obj_id && *player_id == PlayerId(0))));
    }

    #[test]
    fn empty_targets_sets_effect_zone_choice_when_multiple_permanents_exist() {
        let mut state = GameState::new_two_player(42);
        let a = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "A".to_string(),
            Zone::Battlefield,
        );
        let b = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "B".to_string(),
            Zone::Battlefield,
        );
        let ability = make_choice_sacrifice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                count,
                effect_kind,
                zone,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert_eq!(*effect_kind, EffectKind::Sacrifice);
                assert_eq!(*zone, Zone::Battlefield);
                assert!(cards.contains(&a));
                assert!(cards.contains(&b));
            }
            other => panic!("expected EffectZoneChoice, got {other:?}"),
        }
    }

    #[test]
    fn empty_targets_with_single_permanent_auto_sacrifices_and_records_count() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Only Permanent".to_string(),
            Zone::Battlefield,
        );
        let ability = make_choice_sacrifice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.battlefield.contains(&obj_id));
        assert!(state.players[0].graveyard.contains(&obj_id));
        assert_eq!(state.last_effect_count, Some(1));
    }

    #[test]
    fn mandatory_empty_target_sacrifice_without_permanents_sets_failure_flag() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choice_sacrifice_ability(false);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.cost_payment_failed_flag);
    }

    #[test]
    fn up_to_empty_target_sacrifice_without_permanents_does_not_fail() {
        let mut state = GameState::new_two_player(42);
        let ability = make_choice_sacrifice_ability(true);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(!state.cost_payment_failed_flag);
        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    }
}
