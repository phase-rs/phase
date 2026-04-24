use std::collections::HashSet;

use crate::game::filter;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;

use super::resolve_ability_chain;

/// Add a +1/+1 counter to the exploring creature via the replacement pipeline.
fn add_explore_counter(state: &mut GameState, explorer_id: ObjectId, events: &mut Vec<GameEvent>) {
    let proposed = ProposedEvent::AddCounter {
        object_id: explorer_id,
        counter_type: CounterType::Plus1Plus1,
        count: 1,
        applied: HashSet::new(),
    };

    if let ReplacementResult::Execute(ProposedEvent::AddCounter {
        object_id,
        counter_type,
        count,
        ..
    }) = replacement::replace_event(state, proposed, events)
    {
        if let Some(obj) = state.objects.get_mut(&object_id) {
            let entry = obj.counters.entry(counter_type.clone()).or_insert(0);
            *entry += count;
            state.layers_dirty = true;
        }
        events.push(GameEvent::CounterAdded {
            object_id,
            counter_type,
            count,
        });
    }
}

fn next_explore_chooser(
    state: &GameState,
    remaining: &[ObjectId],
) -> Option<(PlayerId, Vec<ObjectId>)> {
    let apnap = crate::game::players::apnap_order(state);
    for player in apnap {
        let choosable: Vec<ObjectId> = remaining
            .iter()
            .copied()
            .filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|object| object.controller == player)
            })
            .collect();
        if !choosable.is_empty() {
            return Some((player, choosable));
        }
    }
    None
}

fn collect_explorers(
    state: &GameState,
    ability: &ResolvedAbility,
    filter_spec: &TargetFilter,
) -> Vec<ObjectId> {
    match filter_spec {
        TargetFilter::ParentTarget => ability
            .targets
            .iter()
            .filter_map(|target| match target {
                TargetRef::Object(id) => Some(*id),
                _ => None,
            })
            .filter(|obj_id| state.objects.contains_key(obj_id))
            .collect(),
        TargetFilter::TrackedSet { id } => state
            .tracked_object_sets
            .get(id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|obj_id| state.objects.contains_key(obj_id))
            .collect(),
        _ => {
            // CR 107.3a + CR 601.2b: ability-context filter evaluation.
            let ctx = filter::FilterContext::from_ability(ability);
            state
                .battlefield
                .iter()
                .copied()
                .filter(|obj_id| filter::matches_target_filter(state, *obj_id, filter_spec, &ctx))
                .collect()
        }
    }
}

fn continuation_for_remaining(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining: Vec<ObjectId>,
) -> Option<ResolvedAbility> {
    if remaining.is_empty() {
        return None;
    }

    let tracked_set_id = crate::types::identifiers::TrackedSetId(state.next_tracked_set_id);
    state.next_tracked_set_id += 1;
    state.tracked_object_sets.insert(tracked_set_id, remaining);

    Some(
        ResolvedAbility::new(
            Effect::ExploreAll {
                filter: TargetFilter::TrackedSet { id: tracked_set_id },
            },
            vec![],
            ability.source_id,
            ability.controller,
        )
        .kind(ability.kind)
        .context(ability.context.clone()),
    )
}

fn resolve_single_explorer(
    state: &mut GameState,
    ability: &ResolvedAbility,
    explorer_id: ObjectId,
    remaining: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let mut single = ResolvedAbility::new(
        Effect::Explore,
        vec![TargetRef::Object(explorer_id)],
        ability.source_id,
        ability.controller,
    )
    .kind(ability.kind)
    .context(ability.context.clone());

    if let Some(next) = continuation_for_remaining(state, ability, remaining) {
        single = single.sub_ability(next);
    } else if let Some(sub) = ability.sub_ability.as_deref() {
        single = single.sub_ability(sub.clone());
    }

    resolve_ability_chain(state, &single, events, 1)
}

fn advance_explore_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    remaining: Vec<ObjectId>,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Some((player, choosable)) = next_explore_chooser(state, &remaining) else {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    };

    if choosable.len() == 1 {
        let chosen = choosable[0];
        let remaining: Vec<ObjectId> = remaining
            .into_iter()
            .filter(|obj_id| *obj_id != chosen)
            .collect();
        return resolve_single_explorer(state, ability, chosen, remaining, events);
    }

    state.waiting_for = WaitingFor::ExploreChoice {
        player,
        source_id: ability.source_id,
        choosable,
        remaining,
        pending_effect: Box::new(ability.clone()),
    };
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
    });
    Ok(())
}

