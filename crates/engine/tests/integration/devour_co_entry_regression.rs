//! Regression for Devour co-entry (CR 702.82 / CR 614.12a / CR 614.13a-b):
//! when one or more Devour creatures enter the battlefield simultaneously with
//! other permanents (via a single `Effect::ChangeZoneAll`), the as-enters Devour
//! sacrifice MUST NOT be able to sacrifice:
//!   * the devourer itself (CR 614.13a — the permanent that's entering),
//!   * any other object entering at the same time (CR 614.13a),
//!   * a co-entering second devourer (CR 614.12a + CR 614.13a — both choices are
//!     made before either devourer "enters", so neither can devour the other).
//!
//! Both tests drive the REAL production `change_zone::resolve_all` path (the same
//! path a "put all creature cards exiled this way onto the battlefield" /
//! reanimation-style `ChangeZoneAll` flows through) with synthetic objects, so
//! they need no card-data and exercise the engine end-to-end. The Devour
//! replacement is constructed to match `database::synthesis::synthesize_devour`.

use engine::game::effects::change_zone::{resolve, resolve_all};
use engine::game::effects::sacrifice;
use engine::game::engine::apply_as_current;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, Effect, QuantityExpr, QuantityRef,
    ReplacementDefinition, ResolvedAbility, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Build the Devour-N as-enters replacement, mirroring
/// `database::synthesis::synthesize_devour`: a `Moved` replacement valid on
/// SelfRef whose `execute` is a ranged (up-to, min 0) `Sacrifice` over
/// "creatures you control", chained to a self-targeted `PutCounter`.
fn devour_replacement(n: i32) -> ReplacementDefinition {
    // Mirror `synthesize_devour` exactly: for n == 1 the per-creature counter
    // count is a bare `Ref` (no Multiply); only n > 1 wraps it in `Multiply`.
    let counter_count = if n == 1 {
        QuantityExpr::Ref {
            qty: QuantityRef::EventContextAmount,
        }
    } else {
        QuantityExpr::Multiply {
            factor: n,
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::EventContextAmount,
            }),
        }
    };
    let put_counters = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: counter_count,
            target: TargetFilter::SelfRef,
        },
    );
    let sacrifice = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            count: QuantityExpr::up_to(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::You),
                    ),
                },
            }),
            min_count: 0,
        },
    )
    .sub_ability(put_counters);

    ReplacementDefinition {
        event: ReplacementEvent::Moved,
        execute: Some(Box::new(sacrifice)),
        valid_card: Some(TargetFilter::SelfRef),
        ..ReplacementDefinition::new(ReplacementEvent::Moved)
    }
}

/// Create a creature in `zone` controlled/owned by P0.
fn make_creature(state: &mut GameState, id: u64, name: &str, zone: Zone) -> ObjectId {
    let oid = create_object(state, CardId(id), PlayerId(0), name.to_string(), zone);
    state
        .objects
        .get_mut(&oid)
        .unwrap()
        .card_types
        .core_types
        .push(CoreType::Creature);
    oid
}

/// Create a Devour-N creature in Exile (so a `ChangeZoneAll { Exile -> Battlefield }`
/// co-entry brings it in alongside the other Exile members).
fn make_devourer_in_exile(state: &mut GameState, id: u64, name: &str, n: i32) -> ObjectId {
    let oid = make_creature(state, id, name, Zone::Exile);
    state.objects.get_mut(&oid).unwrap().replacement_definitions =
        vec![devour_replacement(n)].into();
    oid
}

