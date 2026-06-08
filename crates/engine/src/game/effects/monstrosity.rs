use crate::game::effects::counters::{
    add_counter_with_replacement, stash_pending_counter_completion_with_actions,
};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterPostAction};

/// CR 701.37a: Monstrosity N.
///
/// "If this permanent isn't monstrous, put N +1/+1 counters on it
/// and it becomes monstrous."
///
/// CR 701.37b: Monstrous is a designation that stays until the permanent
/// leaves the battlefield. It is neither an ability nor part of copiable values.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count_expr = match &ability.effect {
        Effect::Monstrosity { count } => count.clone(),
        _ => return Ok(()),
    };

    let source_id = ability.source_id;

    // CR 701.37a: If already monstrous, do nothing.
    if let Some(obj) = state.objects.get(&source_id) {
        if obj.monstrous {
            return Ok(());
        }
    } else {
        return Ok(());
    }

    let n = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as u32;

    // CR 701.37a: Put N +1/+1 counters on the permanent.
    if n > 0
        && !add_counter_with_replacement(
            state,
            ability.controller,
            source_id,
            CounterType::Plus1Plus1,
            n,
            events,
        )
    {
        stash_pending_counter_completion_with_actions(
            state,
            EffectKind::Monstrosity,
            source_id,
            vec![PendingCounterPostAction::MarkMonstrous {
                object_id: source_id,
            }],
        );
        return Ok(());
    }

    // CR 701.37a + CR 701.37b: Set the monstrous designation.
    if let Some(obj) = state.objects.get_mut(&source_id) {
        obj.monstrous = true;
    }

    // CR 701.37a: Emit EffectResolved so "When ~ becomes monstrous" triggers can fire.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Monstrosity,
        source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::types::ability::QuantityExpr;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_creature(state: &mut GameState) -> ObjectId {
        let id = zones::create_object(
            state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(3);
        obj.base_toughness = Some(3);
        obj.power = Some(3);
        obj.toughness = Some(3);
        id
    }

    fn make_monstrosity_ability(source_id: ObjectId, count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Monstrosity { count },
            vec![],
            source_id,
            PlayerId(0),
        )
    }

    #[test]
    fn monstrosity_places_counters_and_sets_monstrous() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let ability = make_monstrosity_ability(id, QuantityExpr::Fixed { value: 4 });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.monstrous);
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), Some(4));
    }

    #[test]
    fn monstrosity_does_nothing_if_already_monstrous() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        // First monstrosity
        let ability = make_monstrosity_ability(id, QuantityExpr::Fixed { value: 4 });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Second monstrosity should do nothing
        events.clear();
        let ability2 = make_monstrosity_ability(id, QuantityExpr::Fixed { value: 4 });
        resolve(&mut state, &ability2, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.monstrous);
        // Still only 4 counters, not 8
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), Some(4));
        // No EffectResolved event should have been emitted
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::EffectResolved { .. })),
            "No events should be emitted when already monstrous"
        );
    }

    #[test]
    fn monstrosity_emits_effect_resolved_event() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let ability = make_monstrosity_ability(id, QuantityExpr::Fixed { value: 3 });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let has_resolved = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Monstrosity,
                    ..
                }
            )
        });
        assert!(has_resolved, "Should emit EffectResolved::Monstrosity");
    }

    #[test]
    fn monstrous_clears_on_zone_change() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        // Make it monstrous
        let ability = make_monstrosity_ability(id, QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.objects.get(&id).unwrap().monstrous);

        // Move to graveyard (dies)
        zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        assert!(
            !state.objects.get(&id).unwrap().monstrous,
            "Monstrous should clear on leaving battlefield"
        );

        // Move back to battlefield (reanimate)
        zones::move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
        assert!(
            !state.objects.get(&id).unwrap().monstrous,
            "Monstrous should remain false after re-entering battlefield"
        );
    }
}
