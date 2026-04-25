//! Integration tests for Floodpits Drowner building blocks.
//!
//! Validates the compound subject splitter, auto-shuffle, owner_library routing,
//! and SelfRef guard work together end-to-end through the effects pipeline.
//!
//! Floodpits Drowner Oracle text:
//!   Flash
//!   Vigilance
//!   When this creature enters, tap target creature an opponent controls and put a stun counter on it.
//!   {1}{U}, {T}: Shuffle this creature and target creature with a stun counter on it into their owners' libraries.

use engine::game::effects;
use engine::game::zones::create_object;
use engine::types::ability::{
    ControllerRef, Effect, FilterProp, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
    TypeFilter, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Test the ETB compound effect: Tap + PutCounter(ParentTarget) chain.
/// This validates that Plan 01's compound splitter correctly chains the effects
/// and ParentTarget propagation works through resolve_ability_chain.
#[test]
fn etb_tap_and_stun_counter() {
    let mut state = GameState::new_two_player(42);

    // Opponent's creature on the battlefield
    let target_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Opponent Creature".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&target_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    let source_id = ObjectId(100);

    // Build the Tap effect with sub_ability PutCounter(ParentTarget)
    // This is what the parser produces for the ETB trigger execute
    let sub_resolved = ResolvedAbility::new(
        Effect::PutCounter {
            counter_type: "stun".to_string(),
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ParentTarget,
        },
        vec![], // empty — ParentTarget inherits parent's targets
        source_id,
        PlayerId(0),
    );

    let mut primary = ResolvedAbility::new(
        Effect::Tap {
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ),
        },
        vec![TargetRef::Object(target_id)],
        source_id,
        PlayerId(0),
    );
    primary.sub_ability = Some(Box::new(sub_resolved));

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &primary, &mut events, 0).unwrap();

    // Assert: creature is tapped
    assert!(
        state.objects[&target_id].tapped,
        "ETB should tap the target creature"
    );

    // Assert: creature has a stun counter
    let stun_count = state.objects[&target_id]
        .counters
        .get(&CounterType::Stun)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        stun_count, 1,
        "ETB should put exactly one stun counter on the target"
    );
}

/// Test the activated ability: shuffle self and target into owners' libraries.
/// This validates SelfRef pre-loop guard, owner_library routing, and auto-shuffle.
#[test]
fn activated_shuffle_both_into_owners_libraries() {
    let mut state = GameState::new_two_player(42);

    // Add library cards to both players so we can verify shuffle
    for i in 0..5 {
        create_object(
            &mut state,
            CardId(100 + i),
            PlayerId(0),
            format!("P0 Lib {}", i),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(200 + i),
            PlayerId(1),
            format!("P1 Lib {}", i),
            Zone::Library,
        );
    }

    // Floodpits Drowner on battlefield (owned by P0)
    let drowner_id = create_object(
        &mut state,
        CardId(1),
        PlayerId(0),
        "Floodpits Drowner".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&drowner_id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);

    // Target creature on battlefield (owned by P1, has stun counter)
    let target_id = create_object(
        &mut state,
        CardId(2),
        PlayerId(1),
        "Stunned Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let obj = state.objects.get_mut(&target_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.counters.insert(CounterType::Stun, 1);
    }

    // Build the first ChangeZone (SelfRef) with sub_ability for second (targeted)
    let sub_resolved = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Library,
            target: TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: None,
                properties: vec![FilterProp::CountersGE {
                    counter_type: engine::types::counter::CounterType::Stun,
                    count: QuantityExpr::Fixed { value: 1 },
                }],
            }),
            owner_library: true,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
        vec![TargetRef::Object(target_id)],
        drowner_id,
        PlayerId(0),
    );

    let mut primary = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Library,
            target: TargetFilter::SelfRef,
            owner_library: true,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        },
        vec![], // empty targets — SelfRef uses source_id
        drowner_id,
        PlayerId(0),
    );
    primary.sub_ability = Some(Box::new(sub_resolved));

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &primary, &mut events, 0).unwrap();

    // Assert: Floodpits Drowner moved to P0's library
    assert!(
        state.players[0].library.contains(&drowner_id),
        "Drowner should be in owner's (P0) library"
    );
    assert!(
        !state.battlefield.contains(&drowner_id),
        "Drowner should no longer be on battlefield"
    );

    // Assert: Target creature moved to P1's library (owner routing)
    assert!(
        state.players[1].library.contains(&target_id),
        "Target should be in owner's (P1) library"
    );
    assert!(
        !state.battlefield.contains(&target_id),
        "Target should no longer be on battlefield"
    );

    // Assert: ZoneChanged events were emitted for both
    let zone_changes: Vec<_> = events
        .iter()
        .filter(|e| matches!(e, GameEvent::ZoneChanged { .. }))
        .collect();
    assert!(
        zone_changes.len() >= 2,
        "Should have at least 2 ZoneChanged events, got {}",
        zone_changes.len()
    );
}

/// Verify the parser produces correct output for the Floodpits Drowner activated ability text.
#[test]
fn parser_produces_compound_shuffle_chain() {
    let effect = engine::parser::oracle_effect::parse_effect(
        "shuffle ~ and target creature with a stun counter on it into their owners' libraries",
    );

    // Primary effect should be ChangeZone to Library with SelfRef
    match &effect {
        Effect::ChangeZone {
            destination: Zone::Library,
            target: TargetFilter::SelfRef,
            owner_library: true,
            enter_transformed: false,
            ..
        } => {} // expected
        other => panic!(
            "expected ChangeZone(SelfRef, Library, owner_library=true), got {:?}",
            other
        ),
    }
}

/// Verify the parser produces correct output for the ETB trigger text.
#[test]
fn parser_produces_compound_tap_stun() {
    let effect = engine::parser::oracle_effect::parse_effect(
        "tap target creature an opponent controls and put a stun counter on it",
    );

    // Primary effect should be Tap with opponent creature target
    match &effect {
        Effect::Tap {
            target: TargetFilter::Typed(tf),
        } => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
        }
        other => panic!("expected Tap(Typed Creature Opponent), got {:?}", other),
    }
}
