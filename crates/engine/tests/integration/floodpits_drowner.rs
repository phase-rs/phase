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

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects;
use engine::game::zones::create_object;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityKind, ControllerRef, Effect, EffectScope, FilterProp, QuantityExpr, ResolvedAbility,
    TapStateChange, TargetFilter, TargetRef, TypeFilter, TypedFilter,
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
            counter_type: CounterType::Stun,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::ParentTarget,
        },
        vec![], // empty — ParentTarget inherits parent's targets
        source_id,
        PlayerId(0),
    );

    let mut primary = ResolvedAbility::new(
        Effect::SetTapState {
            target: TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::Opponent),
            ),
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
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
                properties: vec![FilterProp::Counters {
                    counters: engine::types::counter::CounterMatch::OfType(
                        engine::types::counter::CounterType::Stun,
                    ),
                    comparator: engine::types::ability::Comparator::GE,
                    count: QuantityExpr::Fixed { value: 1 },
                }],
            }),
            owner_library: true,
            enter_transformed: false,
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
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
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
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

/// CR 400.3 + CR 701.24a: The production parser chain freezes both owners
/// before either move resolves. A stolen target therefore routes to and
/// shuffles its owner's library alongside the source owner's library.
#[test]
fn parsed_compound_shuffle_tracks_mixed_owners() {
    let mut state = GameState::new_two_player(42);
    for i in 0..5 {
        create_object(
            &mut state,
            CardId(300 + i),
            PlayerId(0),
            format!("P0 Lib {i}"),
            Zone::Library,
        );
        create_object(
            &mut state,
            CardId(400 + i),
            PlayerId(1),
            format!("P1 Lib {i}"),
            Zone::Library,
        );
    }
    let drowner = create_object(
        &mut state,
        CardId(10),
        PlayerId(0),
        "Floodpits Drowner".to_string(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&drowner)
        .expect("source exists")
        .card_types
        .core_types
        .push(CoreType::Creature);
    let stolen = create_object(
        &mut state,
        CardId(11),
        PlayerId(1),
        "Stolen Stunned Creature".to_string(),
        Zone::Battlefield,
    );
    {
        let object = state.objects.get_mut(&stolen).expect("target exists");
        object.controller = PlayerId(0);
        object.card_types.core_types.push(CoreType::Creature);
        object.counters.insert(CounterType::Stun, 1);
    }

    let definition = parse_effect_chain(
        "shuffle ~ and target creature with a stun counter on it into their owners' libraries",
        AbilityKind::Spell,
    );
    let mut ability = build_resolved_from_def(&definition, drowner, PlayerId(0));
    ability.targets = vec![TargetRef::Object(stolen)];

    let mut events = Vec::new();
    effects::resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("parsed compound owner shuffle resolves");

    assert_eq!(state.objects[&drowner].zone, Zone::Library);
    assert_eq!(state.objects[&stolen].zone, Zone::Library);
    assert_eq!(state.objects[&stolen].owner, PlayerId(1));
    let mut shuffled_players: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: engine::types::events::PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect();
    shuffled_players.sort_unstable_by_key(|player| player.0);
    shuffled_players.dedup();
    assert_eq!(shuffled_players, vec![PlayerId(0), PlayerId(1)]);
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
        Effect::SetTapState {
            target: TargetFilter::Typed(tf),
            scope: EffectScope::Single,
            state: TapStateChange::Tap,
        } => {
            assert!(tf.type_filters.contains(&TypeFilter::Creature));
            assert_eq!(tf.controller, Some(ControllerRef::Opponent));
        }
        other => panic!("expected Tap(Typed Creature Opponent), got {:?}", other),
    }
}