/// CR 614.13a: a Devour creature entering simultaneously with an ordinary
/// creature (one `ChangeZoneAll`) cannot devour that co-arriver, nor itself; a
/// pre-existing battlefield creature IS eligible. The co-arriver survives.
///
/// MUST FAIL on origin/main: without the pre-entry snapshot the devourer's
/// eligible pool would include itself (a creature it controls).
#[test]
fn devour_cannot_eat_simultaneous_co_arrival() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // A creature already on the battlefield — the only legal devour victim.
    let preexisting = make_creature(&mut state, 10, "Pre-Existing Bear", Zone::Battlefield);

    // The devourer and an ordinary creature both wait in Exile and co-enter via
    // one ChangeZoneAll. The pre-entry snapshot is captured before EITHER enters
    // (= {preexisting}), so the co-arriver is excluded regardless of the
    // (unordered) entry order.
    let devourer = make_devourer_in_exile(&mut state, 20, "Devourer", 1);
    let co_arriver = make_creature(&mut state, 30, "Co-Arriver", Zone::Exile);

    let ability = ResolvedAbility::new(
        Effect::ChangeZoneAll {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            random_order: false,
        },
        vec![],
        ObjectId(100),
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve_all(&mut state, &ability, &mut events).unwrap();

    // The devourer's as-enters sacrifice must surface its eligible-pool prompt.
    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "expected Devour sacrifice EffectZoneChoice, got {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&preexisting),
        "pre-existing battlefield creature must be a legal devour victim; pool={cards:?}"
    );
    assert!(
        !cards.contains(&devourer),
        "CR 614.13a: a devourer cannot sacrifice itself; pool={cards:?}"
    );
    assert!(
        !cards.contains(&co_arriver),
        "CR 614.13a: a co-entering creature cannot be devoured; pool={cards:?}"
    );

    // Decline the sacrifice (min 0) — the co-arriver must still enter and survive.
    apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] })
        .expect("decline devour sacrifice");

    assert_eq!(
        state.objects[&co_arriver].zone,
        Zone::Battlefield,
        "the co-arriver must finish entering the battlefield"
    );
    assert_eq!(
        state.objects[&devourer].zone,
        Zone::Battlefield,
        "the devourer entered the battlefield"
    );
}

/// CR 614.12a + CR 614.13a (the strict case): two Devour creatures co-entering
/// via one `ChangeZoneAll` cannot devour each other — both as-enters choices are
/// made before either is considered "entered". After devourer A eats one
/// pre-existing creature, devourer B's prompt must appear (park/resume works,
/// no clobber/deadlock) and B's pool must EXCLUDE devourer A entirely (not just
/// the creature A ate) and exclude B itself.
///
/// MUST FAIL on origin/main: with no persisted pre-entry snapshot, B's pool
/// would include devourer A. This is specifically a persist-snapshot test — a
/// single-clear design (clearing the snapshot in `sacrifice.rs`) would let B
/// re-capture a fresh pool that includes A, and this test would fail.
#[test]
fn two_devourers_cannot_eat_each_other() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // Two pre-existing creatures: A eats one, leaving the other as B's only
    // legal victim — so B's prompt is non-empty and its pool is inspectable.
    let pre_x = make_creature(&mut state, 11, "Pre-X", Zone::Battlefield);
    let pre_y = make_creature(&mut state, 12, "Pre-Y", Zone::Battlefield);

    let devourer_a = make_devourer_in_exile(&mut state, 21, "Devourer A", 1);
    let devourer_b = make_devourer_in_exile(&mut state, 22, "Devourer B", 1);

    let ability = ResolvedAbility::new(
        Effect::ChangeZoneAll {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            enters_under: None,
            enter_tapped: engine::types::zones::EtbTapState::Unspecified,
            enter_with_counters: vec![],
            face_down_profile: None,
            library_position: None,
            random_order: false,
        },
        vec![],
        ObjectId(100),
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve_all(&mut state, &ability, &mut events).unwrap();

    // Devourer A's prompt appears first.
    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "expected devourer A's sacrifice EffectZoneChoice, got {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&pre_x) && cards.contains(&pre_y),
        "A's pool must include both pre-existing creatures; pool={cards:?}"
    );
    assert!(
        !cards.contains(&devourer_a) && !cards.contains(&devourer_b),
        "CR 614.13a: A cannot devour itself or co-entering B; pool={cards:?}"
    );

    // A eats Pre-X.
    apply_as_current(&mut state, GameAction::SelectCards { cards: vec![pre_x] })
        .expect("A devours Pre-X");

    assert_eq!(
        state.objects[&pre_x].zone,
        Zone::Graveyard,
        "Pre-X was sacrificed by devourer A"
    );

    // Devourer B's prompt must now appear (resume works, no clobber/deadlock).
    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "expected devourer B's sacrifice EffectZoneChoice after A resolves, got {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&pre_y),
        "B's pool must still include the surviving pre-existing creature; pool={cards:?}"
    );
    assert!(
        !cards.contains(&devourer_a),
        "CR 614.12a/614.13a: devourer B cannot devour co-entering devourer A; pool={cards:?}"
    );
    assert!(
        !cards.contains(&devourer_b),
        "CR 614.13a: devourer B cannot devour itself; pool={cards:?}"
    );
    assert!(
        !cards.contains(&pre_x),
        "Pre-X already left the battlefield (eaten by A); pool={cards:?}"
    );

    // B declines; both devourers survive on the battlefield.
    apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] })
        .expect("B declines its sacrifice");

    assert_eq!(state.objects[&devourer_a].zone, Zone::Battlefield);
    assert_eq!(state.objects[&devourer_b].zone, Zone::Battlefield);
}

