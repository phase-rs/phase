//! CR 609.3 + CR 608.2c: a `ChangeZone` that moves nothing records "zero moved",
//! so a following "this way" count (`QuantityRef::PreviousEffectCount`) reads 0
//! instead of a count an earlier step of the chain left behind.
//!
//! Real cards: no printed card is this exact chain. The guards below are shared
//! by every untargeted-anaphor move, and the `GainLife` follower stands for the
//! "for each card exiled this way" class.

use engine::game::effects::resolve_ability_chain;
use engine::game::triggers::trigger_source_context_for_latch;
use engine::game::zones::{self, create_object};
use engine::types::ability::{
    Effect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

/// The "this way" count a stale earlier chain step left behind.
const STALE_COUNT: i32 = 3;

fn creature(state: &mut GameState, card: u64, name: &str) -> ObjectId {
    let id = create_object(
        state,
        CardId(card),
        PlayerId(0),
        name.into(),
        Zone::Battlefield,
    );
    state
        .objects
        .get_mut(&id)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    id
}

/// One move of `target` from `origin` to `destination` whose follower gains
/// life equal to the "this way" count.
fn move_then_gain_previous_count(
    state: &GameState,
    source: ObjectId,
    origin: Zone,
    destination: Zone,
    target: TargetFilter,
    targets: Vec<TargetRef>,
) -> ResolvedAbility {
    let mut ability = ResolvedAbility::new(
        Effect::ChangeZone {
            library_shuffle: Default::default(),
            origin: Some(origin),
            destination,
            target,
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
        targets,
        source,
        PlayerId(0),
    )
    .sub_ability(ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::PreviousEffectCount,
            },
            player: TargetFilter::Controller,
        },
        vec![],
        source,
        PlayerId(0),
    ));
    ability.trigger_source = Some(trigger_source_context_for_latch(
        state,
        &state.objects[&source],
    ));
    ability
}

/// Resolve `ability` through the chain resolver with a stale prior count and
/// return `(last_effect_count, life gained)`.
fn resolve_with_stale_count(
    mut state: GameState,
    ability: &ResolvedAbility,
) -> (GameState, Option<i32>, i32) {
    state.last_effect_count = Some(STALE_COUNT);
    let life_before = state.players[0].life;
    let mut events = Vec::new();
    resolve_ability_chain(&mut state, ability, &mut events, 0).unwrap();
    let count = state.last_effect_count;
    let gained = state.players[0].life - life_before;
    (state, count, gained)
}

/// CR 400.7: an untargeted "it" whose inferred referent was the ability's own
/// source names nothing once that source changed zones.
#[test]
fn inferred_source_unbound_stamps_zero_moved() {
    let mut state = GameState::new_two_player(42);
    let source = creature(&mut state, 1, "Source");
    let ability = move_then_gain_previous_count(
        &state,
        source,
        Zone::Battlefield,
        Zone::Exile,
        TargetFilter::ParentTarget,
        vec![],
    );
    zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut Vec::new());
    assert_eq!(
        state.objects[&source].zone,
        Zone::Graveyard,
        "reach: the source departed before the ability resolved"
    );

    let (state, count, gained) = resolve_with_stale_count(state, &ability);
    assert_eq!(
        state.objects[&source].zone,
        Zone::Graveyard,
        "nothing moved"
    );
    assert_eq!(count, Some(0), "the guard stamps zero moved");
    assert_eq!(gained, 0, "the follower read 0, not the stale count");
}

/// CR 400.7: a source that left and returned is a new object, so `SelfRef`
/// names nothing.
#[test]
fn stale_self_ref_stamps_zero_moved() {
    let mut state = GameState::new_two_player(42);
    let source = creature(&mut state, 1, "Source");
    let ability = move_then_gain_previous_count(
        &state,
        source,
        Zone::Battlefield,
        Zone::Exile,
        TargetFilter::SelfRef,
        vec![],
    );
    let mut events = Vec::new();
    zones::move_to_zone(&mut state, source, Zone::Exile, &mut events);
    zones::move_to_zone(&mut state, source, Zone::Battlefield, &mut events);
    assert!(
        !ability.self_ref_is_current(&state),
        "reach: the source is a new object, so the SelfRef guard applies"
    );

    let (state, count, gained) = resolve_with_stale_count(state, &ability);
    assert_eq!(
        state.objects[&source].zone,
        Zone::Battlefield,
        "nothing moved"
    );
    assert_eq!(count, Some(0), "the guard stamps zero moved");
    assert_eq!(gained, 0, "the follower read 0, not the stale count");
}

/// CR 608.2b: an empty target slot (its target illegal as the ability resolved)
/// moves nothing.
#[test]
fn empty_target_slot_stamps_zero_moved() {
    let mut state = GameState::new_two_player(42);
    let source = creature(&mut state, 1, "Source");
    let ability = move_then_gain_previous_count(
        &state,
        source,
        Zone::Battlefield,
        Zone::Exile,
        TargetFilter::ParentTargetSlot { index: 0 },
        vec![],
    );
    assert!(
        ability.targets.is_empty(),
        "reach: no target was declared for slot 0"
    );

    let (state, count, gained) = resolve_with_stale_count(state, &ability);
    assert_eq!(
        state.objects[&source].zone,
        Zone::Battlefield,
        "nothing moved"
    );
    assert_eq!(count, Some(0), "the guard stamps zero moved");
    assert_eq!(gained, 0, "the follower read 0, not the stale count");
}

/// CR 701.23b + CR 609.3: a search of an empty library finds nothing, so the
/// move is a zero-moved no-op (the tutor-shaped chain).
#[test]
fn empty_library_fail_to_find_stamps_zero_moved() {
    let mut state = GameState::new_two_player(42);
    let source = creature(&mut state, 1, "Source");
    let ability = move_then_gain_previous_count(
        &state,
        source,
        Zone::Library,
        Zone::Hand,
        TargetFilter::Any,
        vec![],
    );
    assert!(
        state.players[0].library.is_empty(),
        "reach: the library to search is empty"
    );

    let (_state, count, gained) = resolve_with_stale_count(state, &ability);
    assert_eq!(count, Some(0), "the empty search stamps zero moved");
    assert_eq!(gained, 0, "the follower read 0, not the stale count");
}

/// Positive control: a real move overwrites the stale count with the number of
/// objects moved, and the follower reads that count.
#[test]
fn a_real_move_records_the_moved_count_for_the_follower() {
    let mut state = GameState::new_two_player(42);
    let source = creature(&mut state, 1, "Source");
    let victim = creature(&mut state, 2, "Victim");
    let ability = move_then_gain_previous_count(
        &state,
        source,
        Zone::Battlefield,
        Zone::Exile,
        TargetFilter::Any,
        vec![TargetRef::Object(victim)],
    );

    let (state, count, gained) = resolve_with_stale_count(state, &ability);
    assert_eq!(state.objects[&victim].zone, Zone::Exile, "the target moved");
    assert_eq!(count, Some(1), "one object moved");
    assert_eq!(
        gained, 1,
        "the follower read the moved count, not the stale 3"
    );
}
