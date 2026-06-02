use crate::game::effects::counters::add_counter_with_replacement;
use crate::game::quantity::resolve_quantity_with_targets;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 702.112a: Renown N.
///
/// "When this creature deals combat damage to a player, if it isn't renowned,
/// put N +1/+1 counters on it and it becomes renowned."
///
/// CR 702.112b: Renowned is a designation that stays until the permanent
/// leaves the battlefield. It is neither an ability nor part of copiable values.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count_expr = match &ability.effect {
        Effect::Renown { count } => count.clone(),
        _ => return Ok(()),
    };

    let source_id = ability.source_id;

    // CR 702.112a-b: only permanents can become renowned. If the source is no
    // longer on the battlefield or is already renowned, do nothing. This is the
    // resolution-time intervening-if check for multiple renown instances.
    if let Some(obj) = state.objects.get(&source_id) {
        if obj.zone != Zone::Battlefield || obj.is_renowned {
            return Ok(());
        }
    } else {
        return Ok(());
    }

    let n = resolve_quantity_with_targets(state, &count_expr, ability).max(0) as u32;

    if n > 0 {
        add_counter_with_replacement(
            state,
            ability.controller,
            source_id,
            CounterType::Plus1Plus1,
            n,
            events,
        );
    }

    if let Some(obj) = state.objects.get_mut(&source_id) {
        obj.is_renowned = true;
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::Renown,
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
        obj.base_power = Some(2);
        obj.base_toughness = Some(2);
        obj.power = Some(2);
        obj.toughness = Some(2);
        id
    }

    fn make_renown_ability(source_id: ObjectId, count: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Renown { count }, vec![], source_id, PlayerId(0))
    }

    #[test]
    fn renown_places_counters_and_sets_designation() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let ability = make_renown_ability(id, QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_renowned);
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), Some(2));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::Renown,
                    source_id,
                } if *source_id == id
            )
        }));
    }

    #[test]
    fn renown_does_nothing_if_already_renowned() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        state.objects.get_mut(&id).unwrap().is_renowned = true;
        let ability = make_renown_ability(id, QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(obj.is_renowned);
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), None);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::EffectResolved { .. })));
    }

    #[test]
    fn renown_does_nothing_if_source_left_battlefield() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let ability = make_renown_ability(id, QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        events.clear();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = state.objects.get(&id).unwrap();
        assert!(!obj.is_renowned);
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), None);
        assert!(events.is_empty());
    }

    #[test]
    fn renowned_clears_on_zone_change() {
        let mut state = GameState::new_two_player(42);
        let id = setup_creature(&mut state);
        let ability = make_renown_ability(id, QuantityExpr::Fixed { value: 1 });
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.objects.get(&id).unwrap().is_renowned);

        zones::move_to_zone(&mut state, id, Zone::Graveyard, &mut events);
        assert!(!state.objects.get(&id).unwrap().is_renowned);

        zones::move_to_zone(&mut state, id, Zone::Battlefield, &mut events);
        assert!(!state.objects.get(&id).unwrap().is_renowned);
    }
}