/// CR 701.44a: Explore — reveal the top card of the exploring creature's controller's library.
/// - If the card is a land: put it into that player's hand (no counter).
/// - If the card is not a land: put a +1/+1 counter on the creature, then the player
///   chooses to put the card back on top or into their graveyard
///   (reuses WaitingFor::DigChoice with keep_count=1).
///
/// The exploring creature defaults to the ability's source_id.
/// If the ability has a targeted object, that creature explores instead.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    // Determine the exploring creature
    let explorer_id = ability
        .targets
        .iter()
        .find_map(|t| {
            if let TargetRef::Object(id) = t {
                Some(*id)
            } else {
                None
            }
        })
        .unwrap_or(ability.source_id);

    let controller = state
        .objects
        .get(&explorer_id)
        .map(|object| object.controller)
        .unwrap_or(ability.controller);

    // Find the controller's library
    let player = state
        .players
        .iter()
        .find(|p| p.id == controller)
        .ok_or(EffectError::PlayerNotFound)?;

    if player.library.is_empty() {
        // CR 701.44a: Explore with empty library — just put a +1/+1 counter.
        add_explore_counter(state, explorer_id, events);

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
        return Ok(());
    }

    // Reveal top card
    let top_card_id = player.library[0];
    let revealed_name = state
        .objects
        .get(&top_card_id)
        .map(|o| o.name.clone())
        .unwrap_or_default();
    events.push(GameEvent::CardsRevealed {
        player: controller,
        card_ids: vec![top_card_id],
        card_names: vec![revealed_name],
    });

    // Check if it's a land
    let is_land = state
        .objects
        .get(&top_card_id)
        .map(|obj| obj.card_types.core_types.contains(&CoreType::Land))
        .unwrap_or(false);

    if is_land {
        // CR 701.44a: Land revealed — put the card into the player's hand. No counter.
        if let Some(player) = state.players.iter_mut().find(|p| p.id == controller) {
            player.library.retain(|id| *id != top_card_id);
            player.hand.push_back(top_card_id);
        }
        if let Some(obj) = state.objects.get_mut(&top_card_id) {
            obj.zone = crate::types::zones::Zone::Hand;
        }

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
    } else {
        // CR 701.44a: Nonland revealed — put a +1/+1 counter on the creature,
        // then player chooses to put the card back on top or into graveyard.
        add_explore_counter(state, explorer_id, events);

        // Reuse WaitingFor::DigChoice with keep_count=1:
        //   - selected cards go to hand (keep_count=1 means choose 1 to keep)
        //   - rest go to graveyard (but there's only 1 card, so keep=hand, don't keep=graveyard)
        state.waiting_for = WaitingFor::DigChoice {
            player: controller,
            selectable_cards: vec![top_card_id],
            cards: vec![top_card_id],
            keep_count: 1,
            up_to: false,
            kept_destination: None,
            rest_destination: None,
            source_id: Some(ability.source_id),
        };

        events.push(GameEvent::EffectResolved {
            kind: EffectKind::from(&ability.effect),
            source_id: ability.source_id,
        });
    }

    Ok(())
}

/// CR 701.44d: If multiple permanents explore simultaneously, controllers choose
/// the order within APNAP buckets and each permanent explores one at a time.
pub fn resolve_all(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::ExploreAll { filter } = &ability.effect else {
        return Ok(());
    };
    let remaining = collect_explorers(state, ability, filter);
    advance_explore_all(state, ability, remaining, events)
}

