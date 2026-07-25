use rand::seq::SliceRandom;

use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::zone_pipeline::{self, BatchMoveResult, ZoneMoveRequest};
use crate::types::ability::{
    Effect, EffectError, EffectKind, LibraryPosition, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{BatchCompletion, GameState};
use crate::types::identifiers::ObjectId;
use crate::types::zones::Zone;

/// CR 406.3 + CR 608.2c: Exile the explicit object and the top N cards as one
/// face-down pile. A short library or absent object still exiles whatever is
/// available, but the typed completion performs the "If you do" rider only if
/// the original, complete member set settled in exile.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (object, player_filter, count) = match &ability.effect {
        Effect::ExileFaceDownPile {
            object,
            player,
            count,
        } => (
            object.clone(),
            player.clone(),
            resolve_quantity_with_targets(state, count, ability).max(0) as usize,
        ),
        _ => return Err(EffectError::MissingParam("ExileFaceDownPile".to_string())),
    };

    let player = super::resolve_player_for_context_ref(state, ability, &player_filter);
    let source_object = resolve_pile_object(state, ability, &object);
    let top_cards = state
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .ok_or(EffectError::PlayerNotFound)?
        .library
        .iter()
        .take(count)
        .copied()
        .collect::<Vec<_>>();

    let mut members = source_object.into_iter().collect::<Vec<_>>();
    members.extend(top_cards);
    let requests = members
        .iter()
        .copied()
        .map(|member| {
            ZoneMoveRequest::effect(member, Zone::Exile, ability.source_id).face_down_in_exile()
        })
        .collect();

    let result = zone_pipeline::move_objects_simultaneously_then(
        state,
        requests,
        Some(BatchCompletion::ExileFaceDownPileDeliveryComplete {
            player,
            source_id: ability.source_id,
            members,
            required_member_count: count + 1,
        }),
        events,
    );
    if matches!(result, BatchMoveResult::NeedsChoice) {
        return Ok(());
    }
    Ok(())
}

fn resolve_pile_object(
    state: &GameState,
    ability: &ResolvedAbility,
    object: &TargetFilter,
) -> Option<ObjectId> {
    match object {
        TargetFilter::SelfRef | TargetFilter::TriggeringSource => Some(ability.source_id),
        _ => crate::game::effects::effect_object_targets(object, &ability.targets)
            .into_iter()
            .find(|id| state.objects.contains_key(id)),
    }
}

/// CR 406.3 + CR 608.2c + CR 701.24a: Mark every actually exiled member face
/// down. Only the exact full requested pile may satisfy "If you do"; a missing
/// source, a short library, or any redirected member leaves every settled card
/// exiled face down and skips both shuffle and return.
pub(crate) fn complete_exile_face_down_pile_delivery(
    state: &mut GameState,
    player: crate::types::player::PlayerId,
    source_id: ObjectId,
    mut members: Vec<ObjectId>,
    required_member_count: usize,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    let fully_exiled = members.len() == required_member_count
        && members.iter().all(|id| {
            state
                .objects
                .get(id)
                .is_some_and(|object| object.zone == Zone::Exile)
        });

    if fully_exiled {
        // CR 701.24a: The face-down pile, not the library, is shuffled. The
        // resulting vector is its exact top-to-bottom return order.
        members.shuffle(&mut state.rng);
        for id in &members {
            state
                .objects
                .get_mut(id)
                .expect("settled pile member exists")
                .face_down = false;
        }
        let requests = members
            .iter()
            .rev()
            .copied()
            .map(|id| {
                ZoneMoveRequest::effect(id, Zone::Library, source_id)
                    .at_library_position(LibraryPosition::Top)
            })
            .collect();
        return zone_pipeline::move_objects_simultaneously_then(
            state,
            requests,
            Some(BatchCompletion::ExileFaceDownPileReturnComplete { source_id }),
            events,
        );
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileFaceDownPile,
        source_id,
        subject: None,
    });
    BatchMoveResult::Done
}

pub(crate) fn complete_exile_face_down_pile_return(
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> BatchMoveResult {
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ExileFaceDownPile,
        source_id,
        subject: None,
    });
    BatchMoveResult::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, QuantityExpr, ReplacementDefinition, ReplacementMode,
    };
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::EtbTapState;

    #[test]
    fn complete_pile_returns_exact_source_and_top_cards_to_library_top() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Triumph of Saint Katherine".to_string(),
            Zone::Graveyard,
        );
        let first = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "First".to_string(),
            Zone::Library,
        );
        let second = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Second".to_string(),
            Zone::Library,
        );
        let bottom = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Bottom".to_string(),
            Zone::Library,
        );
        let ability = ResolvedAbility::new(
            Effect::ExileFaceDownPile {
                object: TargetFilter::SelfRef,
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 2 },
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let top = state.players[0]
            .library
            .iter()
            .take(3)
            .copied()
            .collect::<Vec<_>>();
        assert_eq!(top.len(), 3);
        assert!(top.contains(&source));
        assert!(top.contains(&first));
        assert!(top.contains(&second));
        assert_eq!(state.players[0].library[3], bottom);
        assert!(top.iter().all(|id| !state.objects[id].face_down));
    }

    #[test]
    fn short_library_leaves_partial_pile_exiled_face_down() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Triumph".to_string(),
            Zone::Graveyard,
        );
        let only = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Only".to_string(),
            Zone::Library,
        );
        let ability = ResolvedAbility::new(
            Effect::ExileFaceDownPile {
                object: TargetFilter::SelfRef,
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 2 },
            },
            vec![],
            source,
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        for id in [source, only] {
            assert_eq!(state.objects[&id].zone, Zone::Exile);
            assert!(state.objects[&id].face_down);
        }
        assert!(state.players[0].library.is_empty());
    }

    #[test]
    fn delivered_member_is_concealed_before_later_pile_member_parks_for_replacement_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Triumph".to_string(),
            Zone::Graveyard,
        );
        let top_card = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Top card".to_string(),
            Zone::Library,
        );
        let redirect_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Optional Exile Redirect".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&redirect_source)
            .expect("replacement source exists")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .mode(ReplacementMode::Optional { decline: None })
                    .valid_card(TargetFilter::SpecificObject { id: top_card })
                    .destination_zone(Zone::Exile)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::Any,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    )),
            );
        let ability = ResolvedAbility::new(
            Effect::ExileFaceDownPile {
                object: TargetFilter::SelfRef,
                player: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
            },
            vec![],
            source,
            PlayerId(0),
        );

        resolve(&mut state, &ability, &mut Vec::new()).expect("pile resolves until choice");

        assert_eq!(state.objects[&source].zone, Zone::Exile);
        assert!(
            state.objects[&source].face_down,
            "a delivered member must be concealed before a later member parks on CR 616.1"
        );
    }
}
