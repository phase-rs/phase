//! S1a evaluator fixtures for the `subject: ObjectScope` axis and the pin-aware snapshot read of
//! `AbilityCondition::TargetMatchesFilter`.
//!
//! These are evaluator unit fixtures on hand-built conditions: no printed card prints the shape
//! "stale id-keyed LKI entry" or "unadmitted scope". The printed cards the evaluator serves
//! (Reject Imperfection, Carnivorous Canopy, Vindictive Triumph, Sound the Trumpets, Prismari
//! Apprentice, Herald of Ilharg) are driven through the production cast path by the sibling
//! witness files. Each fixture states the two operands it distinguishes.

use engine::game::effects::resolve_ability_chain;
use engine::game::zones::{create_object, move_to_zone};
use engine::types::ability::{
    AbilityCondition, Comparator, Effect, FilterProp, ObjectScope, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn object(state: &mut GameState, id: u64, zone: Zone, core: CoreType, mana_value: u32) -> ObjectId {
    let object_id = create_object(state, CardId(id), P1, format!("Fixture {id}"), zone);
    let obj = state.objects.get_mut(&object_id).expect("object exists");
    obj.card_types.core_types.push(core);
    obj.base_card_types = obj.card_types.clone();
    obj.mana_cost = ManaCost::generic(mana_value);
    object_id
}

/// CR 202.3: "mana value N or less".
fn mana_value_at_most(n: i32) -> TargetFilter {
    TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
        comparator: Comparator::LE,
        value: QuantityExpr::Fixed { value: n },
    }]))
}

fn condition(n: i32, use_lki: bool, subject: ObjectScope) -> AbilityCondition {
    AbilityCondition::TargetMatchesFilter {
        filter: mana_value_at_most(n),
        use_lki,
        subject_slot: None,
        subject,
    }
}

/// A parent instruction declaring `target` (announced: pinned to its current incarnation) with a
/// gated "gain 1 life" sub-instruction. Returns the controller's life delta after resolution.
fn gated_life_gain(
    state: &mut GameState,
    source: ObjectId,
    target: ObjectId,
    c: AbilityCondition,
) -> i32 {
    let mut parent = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 0 },
            player: TargetFilter::Controller,
        },
        vec![TargetRef::Object(target)],
        source,
        P0,
    );
    parent.announce_target_pins(state);
    let sub = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        Vec::new(),
        source,
        P0,
    )
    .condition(c);
    let ability = parent.sub_ability(sub);
    let before = state.players[0].life;
    let mut events = Vec::new();
    resolve_ability_chain(state, &ability, &mut events, 0).expect("chain resolves");
    state.players[0].life - before
}

fn stale_cache_entry(state: &mut GameState, id: ObjectId, mana_value: u32) {
    // An id-keyed LKI entry for an EARLIER object at the same storage id: exactly the
    // value the pre-pin chain read, whichever object currently holds the id.
    let mut stale = state
        .objects
        .get(&id)
        .expect("object exists")
        .snapshot_public_characteristics();
    stale.mana_value = mana_value;
    state.lki_cache.insert(id, stale);
}

