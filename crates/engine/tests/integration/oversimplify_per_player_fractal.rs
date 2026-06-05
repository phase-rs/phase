//! Oversimplify (SOC/C21/DSC/PRM) — per-player Fractal token counter count.
//!
//! Oracle text:
//!   "Exile all creatures. Each player creates a 0/0 green and blue Fractal
//!    creature token and puts a number of +1/+1 counters on it equal to the
//!    total power of creatures they controlled that were exiled this way."
//!
//! Pre-fix bug: the dynamic-quantity clause was silently swallowed at parse
//! time (`SwallowedClause` / `DynamicQty`), so the Fractal token entered with
//! `enter_with_counters: []` and was a 0/0 — instantly dead to SBAs. Four
//! repairs land the card end-to-end:
//!
//!   1. Parser — `try_parse_put_counters_on_token_followup` now accepts the
//!      third-person `puts ` verb AND the `"a number of <type> counters on it
//!      equal to <quantity>"` body (delegating to the shared
//!      `parse_dynamic_counter_suffix_body`).
//!   2. Parser — `parse_type_phrase_with_ctx` lowers
//!      `"creatures they controlled that were exiled this way"` into a
//!      composite `And{Typed{Creature,ScopedPlayer}, ExiledBySource}` filter.
//!   3. Engine — `QuantityRef::Aggregate{Power|Toughness}` falls back to the
//!      LKI snapshot when the matched object is off battlefield, per CR 608.2h
//!      + CR 400.7. Without this, the aggregate returned 0 because
//!        `obj.power` is `None` for non-battlefield objects.
//!   4. Engine — `Typed{controller: ...}` filter evaluation falls back to the
//!      LKI controller for non-battlefield objects, so "they controlled"
//!      semantics work even for stolen creatures (whose post-exile controller
//!      resets to owner).
//!
//! This test drives the real resolver (`resolve_ability_chain`) end-to-end:
//! exile all creatures, then verify each player's Fractal token enters with
//! the correct counter count equal to that player's total exiled power.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::GameState;
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn add_creature(
    state: &mut GameState,
    card_id: u64,
    owner: PlayerId,
    name: &str,
    power: i32,
    toughness: i32,
) -> engine::types::identifiers::ObjectId {
    let id = create_object(
        state,
        CardId(card_id),
        owner,
        name.to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.power = Some(power);
    obj.toughness = Some(toughness);
    obj.base_power = Some(power);
    obj.base_toughness = Some(toughness);
    // Ensure the battlefield list reflects the live creature.
    if !state.battlefield.contains(&id) {
        state.battlefield.push_back(id);
    }
    id
}

#[test]
fn oversimplify_per_player_fractal_counters_match_exiled_power() {
    let Some(db) = load_db() else {
        // No card-data export available in this build — skip.
        return;
    };

    let face = db
        .get_face_by_name("Oversimplify")
        .expect("Oversimplify should be in the card database");
    let definition = face
        .abilities
        .first()
        .expect("Oversimplify should parse a spell ability");

    // ----- Parser-level assertions (the four-task class repair). -----
    use engine::types::ability::{
        AggregateFunction, ControllerRef, Effect, ObjectProperty, QuantityExpr, QuantityRef,
        TargetFilter, TypedFilter,
    };
    let Effect::ChangeZoneAll { .. } = definition.effect.as_ref() else {
        panic!(
            "Oversimplify must start with ChangeZoneAll (exile all creatures), got {:?}",
            definition.effect
        );
    };
    let token_def = definition
        .sub_ability
        .as_deref()
        .expect("Oversimplify chains the per-player Token sub-ability");
    let Effect::Token {
        enter_with_counters,
        ..
    } = token_def.effect.as_ref()
    else {
        panic!(
            "Oversimplify's sub-ability must be a Token effect, got {:?}",
            token_def.effect
        );
    };
    assert_eq!(
        enter_with_counters.len(),
        1,
        "the Fractal token must carry exactly one enter_with_counters entry — \
         pre-fix this list was empty (DynamicQty SwallowedClause)"
    );
    let (counter_type, count_expr) = &enter_with_counters[0];
    assert_eq!(*counter_type, CounterType::Plus1Plus1);
    let expected_count = QuantityExpr::Ref {
        qty: QuantityRef::Aggregate {
            function: AggregateFunction::Sum,
            property: ObjectProperty::Power,
            filter: TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(
                        TypedFilter::creature().controller(ControllerRef::ScopedPlayer),
                    ),
                    TargetFilter::ExiledBySource,
                ],
            },
        },
    };
    assert_eq!(
        count_expr, &expected_count,
        "the counter quantity must be the composite Aggregate{{Sum, Power, \
         And[Typed{{Creature, ScopedPlayer}}, ExiledBySource]}}"
    );

    // ----- Runtime end-to-end: cast and resolve. -----
    let mut state = GameState::new_two_player(7);
    let source_id = create_object(
        &mut state,
        CardId(1000),
        PlayerId(0),
        "Oversimplify".to_string(),
        Zone::Stack,
    );
    state
        .objects
        .get_mut(&source_id)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Sorcery];

    // P0 controls a 4/4 and a 1/1 — total power 5.
    let p0_big = add_creature(&mut state, 101, PlayerId(0), "P0 Big", 4, 4);
    let p0_small = add_creature(&mut state, 102, PlayerId(0), "P0 Small", 1, 1);
    // P1 controls a single 3/3 — total power 3.
    let p1_mid = add_creature(&mut state, 103, PlayerId(1), "P1 Mid", 3, 3);
    // CR 109.4 + CR 608.2h: A *stolen* creature — owned by P0 but currently
    // controlled by P1 (Threaten-style). This exercises Task 4's LKI
    // controller-fallback end-to-end: post-exile, `obj.controller` would
    // reset toward owner (P0), but the look-back filter's "they controlled"
    // scoping must read the at-exile controller (P1) from the LKI cache.
    // Without the fallback, this 2/2 would count toward P0 (yielding P0:7,
    // P1:3) instead of P1 (correct: P0:5, P1:5).
    let stolen = add_creature(&mut state, 104, PlayerId(0), "Stolen 2/2", 2, 2);
    state.objects.get_mut(&stolen).unwrap().controller = PlayerId(1);

    let ability = build_resolved_from_def(definition, source_id, PlayerId(0));
    let mut events = Vec::<GameEvent>::new();
    resolve_ability_chain(&mut state, &ability, &mut events, 0)
        .expect("Oversimplify must resolve without error");

    // Sanity: every battlefield creature left the battlefield (exiled).
    for victim in [p0_big, p0_small, p1_mid, stolen] {
        assert!(
            !state.battlefield.contains(&victim),
            "creature {victim:?} must be exiled off the battlefield"
        );
    }

    // Find each player's Fractal token. The per-player iteration creates one
    // token per player; the iterating player is the token's controller.
    let p0_token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|obj| obj.is_token && obj.controller == PlayerId(0) && obj.name == "Fractal")
        .expect("P0 must have a Fractal token on the battlefield");
    let p1_token = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .find(|obj| obj.is_token && obj.controller == PlayerId(1) && obj.name == "Fractal")
        .expect("P1 must have a Fractal token on the battlefield");

    // P0 controlled 4+1 = 5 power. P1 controlled 3 (P1 Mid) + 2 (the stolen
    // P0-owned 2/2) = 5 power — the stolen creature counts toward P1, NOT
    // P0, because "they controlled" reads the at-exile controller via LKI
    // (Task 4). If the LKI controller fallback regresses, this becomes
    // P0:7 / P1:3 and the asserts fail. Each token must also survive SBAs
    // (a 0/0 with N counters is N/N).
    let p0_counters = p0_token
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    let p1_counters = p1_token
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        p0_counters, 5,
        "P0's Fractal must enter with 5 +1/+1 counters (4+1 power exiled — \
         the stolen 2/2 must NOT count for P0 since P1 controlled it at exile), \
         got {p0_counters}"
    );
    assert_eq!(
        p1_counters, 5,
        "P1's Fractal must enter with 5 +1/+1 counters (3 own + 2 stolen \
         — the stolen P0-owned creature counts toward P1 because P1 was its \
         controller at exile per CR 109.4 LKI), got {p1_counters}"
    );
}
