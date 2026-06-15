//! Regression tests for GitHub issue #1995: proliferate must not add counters
//! of a type a permanent no longer has after losing its last counter of that
//! kind. CR 701.34a — proliferate only affects permanents/players that already
//! have counters, and only adds kinds already present at count > 0.

use engine::game::effects::counters::resolve_remove;
use engine::game::effects::proliferate::{apply_proliferate, resolve};
use engine::game::zones::create_object;
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn proliferate_ability(controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(Effect::Proliferate, vec![], ObjectId(999), controller)
}

fn remove_all_plus_one(obj: ObjectId, state: &mut GameState) {
    let ability = ResolvedAbility::new(
        Effect::RemoveCounter {
            counter_type: Some(CounterType::Plus1Plus1),
            count: QuantityExpr::Fixed { value: -1 },
            target: TargetFilter::Any,
        },
        vec![TargetRef::Object(obj)],
        ObjectId(998),
        PlayerId(0),
    );
    resolve_remove(state, &ability, &mut Vec::new()).unwrap();
}

#[test]
fn issue_1995_removed_counter_type_not_proliferated() {
    let mut state = GameState::new_two_player(42);
    let creature = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Formerly Pumped".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 1);

    remove_all_plus_one(creature, &mut state);

    let mut events = Vec::new();
    resolve(&mut state, &proliferate_ability(PlayerId(0)), &mut events).unwrap();

    assert!(
        !matches!(state.waiting_for, WaitingFor::ProliferateChoice { .. }),
        "creature with no positive counters must not open proliferate choice"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, GameEvent::PlayerPerformedAction { .. })),
        "empty proliferate still emits PlayerPerformedAction"
    );
}

#[test]
fn issue_1995_stale_zero_map_entry_does_not_reopen_proliferate() {
    let mut state = GameState::new_two_player(42);
    let creature = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Stale Entry".to_string(),
        Zone::Battlefield,
    );
    // Simulate the pre-fix bug: counter removed to zero but map key retained.
    state
        .objects
        .get_mut(&creature)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 0);

    let mut events = Vec::new();
    resolve(&mut state, &proliferate_ability(PlayerId(0)), &mut events).unwrap();

    assert!(
        !matches!(state.waiting_for, WaitingFor::ProliferateChoice { .. }),
        "stale zero-count key must not qualify for proliferate"
    );

    let mut apply_events = Vec::new();
    apply_proliferate(
        &mut state,
        PlayerId(0),
        &[TargetRef::Object(creature)],
        &mut apply_events,
    );

    assert!(
        !state.objects[&creature]
            .counters
            .contains_key(&CounterType::Plus1Plus1),
        "forcing apply on a stale target must not resurrect +1/+1 counters"
    );
}

#[test]
fn issue_1995_mixed_zero_and_positive_only_proliferates_present_kinds() {
    let mut state = GameState::new_two_player(42);
    let artifact = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Battery".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&artifact)
        .unwrap()
        .counters
        .insert(CounterType::Plus1Plus1, 0);
    state
        .objects
        .get_mut(&artifact)
        .unwrap()
        .counters
        .insert(CounterType::Generic("charge".to_string()), 4);

    let mut events = Vec::new();
    apply_proliferate(
        &mut state,
        PlayerId(0),
        &[TargetRef::Object(artifact)],
        &mut events,
    );

    assert_eq!(
        state.objects[&artifact].counters[&CounterType::Generic("charge".to_string())],
        5
    );
    assert!(!state.objects[&artifact]
        .counters
        .contains_key(&CounterType::Plus1Plus1));
}