/// CR 608.2h + CR 400.7: a departed announced target reads the exit snapshot of ITS incarnation.
/// Operands: snapshot mana value 2, stale id-keyed entry 5, live graveyard object 2 (printed
/// cost kept) — the stale entry is what distinguishes the selections.
#[test]
fn departed_pin_reads_its_own_exit_snapshot_not_the_stale_id_keyed_entry() {
    let mut state = GameState::new_two_player(7);
    let source = object(&mut state, 1, Zone::Battlefield, CoreType::Sorcery, 0);
    let target = object(&mut state, 2, Zone::Battlefield, CoreType::Creature, 2);
    // Announce, then depart: the incarnation of the pin is gone.
    let mut parent = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 0 },
            player: TargetFilter::Controller,
        },
        vec![TargetRef::Object(target)],
        source,
        P0,
    );
    parent.announce_target_pins(&state);
    let mut events = Vec::new();
    move_to_zone(&mut state, target, Zone::Graveyard, &mut events);
    assert_ne!(
        state.objects[&target].incarnation,
        parent.target_pin_at(0).unwrap().incarnation,
        "the target moved: the announced incarnation is gone"
    );
    stale_cache_entry(&mut state, target, 5);
    assert_eq!(state.lki_cache[&target].mana_value, 5, "stale entry staged");
    let exit = state.lki_by_incarnation[&target]
        .values()
        .next()
        .expect("exit snapshot");
    assert_eq!(
        exit.mana_value, 2,
        "exit snapshot of the departed incarnation"
    );

    let sub = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        Vec::new(),
        source,
        P0,
    )
    .condition(condition(3, true, ObjectScope::Target));
    let ability = parent.sub_ability(sub);
    let before = state.players[0].life;
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("chain resolves");
    assert_eq!(
        state.players[0].life - before,
        1,
        "MV 2 <= 3 by the exit snapshot; the stale MV 5 entry must not answer"
    );
}

/// CR 608.2h: a still-current announced target is read live, whatever the id-keyed cache holds.
/// Operands: live mana value 2, stale id-keyed entry 5.
#[test]
fn current_pin_reads_live_not_the_stale_id_keyed_entry() {
    let mut state = GameState::new_two_player(7);
    let source = object(&mut state, 1, Zone::Battlefield, CoreType::Sorcery, 0);
    let target = object(&mut state, 2, Zone::Battlefield, CoreType::Creature, 2);
    stale_cache_entry(&mut state, target, 5);
    assert_eq!(
        gated_life_gain(
            &mut state,
            source,
            target,
            condition(3, true, ObjectScope::Target)
        ),
        1,
        "live MV 2 <= 3; the stale MV 5 entry must not answer for a target that never left"
    );
}

/// CR 400.7: the snapshot is selected by exact incarnation. Operands: the pinned incarnation's
/// exit snapshot holds mana value 5; another incarnation's record at the same id holds 2.
#[test]
fn snapshot_is_selected_by_the_pinned_incarnation_not_by_id() {
    let mut state = GameState::new_two_player(7);
    let source = object(&mut state, 1, Zone::Battlefield, CoreType::Sorcery, 0);
    let target = object(&mut state, 2, Zone::Battlefield, CoreType::Creature, 5);
    let mut parent = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 0 },
            player: TargetFilter::Controller,
        },
        vec![TargetRef::Object(target)],
        source,
        P0,
    );
    parent.announce_target_pins(&state);
    let pinned = parent.target_pin_at(0).unwrap().incarnation;
    let mut events = Vec::new();
    move_to_zone(&mut state, target, Zone::Graveyard, &mut events);
    // A record for a DIFFERENT incarnation at the same id, with a different mana value.
    let mut other = state.lki_by_incarnation[&target][&pinned].clone();
    other.mana_value = 2;
    state
        .lki_by_incarnation
        .entry(target)
        .or_default()
        .insert(pinned + 100, other);
    let sub = ResolvedAbility::new(
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
        Vec::new(),
        source,
        P0,
    )
    .condition(condition(3, true, ObjectScope::Target));
    let ability = parent.sub_ability(sub);
    let before = state.players[0].life;
    resolve_ability_chain(&mut state, &ability, &mut events, 0).expect("chain resolves");
    assert_eq!(
        state.players[0].life - before,
        0,
        "the pinned incarnation held MV 5 > 3; the other incarnation's MV 2 must not answer"
    );
}

/// CR 603.2: `EventSource` reads the object the trigger event names even when the node declares
/// a different target. Operands: event spell mana value 6, declared target mana value 1.
#[test]
fn event_source_reads_the_event_object_not_the_declared_target() {
    for (scope, expected) in [(ObjectScope::EventSource, 0), (ObjectScope::Target, 1)] {
        let mut state = GameState::new_two_player(7);
        let source = object(&mut state, 1, Zone::Battlefield, CoreType::Creature, 0);
        let spell = object(&mut state, 2, Zone::Stack, CoreType::Instant, 6);
        let target = object(&mut state, 3, Zone::Battlefield, CoreType::Land, 1);
        state.current_trigger_event = Some(GameEvent::SpellCast {
            card_id: CardId(2),
            controller: P0,
            object_id: spell,
            cast_mana_value: Some(6),
        });
        assert_eq!(
            gated_life_gain(&mut state, source, target, condition(3, false, scope)),
            expected,
            "{scope:?}: event spell MV 6 > 3 fails the gate; declared target MV 1 passes it"
        );
    }
}