pub fn handle_choice(
    state: &mut GameState,
    chosen: ObjectId,
    remaining: &[ObjectId],
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, crate::game::engine::EngineError> {
    let WaitingFor::ExploreChoice { choosable, .. } = &state.waiting_for else {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Not waiting for explore choice".to_string(),
        ));
    };
    if !choosable.contains(&chosen) {
        return Err(crate::game::engine::EngineError::InvalidAction(
            "Invalid explore choice".to_string(),
        ));
    }

    let remaining: Vec<ObjectId> = remaining
        .iter()
        .copied()
        .filter(|obj_id| *obj_id != chosen)
        .collect();
    resolve_single_explorer(state, ability, chosen, remaining, events).map_err(|err| {
        crate::game::engine::EngineError::InvalidAction(format!(
            "Failed to continue explore sequence: {err}"
        ))
    })?;
    Ok(state.waiting_for.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{ControllerRef, Effect, TargetFilter, TargetRef, TypedFilter};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_explore_ability(source_id: ObjectId) -> ResolvedAbility {
        ResolvedAbility::new(Effect::Explore, vec![], source_id, PlayerId(0))
    }

    #[test]
    fn test_explore_land_goes_to_hand() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Jadelight Ranger".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&explorer).unwrap().power = Some(2);
        state.objects.get_mut(&explorer).unwrap().toughness = Some(1);
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Put a land on top of library
        let land_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.44a: Land revealed — no counter on explorer
        assert!(!state.objects[&explorer]
            .counters
            .contains_key(&CounterType::Plus1Plus1));
        // Land moved to hand
        assert!(state.players[0].hand.contains(&land_id));
        // Land removed from library
        assert!(!state.players[0].library.contains(&land_id));
    }

    #[test]
    fn test_explore_nonland_adds_counter_and_gives_choice() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Merfolk Branchwalker".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Put a nonland on top of library
        let spell_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&spell_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // CR 701.44a: Nonland revealed — explorer gets +1/+1 counter
        assert_eq!(
            state.objects[&explorer].counters[&CounterType::Plus1Plus1],
            1
        );

        // Player chooses to put card back on top or into graveyard
        match &state.waiting_for {
            WaitingFor::DigChoice {
                player,
                cards,
                keep_count,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(cards.len(), 1);
                assert_eq!(cards[0], spell_id);
                assert_eq!(*keep_count, 1);
            }
            other => panic!("Expected DigChoice, got {:?}", other),
        }
    }

    #[test]
    fn test_explore_empty_library_adds_counter() {
        let mut state = GameState::new_two_player(42);
        let explorer = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Explorer".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&explorer)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        assert!(state.players[0].library.is_empty());

        let ability = make_explore_ability(explorer);
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        // With empty library, explorer still gets +1/+1 counter
        assert_eq!(
            state.objects[&explorer].counters[&CounterType::Plus1Plus1],
            1
        );
    }

    #[test]
    fn targeted_explore_uses_target_controllers_library() {
        let mut state = GameState::new_two_player(42);
        let target = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let opponent_top = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opponent Spell".to_string(),
            Zone::Library,
        );
        state
            .objects
            .get_mut(&opponent_top)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Sorcery);
        let _controller_top = create_object(
            &mut state,
            CardId(12),
            PlayerId(0),
            "Controller Land".to_string(),
            Zone::Library,
        );

        let ability = ResolvedAbility::new(
            Effect::Explore,
            vec![TargetRef::Object(target)],
            ObjectId(900),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&target].counters[&CounterType::Plus1Plus1],
            1,
            "targeted explore should put the counter on the chosen creature"
        );
        match &state.waiting_for {
            WaitingFor::DigChoice { player, cards, .. } => {
                assert_eq!(*player, PlayerId(1));
                assert_eq!(cards.as_slice(), &[opponent_top]);
            }
            other => panic!("expected DigChoice from opponent library, got {other:?}"),
        }
    }

    #[test]
    fn explore_all_waits_for_choice_when_one_player_has_multiple_explorers() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(20),
            PlayerId(0),
            "First".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(21),
            PlayerId(0),
            "Second".to_string(),
            Zone::Battlefield,
        );
        for creature in [first, second] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        state
            .objects
            .get_mut(&second)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let ability = ResolvedAbility::new(
            Effect::ExploreAll {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
            vec![],
            ObjectId(901),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ExploreChoice {
                player,
                choosable,
                remaining,
                ..
            } => {
                assert_eq!(*player, PlayerId(0));
                assert_eq!(choosable.len(), 2);
                assert!(choosable.contains(&first));
                assert!(choosable.contains(&second));
                assert_eq!(remaining.len(), 2);
            }
            other => panic!("expected ExploreChoice, got {other:?}"),
        }
    }

    #[test]
    fn explore_all_parent_target_uses_inherited_targets() {
        let mut state = GameState::new_two_player(42);
        let first = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "First".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(31),
            PlayerId(0),
            "Second".to_string(),
            Zone::Battlefield,
        );
        let outsider = create_object(
            &mut state,
            CardId(32),
            PlayerId(0),
            "Outsider".to_string(),
            Zone::Battlefield,
        );
        for creature in [first, second, outsider] {
            state
                .objects
                .get_mut(&creature)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let ability = ResolvedAbility::new(
            Effect::ExploreAll {
                filter: TargetFilter::ParentTarget,
            },
            vec![TargetRef::Object(first), TargetRef::Object(second)],
            ObjectId(902),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve_all(&mut state, &ability, &mut events).unwrap();

        match &state.waiting_for {
            WaitingFor::ExploreChoice { choosable, .. } => {
                assert_eq!(choosable.len(), 2);
                assert!(choosable.contains(&first));
                assert!(choosable.contains(&second));
                assert!(!choosable.contains(&outsider));
            }
            other => panic!("expected ExploreChoice, got {other:?}"),
        }
    }
}
