use crate::game::effects::counters::{
    add_counter_with_replacement, stash_pending_counter_completion,
};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// CR 701.46a: Adapt N.
///
/// "If this permanent has no +1/+1 counters on it, put N +1/+1 counters on it."
///
/// Unlike Monstrosity (which uses a `monstrous` designation flag), Adapt's guard
/// is purely counter-based: it checks for the presence of any +1/+1 counters.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count_expr = match &ability.effect {
        Effect::Adapt { count } => count.clone(),
        _ => return Ok(()),
    };

    let source_id = ability.source_id;

    // CR 701.46a: If the permanent has any +1/+1 counters, do nothing.
    if let Some(obj) = state.objects.get(&source_id) {
        if obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0)
            > 0
        {
            return Ok(());
        }
    } else {
        return Ok(());
    }

    let n = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as u32;

    // CR 701.46a: Put N +1/+1 counters on the permanent.
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
        stash_pending_counter_completion(state, EffectKind::Adapt, source_id);
        return Ok(());
    }

    // CR 701.46a: Emit EffectResolved so "When ~ adapts" triggers can fire.
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Adapt,
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

    fn make_adapt_ability(source_id: ObjectId, count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Adapt { count }, vec![], source_id, PlayerId(0))
    }

    #[test]
    fn adapt_places_counters_when_none_present() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let ability = make_adapt_ability(id, QuantityExpr::Fixed { value: 4 });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), Some(4));
    }

    #[test]
    fn adapt_does_nothing_if_has_plus1_counters() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);

        // First adapt — should place counters
        let ability = make_adapt_ability(id, QuantityExpr::Fixed { value: 3 });
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state
                .objects
                .get(&id)
                .unwrap()
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(3)
        );

        // Second adapt — should do nothing (already has +1/+1 counters)
        events.clear();
        let ability2 = make_adapt_ability(id, QuantityExpr::Fixed { value: 3 });
        resolve(&mut state, &ability2, &mut events).unwrap();

        // Still only 3 counters, not 6
        assert_eq!(
            state
                .objects
                .get(&id)
                .unwrap()
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied(),
            Some(3)
        );
        // No EffectResolved event on blocked adapt
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, GameEvent::EffectResolved { .. })),
            "No events should be emitted when adapt is blocked by existing counters"
        );
    }

    #[test]
    fn adapt_emits_effect_resolved_event() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let ability = make_adapt_ability(id, QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let has_resolved = events.iter().any(|e| {
            matches!(
                e,
                GameEvent::EffectResolved {
                    kind: EffectKind::Adapt,
                    ..
                }
            )
        });
        assert!(has_resolved, "Should emit EffectResolved::Adapt");
    }
}