/// CR 608.2h: a departed event spell (countered before the trigger resolves) reads the last
/// known information of its stack exit. Operands: stack-exit snapshot 6, graveyard card 0
/// (the card's printed cost is erased in the test by zeroing it after the move).
#[test]
fn departed_event_spell_reads_its_stack_exit_snapshot() {
    let mut state = GameState::new_two_player(7);
    let source = object(&mut state, 1, Zone::Battlefield, CoreType::Creature, 0);
    let spell = object(&mut state, 2, Zone::Stack, CoreType::Creature, 6);
    let target = object(&mut state, 3, Zone::Battlefield, CoreType::Land, 1);
    state.current_trigger_event = Some(GameEvent::SpellCast {
        card_id: CardId(2),
        controller: P0,
        object_id: spell,
        cast_mana_value: Some(6),
    });
    let mut events = Vec::new();
    move_to_zone(&mut state, spell, Zone::Graveyard, &mut events);
    state.objects.get_mut(&spell).unwrap().mana_cost = ManaCost::generic(0);
    assert_eq!(
        state.objects[&spell].mana_cost,
        ManaCost::generic(0),
        "live graveyard operand 0"
    );
    let c = AbilityCondition::TargetMatchesFilter {
        filter: TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Cmc {
            comparator: Comparator::GE,
            value: QuantityExpr::Fixed { value: 5 },
        }])),
        use_lki: false,
        subject_slot: None,
        subject: ObjectScope::EventSource,
    };
    assert_eq!(
        gated_life_gain(&mut state, source, target, c),
        1,
        "stack-exit snapshot MV 6 >= 5; the live graveyard read (0) must not answer"
    );
}

/// The evaluator admits `Target` and `EventSource` only; any other scope has no referent. The
/// `Target` row is the reach guard: the same fixture and gate DO fire for an admitted scope.
#[test]
fn unadmitted_scope_fails_closed() {
    for (scope, expected) in [
        (ObjectScope::Target, 1),
        (ObjectScope::Source, 0),
        (ObjectScope::CostPaidObject, 0),
        (ObjectScope::Recipient, 0),
    ] {
        let mut state = GameState::new_two_player(7);
        let source = object(&mut state, 1, Zone::Battlefield, CoreType::Creature, 0);
        let target = object(&mut state, 2, Zone::Battlefield, CoreType::Creature, 1);
        assert_eq!(
            gated_life_gain(&mut state, source, target, condition(3, false, scope)),
            expected,
            "{scope:?}: only Target and EventSource name a referent for TargetMatchesFilter"
        );
    }
}

/// Serde: the default scope is omitted (every existing card-data entry round-trips unchanged);
/// `EventSource` is kept; an old-shape payload without `subject` reads as `Target`.
#[test]
fn subject_serde_round_trip() {
    let default = condition(3, true, ObjectScope::Target);
    let json = serde_json::to_value(&default).unwrap();
    assert!(
        json.get("subject").is_none(),
        "default scope is skipped: {json}"
    );
    let event = condition(3, false, ObjectScope::EventSource);
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["subject"]["type"], "EventSource");
    assert_eq!(
        serde_json::from_value::<AbilityCondition>(json).unwrap(),
        event
    );
    let old_shape = serde_json::json!({
        "type": "TargetMatchesFilter",
        "filter": serde_json::to_value(mana_value_at_most(3)).unwrap(),
        "use_lki": true,
    });
    assert_eq!(
        serde_json::from_value::<AbilityCondition>(old_shape).unwrap(),
        default,
        "no `subject` key reads as Target"
    );
}
