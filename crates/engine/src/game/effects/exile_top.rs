use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zones;
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (count, player_filter) = match &ability.effect {
        Effect::ExileTop { count, player } => (
            // Use resolve_quantity_with_targets so that TargetZoneCardCount (and
            // HalfRounded wrapping it) can resolve against the targeted player.
            resolve_quantity_with_targets(state, count, ability) as usize,
            player.clone(),
        ),
        _ => return Err(EffectError::MissingParam("ExileTop count".to_string())),
    };

    // CR 115.1: Mirror Draw/Mill/Discard — context-ref filters (Controller, etc.)
    // must consult state slots, not `ability.targets`. Otherwise a chained
    // sub-ability's "exile the top N cards of your library" would inherit the
    // parent's Player target and exile from the wrong library.
    let target_player = super::resolve_player_for_context_ref(state, ability, &player_filter);

    // CR 701.17b: A player can't mill/exile more cards than are in their library;
    // exile as many as possible.
    let player = state
        .players
        .iter()
        .find(|p| p.id == target_player)
        .ok_or(EffectError::PlayerNotFound)?;
    let count = count.min(player.library.len());
    let top_cards: Vec<_> = player
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    for object_id in top_cards {
        zones::move_to_zone(state, object_id, Zone::Exile, events);
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileTop,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        CardTypeSetSource, ControllerRef, FilterProp, QuantityExpr, QuantityRef, TargetFilter,
        TargetRef, TypeFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn make_exile_top_ability(count: u32) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed {
                    value: count as i32,
                },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn exile_top_moves_top_card_of_controller_library() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top".to_string(),
            Zone::Library,
        );
        let bottom = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bottom".to_string(),
            Zone::Library,
        );
        let ability = make_exile_top_ability(1);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&bottom).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_triggering_player_uses_attacking_players_library() {
        let mut state = GameState::new_two_player(42);
        let controller_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Controller Top".to_string(),
            Zone::Library,
        );
        let opponent_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Opponent Top".to_string(),
            Zone::Library,
        );
        let attacker = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Attacker".to_string(),
            Zone::Battlefield,
        );
        state.current_trigger_event = Some(GameEvent::AttackersDeclared {
            attacker_ids: vec![attacker],
            defending_player: PlayerId(0),
            attacks: vec![(
                attacker,
                crate::game::combat::AttackTarget::Player(PlayerId(0)),
            )],
        });
        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::TriggeringPlayer,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&opponent_top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&controller_top).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_moves_multiple_cards() {
        let mut state = GameState::new_two_player(42);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Third".to_string(),
            Zone::Library,
        );
        let ability = make_exile_top_ability(2);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&second).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&third).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_controller_filter_does_not_inherit_parent_player_target() {
        // CR 115.1 regression: a chained ExileTop with `player: Controller`
        // must exile from the spell controller's library, not the parent's
        // inherited Player target.
        let mut state = GameState::new_two_player(42);
        let p0_top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "P0 top".to_string(),
            Zone::Library,
        );
        let p1_top = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "P1 top".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![TargetRef::Player(PlayerId(1))], // inherited parent target
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&p0_top).map(|obj| obj.zone),
            Some(Zone::Exile),
            "P0's library top should be exiled (Controller filter resolves to caster)"
        );
        assert_eq!(
            state.objects.get(&p1_top).map(|obj| obj.zone),
            Some(Zone::Library),
            "P1's library must NOT be exiled — parent target inheritance must not override Controller filter"
        );
    }

    #[test]
    fn exile_top_dynamic_card_type_count_moves_that_many_cards() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Loot, the Key to Everything".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let artifact = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Artifact".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&artifact)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Artifact];

        let enchantment = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Enchantment".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&enchantment)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Enchantment];

        let creature = create_object(
            &mut state,
            CardId(13),
            PlayerId(0),
            "Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types = vec![CoreType::Creature];

        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let third = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Third".to_string(),
            Zone::Library,
        );
        let fourth = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Fourth".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::ExileTop {
                player: TargetFilter::Controller,
                count: QuantityExpr::Ref {
                    qty: QuantityRef::DistinctCardTypes {
                        source: CardTypeSetSource::Objects {
                            filter: TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Permanent)
                                    .with_type(TypeFilter::Non(Box::new(TypeFilter::Land)))
                                    .controller(ControllerRef::You)
                                    .properties(vec![FilterProp::Another]),
                            ),
                        },
                    },
                },
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects.get(&top).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&second).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&third).map(|obj| obj.zone),
            Some(Zone::Exile)
        );
        assert_eq!(
            state.objects.get(&fourth).map(|obj| obj.zone),
            Some(Zone::Library)
        );
    }

    #[test]
    fn exile_top_with_empty_library_resolves_without_error() {
        let mut state = GameState::new_two_player(42);
        let ability = make_exile_top_ability(3);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::ExileTop,
                ..
            }
        )));
    }
}
