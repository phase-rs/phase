use crate::game::effects::counters::{
    add_counter_with_replacement, stash_pending_counter_completion,
};
use crate::game::quantity::resolve_quantity;
use crate::types::ability::{Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingContinuation, WaitingFor};
use crate::types::identifiers::ObjectId;

/// CR 701.39a: Bolster N.
///
/// "Choose a creature you control with the least toughness or tied for
/// least toughness among creatures you control. Put N +1/+1 counters
/// on that creature."
///
/// Bolster is a keyword action that "chooses" (not "targets") — hexproof and
/// shroud do not prevent bolster, and the choice is made at resolution time.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let count_expr = match &ability.effect {
        Effect::Bolster { count } => count.clone(),
        _ => return Ok(()),
    };

    let source_id = ability.source_id;
    let controller = ability.controller;

    let n = resolve_quantity(state, &count_expr, controller, source_id).max(0) as u32;

    // CR 701.39a: Find creatures controlled by the bolster controller.
    let creatures: Vec<(ObjectId, i32)> = state
        .battlefield
        .iter()
        .filter_map(|&obj_id| {
            let obj = state.objects.get(&obj_id)?;
            if obj.controller != controller {
                return None;
            }
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Creature)
            {
                return None;
            }
            // Use post-layer-system effective toughness.
            let toughness = obj.toughness.unwrap_or(0);
            Some((obj_id, toughness))
        })
        .collect();

    // CR 701.39a: If no creatures, do nothing.
    if creatures.is_empty() {
        return Ok(());
    }

    let min_toughness = creatures.iter().map(|(_, t)| *t).min().unwrap();
    let tied: Vec<ObjectId> = creatures
        .iter()
        .filter(|(_, t)| *t == min_toughness)
        .map(|(id, _)| *id)
        .collect();

    if tied.len() == 1 {
        // CR 701.39a: Unique minimum — auto-choose and add counters.
        if n > 0
            && !add_counter_with_replacement(
                state,
                ability.controller,
                tied[0],
                CounterType::Plus1Plus1,
                n,
                events,
            )
        {
            stash_pending_counter_completion(state, EffectKind::Bolster, source_id);
            return Ok(());
        }

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Bolster,
            source_id,
        });
    } else {
        // CR 701.39a: Multiple creatures tied — controller chooses.
        // Set up a PutCounter continuation that the ChooseFromZoneChoice handler
        // will resolve after the player selects a creature. The handler injects
        // the chosen ID into cont.targets, and PutCounter reads from those targets.
        let continuation = ResolvedAbility::new(
            Effect::PutCounter {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: n as i32 },
                target: crate::types::ability::TargetFilter::Any,
            },
            vec![],
            source_id,
            controller,
        );

        state.pending_continuation = Some(PendingContinuation::new(Box::new(continuation)));
        state.waiting_for = WaitingFor::ChooseFromZoneChoice {
            player: controller,
            cards: tied,
            count: 1,
            up_to: false,
            constraint: None,
            source_id,
        };

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::Bolster,
            source_id,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn setup_creature_with_toughness(
        state: &mut GameState,
        card_id: u32,
        toughness: i32,
    ) -> ObjectId {
        let id = zones::create_object(
            state,
            CardId(card_id as u64),
            PlayerId(0),
            format!("Creature {card_id}"),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_power = Some(1);
        obj.base_toughness = Some(toughness);
        obj.power = Some(1);
        obj.toughness = Some(toughness);
        id
    }

    fn make_bolster_ability(source_id: ObjectId, count: i32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Bolster {
                count: QuantityExpr::Fixed { value: count },
            },
            vec![],
            source_id,
            PlayerId(0),
        )
    }

    #[test]
    fn bolster_auto_chooses_unique_minimum() {
        let mut state = GameState::new_two_player(42);
        let small = setup_creature_with_toughness(&mut state, 1, 1);
        let _big = setup_creature_with_toughness(&mut state, 2, 4);
        let ability = make_bolster_ability(small, 2);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Counters placed on the smallest creature
        let obj = state.objects.get(&small).unwrap();
        assert_eq!(obj.counters.get(&CounterType::Plus1Plus1).copied(), Some(2));
        // Should NOT be waiting for a choice
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseFromZoneChoice { .. }),
            "Should auto-choose when unique minimum"
        );
    }

    #[test]
    fn bolster_prompts_choice_on_tied_minimum() {
        let mut state = GameState::new_two_player(42);
        let a = setup_creature_with_toughness(&mut state, 1, 2);
        let b = setup_creature_with_toughness(&mut state, 2, 2);
        let _big = setup_creature_with_toughness(&mut state, 3, 5);
        let ability = make_bolster_ability(a, 3);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // Should be waiting for a choice between the two tied creatures
        match &state.waiting_for {
            WaitingFor::ChooseFromZoneChoice {
                player,
                cards,
                count,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(*count, 1);
                assert!(cards.contains(&a));
                assert!(cards.contains(&b));
                assert_eq!(cards.len(), 2);
            }
            other => panic!("Expected ChooseFromZoneChoice, got {:?}", other),
        }
        // Continuation should be set for PutCounter
        assert!(state.pending_continuation.is_some());
    }

    #[test]
    fn bolster_does_nothing_with_no_creatures() {
        let mut state = GameState::new_two_player(42);
        // Create a non-creature so the source_id is valid
        let id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );
        let ability = make_bolster_ability(id, 2);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // No events, no waiting
        assert!(events.is_empty());
    }
}
