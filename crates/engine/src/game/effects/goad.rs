use crate::game::filter::{matches_target_filter, FilterContext};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 701.15a: Goad a creature — until the goading player's next turn, the creature
/// is goaded. It must attack each combat if able and must attack a player other than
/// the goading player if able (CR 701.15b).
///
/// CR 701.15c: A creature can be goaded by multiple players, creating additional
/// combat requirements.
///
/// CR 701.15d: The same player goading a creature again has no effect (HashSet
/// insert is idempotent).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for obj_id in goad_targets(state, ability) {
        let Some(obj) = state.objects.get_mut(&obj_id) else {
            continue;
        };

        // CR 701.15a: Only goad creatures on the battlefield.
        if obj.zone != Zone::Battlefield {
            continue;
        }

        // CR 701.15a: Mark the creature as goaded by the controller of this effect.
        // CR 701.15d: Re-goading by the same player is a no-op (HashSet semantics).
        obj.goaded_by.insert(ability.controller);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });

    Ok(())
}

fn goad_targets(state: &GameState, ability: &ResolvedAbility) -> Vec<ObjectId> {
    if let Effect::GoadAll { target } = &ability.effect {
        let effective_filter = crate::game::effects::resolved_object_filter(ability, target);
        let ctx = FilterContext::from_ability(ability);
        return state
            .battlefield_phased_in_ids()
            .into_iter()
            .filter(|id| matches_target_filter(state, *id, &effective_filter, &ctx))
            .collect();
    }

    ability
        .targets
        .iter()
        .filter_map(|target| match target {
            TargetRef::Object(obj_id) => Some(*obj_id),
            TargetRef::Player(_) => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Effect, TargetFilter, TargetRef, TypedFilter};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_goad_ability(target: ObjectId, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Goad {
                target: TargetFilter::Any,
            },
            vec![TargetRef::Object(target)],
            ObjectId(100),
            controller,
        )
    }

    fn mark_creature(state: &mut GameState, object_id: ObjectId) {
        state
            .objects
            .get_mut(&object_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
    }

    #[test]
    fn goad_marks_creature_with_goading_player() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_goad_ability(target_id, PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&target_id).unwrap();
        assert!(obj.goaded_by.contains(&PlayerId(0)));
        assert_eq!(obj.goaded_by.len(), 1);
    }

    #[test]
    fn goad_same_player_twice_is_idempotent() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_goad_ability(target_id, PlayerId(0));
        let mut events = Vec::new();

        // CR 701.15d: Same player goading again has no additional effect.
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&target_id).unwrap();
        assert_eq!(obj.goaded_by.len(), 1);
    }

    #[test]
    fn goad_multiple_players() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let mut events = Vec::new();
        // CR 701.15c: Goaded by two different players.
        resolve(
            &mut state,
            &make_goad_ability(target_id, PlayerId(0)),
            &mut events,
        )
        .unwrap();
        resolve(
            &mut state,
            &make_goad_ability(target_id, PlayerId(1)),
            &mut events,
        )
        .unwrap();

        let obj = state.objects.get(&target_id).unwrap();
        assert!(obj.goaded_by.contains(&PlayerId(0)));
        assert!(obj.goaded_by.contains(&PlayerId(1)));
        assert_eq!(obj.goaded_by.len(), 2);
    }

    #[test]
    fn goad_all_marks_matching_creatures_without_explicit_targets() {
        let mut state = GameState::new_two_player(42);
        let opponent_creature_a = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let opponent_creature_b = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        let controller_creature = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Cat".to_string(),
            Zone::Battlefield,
        );
        mark_creature(&mut state, opponent_creature_a);
        mark_creature(&mut state, opponent_creature_b);
        mark_creature(&mut state, controller_creature);
        let ability = ResolvedAbility::new(
            Effect::GoadAll {
                target: TargetFilter::Typed(
                    TypedFilter::creature().controller(ControllerRef::Opponent),
                ),
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        assert!(state
            .objects
            .get(&opponent_creature_a)
            .unwrap()
            .goaded_by
            .contains(&PlayerId(0)));
        assert!(state
            .objects
            .get(&opponent_creature_b)
            .unwrap()
            .goaded_by
            .contains(&PlayerId(0)));
        assert!(!state
            .objects
            .get(&controller_creature)
            .unwrap()
            .goaded_by
            .contains(&PlayerId(0)));
    }

    /// CR 701.15a + CR 701.15b: Maximum Carnage chapter I — "each creature
    /// attacks each combat if able and attacks a player other than you if able"
    /// is the printed goad definition. The parser must lower it to
    /// `Effect::GoadAll` over all creatures; resolving that effect marks every
    /// creature (both the controller's and the opponents') as goaded by the
    /// resolving controller. Reverting `try_parse_goad_equivalent` makes the
    /// chapter line lower to `Effect::Unimplemented` — there is no GoadAll to
    /// resolve and no creature gets goaded, so this test fails.
    #[test]
    fn maximum_carnage_goads_every_creature_via_real_parser() {
        let parsed = crate::parser::parse_oracle_text(
            "Until your next turn, each creature attacks each combat if able and attacks a player other than you if able.",
            "Maximum Carnage",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        let goad_effect = parsed
            .abilities
            .iter()
            .map(|def| def.effect.as_ref().clone())
            .find(|effect| matches!(effect, Effect::GoadAll { .. }))
            .expect("Maximum Carnage chapter I must parse to Effect::GoadAll");

        let mut state = GameState::new_two_player(42);
        let my_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let opp_creature = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Wolf".to_string(),
            Zone::Battlefield,
        );
        for id in [my_creature, opp_creature] {
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(goad_effect, vec![], ObjectId(100), PlayerId(0));
        resolve(&mut state, &ability, &mut Vec::new()).unwrap();

        // CR 701.15b: even the controller's own creature is goaded by the
        // controller — it must then attack a player other than the controller.
        assert!(state.objects[&my_creature].goaded_by.contains(&PlayerId(0)));
        assert!(state.objects[&opp_creature]
            .goaded_by
            .contains(&PlayerId(0)));
    }

    #[test]
    fn goad_nonexistent_target_is_skipped() {
        let mut state = GameState::new_two_player(42);
        let ability = make_goad_ability(ObjectId(999), PlayerId(0));
        let mut events = Vec::new();

        // Should succeed (no-op for missing targets, not an error).
        let result = resolve(&mut state, &ability, &mut events);
        assert!(result.is_ok());
    }

    #[test]
    fn goad_emits_effect_resolved() {
        let mut state = GameState::new_two_player(42);
        let target_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );

        let ability = make_goad_ability(target_id, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Goad,
                ..
            }
        )));
    }
}
