//! Regression for GitHub issue #899 — Spellstutter Sprite's ETB counter
//! ability silently failed because `parse_counter_ast` dropped the trailing
//! ", where X is the number of Faeries you control" defining clause, leaving
//! the `Cmc` bound as `QuantityRef::Variable("X")`. With no defining
//! expression, the literal X collapses to 0 at resolution and every target
//! spell with `mana_value >= 1` is rejected at the CR 608.2b target
//! legality re-check.
//!
//! CR 107.3i (all instances of X on an object share one value at any given
//! time) + CR 202.3 (mana value) + CR 608.2b (as a spell or ability begins
//! to resolve, each of its targets is checked to see whether it's still a
//! legal target).
//!
//! These tests drive the real engine:
//!   - the parser (via `CardDatabase::from_export`) to confirm the produced
//!     `Effect::Counter` filter resolves X to
//!     `QuantityRef::ObjectCount { filter: Faeries you control }`;
//!   - `find_legal_targets` — the exact target-legality path `apply` uses at
//!     target-declaration time — to confirm the dynamic Cmc bound scales
//!     with the controller's Faerie count.
//!
//! The three scenarios (1-Faerie, 2-Faerie, 3-Faerie controllers) exercise
//! the same composed building blocks the parser stitched together:
//! `strip_trailing_where_x` + `apply_where_x_to_filter` +
//! `QuantityRef::ObjectCount`.

use engine::database::card_db::CardDatabase;
use engine::game::targeting::find_legal_targets;
use engine::game::zones::create_object;
use engine::types::ability::{Effect, TargetFilter, TargetRef};
use engine::types::card_type::{CardType, CoreType};
use engine::types::game_state::{CastingVariant, GameState, StackEntry, StackEntryKind};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

use crate::support::shared_card_db as load_db;

/// Extract the `Effect::Counter` target filter from Spellstutter Sprite's
/// parsed card definition. There are two ability slots on Spellstutter Sprite
/// (Flash, Flying are keywords; the counter is on a ChangesZone trigger), so
/// reach into the trigger collection rather than `abilities`.
fn spellstutter_counter_target(db: &CardDatabase) -> TargetFilter {
    let face = db
        .get_face_by_name("Spellstutter Sprite")
        .expect("Spellstutter Sprite should be in the card database");
    face.triggers
        .iter()
        .find_map(|t| match t.execute.as_ref()?.effect.as_ref() {
            Effect::Counter { target, .. } => Some(target.clone()),
            _ => None,
        })
        .expect("Spellstutter Sprite should parse a Counter trigger")
}

/// Add a Faerie creature on the given player's battlefield. The object
/// carries the Faerie subtype on its card-type struct so `ObjectCount`
/// finds it via the typed-filter `Subtype("Faerie")` predicate.
fn add_faerie(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(
        state,
        card_id,
        controller,
        name.to_string(),
        Zone::Battlefield,
    );
    let faerie = CardType {
        core_types: vec![CoreType::Creature],
        subtypes: vec!["Faerie".to_string()],
        ..Default::default()
    };
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types = faerie.clone();
    obj.base_card_types = faerie;
    obj.power = Some(1);
    obj.toughness = Some(1);
    obj.base_power = Some(1);
    obj.base_toughness = Some(1);
    id
}

/// Push an instant spell of the given mana value onto the stack.
fn push_instant_on_stack(
    state: &mut GameState,
    controller: PlayerId,
    name: &str,
    mana_value: u32,
) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let id = create_object(state, card_id, controller, name.to_string(), Zone::Stack);
    let instant = CardType {
        core_types: vec![CoreType::Instant],
        ..Default::default()
    };
    {
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types = instant.clone();
        obj.base_card_types = instant;
        obj.mana_cost = ManaCost::generic(mana_value);
    }
    state.stack.push_back(StackEntry {
        id,
        source_id: id,
        controller,
        kind: StackEntryKind::Spell {
            card_id,
            ability: None,
            casting_variant: CastingVariant::Normal,
            actual_mana_spent: 0,
        },
    });
    id
}

