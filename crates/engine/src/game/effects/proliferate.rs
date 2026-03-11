use crate::game::game_object::CounterType;
use crate::types::ability::{effect_variant_name, EffectError, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;

/// Proliferate: for each permanent that has a counter and each player that has a
/// poison counter, add one additional counter of each type already present.
///
/// Per MTG rules, the controller chooses which permanents/players to proliferate.
/// For simplicity, we proliferate all eligible permanents and players automatically.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Collect permanents on battlefield with counters
    let permanents_with_counters: Vec<_> = state
        .battlefield
        .iter()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|obj| !obj.counters.is_empty())
                .unwrap_or(false)
        })
        .copied()
        .collect();

    // For each permanent, add one counter of each type already present
    for obj_id in permanents_with_counters {
        // Collect counter types and current counts first
        let counter_types: Vec<CounterType> = state
            .objects
            .get(&obj_id)
            .map(|obj| obj.counters.keys().cloned().collect())
            .unwrap_or_default();

        for ct in counter_types {
            if let Some(obj) = state.objects.get_mut(&obj_id) {
                if let Some(entry) = obj.counters.get_mut(&ct) {
                    *entry += 1;
                }
            }

            // Mark layers dirty for P/T-affecting counters
            if matches!(ct, CounterType::Plus1Plus1 | CounterType::Minus1Minus1) {
                state.layers_dirty = true;
            }

            let counter_type_str = match &ct {
                CounterType::Plus1Plus1 => "P1P1".to_string(),
                CounterType::Minus1Minus1 => "M1M1".to_string(),
                CounterType::Loyalty => "LOYALTY".to_string(),
                CounterType::Generic(s) => s.clone(),
            };

            events.push(GameEvent::CounterAdded {
                object_id: obj_id,
                counter_type: counter_type_str,
                count: 1,
            });
        }
    }

    // Note: poison counter proliferation on players is deferred (poison counters not yet tracked)

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
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
    use crate::types::zones::Zone;
    use std::collections::HashMap;

    fn make_proliferate_ability() -> ResolvedAbility {
        ResolvedAbility::from_raw(
            "Proliferate",
            HashMap::new(),
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn test_proliferate_adds_counters() {
        let mut state = GameState::new_two_player(42);
        let obj1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature A".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj1)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        let obj2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Creature B".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj2)
            .unwrap()
            .counters
            .insert(CounterType::Minus1Minus1, 1);

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Creature A: 2 + 1 = 3 +1/+1 counters
        assert_eq!(state.objects[&obj1].counters[&CounterType::Plus1Plus1], 3);
        // Creature B: 1 + 1 = 2 -1/-1 counters
        assert_eq!(state.objects[&obj2].counters[&CounterType::Minus1Minus1], 2);
    }

    #[test]
    fn test_proliferate_skips_permanents_without_counters() {
        let mut state = GameState::new_two_player(42);
        let with_counters = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "With".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&with_counters)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let without_counters = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Without".to_string(),
            Zone::Battlefield,
        );

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&with_counters].counters[&CounterType::Plus1Plus1],
            2
        );
        assert!(state.objects[&without_counters].counters.is_empty());
    }

    #[test]
    fn test_proliferate_multiple_counter_types() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Generic("charge".to_string()), 3);

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.objects[&obj].counters[&CounterType::Plus1Plus1], 2);
        assert_eq!(
            state.objects[&obj].counters[&CounterType::Generic("charge".to_string())],
            4
        );
    }

    #[test]
    fn test_proliferate_emits_counter_added_events() {
        let mut state = GameState::new_two_player(42);
        let obj = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&obj)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 1);

        let ability = make_proliferate_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::CounterAdded {
                counter_type,
                count: 1,
                ..
            } if counter_type == "P1P1"
        )));
    }
}