/// CR 614.13a (snapshot lifetime): the pre-entry Devour snapshot must not leak
/// past the entry event that captured it. A *single-target* `Effect::ChangeZone`
/// of ONE devourer flows through the single-pick branch in `change_zone::resolve`
/// (not the mass loop), pausing on its as-enters Devour sacrifice and resuming
/// via the deferred counter-pause `EffectResolved`. If the snapshot isn't cleared
/// on that resume, it stays `Some` and wrongly shrinks the eligible pool of the
/// NEXT, unrelated `Effect::Sacrifice` in the same resolution chain.
///
/// MUST FAIL without the single-pick resume clear: the later sacrifice's pool
/// collapses to the stale snapshot `{preexisting}` (count 1 ≤ pool 1 → the
/// mandatory-all fast-path silently sacrifices the pre-existing creature with no
/// prompt, excluding the devourer and the freshly-created creature entirely).
/// PASSES with the clear: the snapshot is `None`, so the later sacrifice sees the
/// FULL controller pool and surfaces a prompt over all of it.
#[test]
fn single_pick_devour_does_not_leak_snapshot_to_later_sacrifice() {
    let mut state = GameState::new_two_player(42);
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);

    // One pre-existing creature on the battlefield — the devourer's only legal
    // victim and (if the snapshot leaks) the only survivor of the later pool.
    let preexisting = make_creature(&mut state, 10, "Pre-Existing Bear", Zone::Battlefield);

    // A single devourer waiting in Exile; nothing else is in Exile, so the
    // untargeted `ChangeZone { Exile -> Battlefield, creature you control }`
    // scan resolves to exactly ONE eligible object and takes the single-pick
    // (single-eligible) branch in `change_zone::resolve`.
    let devourer = make_devourer_in_exile(&mut state, 20, "Devourer", 1);

    let change_zone = ResolvedAbility::new(
        Effect::ChangeZone {
            origin: Some(Zone::Exile),
            destination: Zone::Battlefield,
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            owner_library: false,
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
        vec![],
        ObjectId(100),
        PlayerId(0),
    );

    let mut events = Vec::new();
    resolve(&mut state, &change_zone, &mut events).unwrap();

    // The single-pick entry paused on the devourer's as-enters sacrifice prompt;
    // its pool is the snapshot-constrained pre-entry pool {preexisting}.
    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "expected the devourer's single-pick as-enters sacrifice EffectZoneChoice, got {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&preexisting) && !cards.contains(&devourer),
        "devourer's own pool = pre-entry snapshot (excludes itself); pool={cards:?}"
    );

    // Decline the as-enters sacrifice (min 0). The deferred counter-pause
    // `EffectResolved` drains on this resume — and, with the fix, the snapshot
    // clear drains alongside it.
    apply_as_current(&mut state, GameAction::SelectCards { cards: vec![] })
        .expect("decline the devour sacrifice");
    assert_eq!(
        state.objects[&devourer].zone,
        Zone::Battlefield,
        "the devourer finished entering the battlefield"
    );

    // Now create a fresh creature and run an UNRELATED sacrifice in a fresh
    // resolution. With the snapshot cleared, the eligible pool is the controller's
    // FULL creature set; if it leaked, the pool would be the stale {preexisting}.
    let fresh = make_creature(&mut state, 30, "Fresh Beast", Zone::Battlefield);

    let unrelated_sacrifice = ResolvedAbility::new(
        Effect::Sacrifice {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            count: QuantityExpr::Fixed { value: 1 },
            min_count: 1,
        },
        vec![],
        ObjectId(101),
        PlayerId(0),
    );

    let mut events2 = Vec::new();
    sacrifice::resolve(&mut state, &unrelated_sacrifice, &mut events2).unwrap();

    // count 1 < 3 eligible → the sacrifice surfaces a choice over the FULL pool.
    let WaitingFor::EffectZoneChoice { cards, .. } = &state.waiting_for else {
        panic!(
            "snapshot leaked: the unrelated sacrifice auto-resolved over a shrunk \
             pool instead of prompting over the full pool; got {:?}",
            state.waiting_for
        );
    };
    assert!(
        cards.contains(&devourer) && cards.contains(&preexisting) && cards.contains(&fresh),
        "CR 614.13a: a stale Devour snapshot must not constrain a later, unrelated \
         sacrifice — pool must be the FULL controller creature set; pool={cards:?}"
    );
}