/// The parser fix is the foundation: the filter must carry a dynamic
/// ObjectCount bound, NOT the bare `Variable("X")` regression value.
#[test]
fn spellstutter_filter_carries_dynamic_faerie_count() {
    use engine::types::ability::{
        Comparator, ControllerRef, FilterProp, QuantityExpr, QuantityRef, TypeFilter,
    };

    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let filter = spellstutter_counter_target(db);

    let typed = match &filter {
        TargetFilter::Typed(tf) => tf,
        TargetFilter::And { filters } => filters
            .iter()
            .find_map(|f| match f {
                TargetFilter::Typed(tf) => Some(tf),
                _ => None,
            })
            .expect("expected a typed leg under the And constraint"),
        other => panic!("expected a typed counter filter, got {other:?}"),
    };
    let cmc = typed
        .properties
        .iter()
        .find_map(|p| match p {
            FilterProp::Cmc { comparator, value } => Some((comparator, value)),
            _ => None,
        })
        .expect("expected a Cmc property on Spellstutter's filter");
    assert_eq!(
        *cmc.0,
        Comparator::LE,
        "Spellstutter's 'or less' clause must parse as a <= mana-value bound"
    );
    let cmc = cmc.1;
    let QuantityExpr::Ref { qty } = cmc else {
        panic!("expected a dynamic Ref bound, got {cmc:?}");
    };
    let QuantityRef::ObjectCount {
        filter: count_filter,
    } = qty
    else {
        panic!(
            "regression: the where-X clause was dropped — the Cmc bound is \
             {qty:?} instead of ObjectCount(Faeries you control)"
        );
    };
    let count_typed = match count_filter {
        TargetFilter::Typed(tf) => tf,
        other => panic!("ObjectCount filter must be Typed, got {other:?}"),
    };
    assert!(
        count_typed
            .type_filters
            .iter()
            .any(|t| matches!(t, TypeFilter::Subtype(s) if s.eq_ignore_ascii_case("Faerie"))),
        "ObjectCount filter must include the Faerie subtype: {count_typed:?}"
    );
    assert_eq!(
        count_typed.controller,
        Some(ControllerRef::You),
        "ObjectCount filter must be controller-scoped to You: {count_typed:?}"
    );
}

/// Scenario 1 — controller has TWO Faeries on the battlefield. X = 2, so the
/// 2-CMC instant is a legal counter target.
#[test]
fn spellstutter_counters_two_cmc_spell_with_two_faeries() {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let filter = spellstutter_counter_target(db);

    let mut state = GameState::new_two_player(42);
    let _faerie_a = add_faerie(&mut state, P0, "Faerie A");
    let sprite = add_faerie(&mut state, P0, "Spellstutter Sprite");
    let two_cmc = push_instant_on_stack(&mut state, P1, "Counterspell", 2);

    let legal = find_legal_targets(&state, &filter, P0, sprite);
    assert!(
        legal.contains(&TargetRef::Object(two_cmc)),
        "with two Faeries on the battlefield (Sprite + one other), a 2-CMC \
         spell must be a legal target. Got {legal:?}"
    );
}

/// Scenario 2 — Spellstutter Sprite enters with NO other Faeries on the
/// battlefield. X = 1 (Sprite itself), so a 2-CMC instant is NOT a legal
/// target.
#[test]
fn spellstutter_cannot_counter_two_cmc_with_only_sprite() {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let filter = spellstutter_counter_target(db);

    let mut state = GameState::new_two_player(42);
    let sprite = add_faerie(&mut state, P0, "Spellstutter Sprite");
    let two_cmc = push_instant_on_stack(&mut state, P1, "Counterspell", 2);

    let legal = find_legal_targets(&state, &filter, P0, sprite);
    assert!(
        !legal.contains(&TargetRef::Object(two_cmc)),
        "with only Spellstutter Sprite on the battlefield (X = 1), a 2-CMC \
         spell must NOT be a legal target. Got {legal:?}"
    );
}

/// Scenario 3 — controller has THREE Faeries (Sprite + two others). X = 3,
/// so a 3-CMC spell is legal AND a 4-CMC spell is illegal.
#[test]
fn spellstutter_counter_cmc_bound_scales_with_faerie_count() {
    let Some(db) = load_db() else {
        eprintln!("card-data.json missing — skipping");
        return;
    };
    let filter = spellstutter_counter_target(db);

    let mut state = GameState::new_two_player(42);
    let _faerie_a = add_faerie(&mut state, P0, "Faerie A");
    let _faerie_b = add_faerie(&mut state, P0, "Faerie B");
    let sprite = add_faerie(&mut state, P0, "Spellstutter Sprite");
    let three_cmc = push_instant_on_stack(&mut state, P1, "Three-CMC Instant", 3);
    let four_cmc = push_instant_on_stack(&mut state, P1, "Four-CMC Instant", 4);

    let legal = find_legal_targets(&state, &filter, P0, sprite);
    assert!(
        legal.contains(&TargetRef::Object(three_cmc)),
        "with three Faeries (X = 3), a 3-CMC spell must be a legal target. \
         Got {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(four_cmc)),
        "with three Faeries (X = 3), a 4-CMC spell must NOT be a legal \
         target. Got {legal:?}"
    );
}
