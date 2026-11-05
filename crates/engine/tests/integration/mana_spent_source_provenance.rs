//! Cast-history mana-source provenance — `FilterProp::ManaSpentFromSource`.
//!
//! CR 106.3 + CR 601.2h + CR 400.7d: "mana from a Treasure was spent to cast"
//! is a payment-time fact. The engine already captures one
//! `ManaSpentSourceSnapshot` per spent mana unit on the cast object
//! (`casting.rs`, CR 601.2h), and `spell_cast_record_for` now copies them into
//! `SpellCastRecord::mana_spent_source_snapshots` at CR 601.2i so the fact
//! survives resolution and the source's departure (CR 400.7).
//!
//! These tests drive the real cast pipeline (`GameScenario` + `runner.cast(..)`)
//! and read the fact through every reader the property has:
//!
//! - R2 `record_answers_source_qualified_payment` — the spell-history record reader
//!   (`spell_record_matches_filter`), the reader a turn-history gate counts with.
//! - R3 `live_stack_spell_answers_source_qualified_payment` — the live object reader
//!   (`matches_target_filter`) on the spell while it is on the stack.
//! - R3b `live_projected_record_answers_source_qualified_payment` — the
//!   live-projected record reader (`spell_object_matches_filter_from_state`), the
//!   route a battlefield static's cost-modifier and cast-keyword-grant filters take.
//! - R4 `spells_cast_this_turn_counts_source_qualified_cast` — the `SpellsCastThisTurn`
//!   gate reader a first-qualified-spell condition counts with.
//! - R5 `two_units_from_one_source_and_mixed_sources_still_answer_true` — existential
//!   matching, plus the preservation row that the `FromSource` quantity metric still
//!   COUNTS the matching units.
//! - R6 `departed_source_still_matches_from_snapshot` — a source that has left answers
//!   from its latched snapshot, never from a live re-read (CR 400.7).
//! - R7 `record_without_payment_provenance_answers_false` — an unprovable record fails
//!   closed instead of answering a vacuous true.
//!
//! Fixture authority: the cast subject is a vanilla **Grizzly Bears** (CR 601.2a: a
//! sorcery-speed permanent spell, cast and resolved sequentially), the matching mana
//! source is the printed **Treasure** artifact token that Rain of Riches itself
//! creates ("It's an artifact with '{T}, Sacrifice this token: Add one mana of any
//! color.'"), and the non-matching source is a **Forest**. Both legs pay
//! `{2}` with two one-mana units so each leg's guard is measurable in both
//! directions ("exactly two snapshots, every one naming this leg's source and none
//! naming the other").

use engine::game::filter::{
    matches_target_filter, spell_object_matches_filter_from_state, spell_record_matches_filter,
    FilterContext,
};
use engine::game::quantity::resolve_quantity;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::zones::create_object;
use engine::types::ability::{
    CastManaObjectScope, CastManaSpentMetric, CountScope, FilterProp, QuantityExpr, QuantityRef,
    TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{SpellCastRecord, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::GameState;

/// The cast subject: a vanilla 2/2 with no Oracle text.
const BEARS_NAME: &str = "Grizzly Bears";

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

/// A battlefield Treasure artifact TOKEN's characteristics, as a real object so the
/// payment block snapshots it per spent unit. (The engine's payment path records
/// the producing source of each spent mana unit; a unit whose source is a real
/// object stamps one `ManaSpentSourceSnapshot`.)
fn treasure_source(runner: &mut GameRunner, card_id: u64) -> ObjectId {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Treasure".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Artifact);
    obj.card_types.subtypes.push("Treasure".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// The non-matching control source: a plain Forest.
fn forest_source(runner: &mut GameRunner, card_id: u64) -> ObjectId {
    let state = runner.state_mut();
    let id = create_object(
        state,
        CardId(card_id),
        P0,
        "Forest".to_string(),
        Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Land);
    obj.card_types.subtypes.push("Forest".to_string());
    obj.base_card_types = obj.card_types.clone();
    id
}

/// Pool mana whose `ManaUnit::source` names a REAL object, so the payment block
/// stamps a `ManaSpentSourceSnapshot` for each unit (CR 601.2h). `add_mana`, which
/// uses a dummy source id, stamps nothing — which is why every leg uses this.
fn add_mana_from_source(
    runner: &mut GameRunner,
    player: PlayerId,
    mana_type: ManaType,
    amount: u32,
    source: ObjectId,
) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(mana_type, source, false, vec![]));
    }
}

/// The filter Phase 3 lowers "mana from a Treasure" to: a `Typed` filter naming the
/// Treasure subtype, no controller, no further properties.
fn treasure_source_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter {
        type_filters: vec![TypeFilter::Subtype("Treasure".to_string())],
        controller: None,
        properties: vec![],
    })
}

/// The property under test inside the outer spell filter the readers take.
fn mana_from_treasure_filter() -> TargetFilter {
    TargetFilter::Typed(
        TypedFilter::card().properties(vec![FilterProp::ManaSpentFromSource {
            source_filter: Box::new(treasure_source_filter()),
        }]),
    )
}

/// Drain priority (answering single-slot trigger-ordering prompts with the identity
/// order) until `done` holds. Copies the established idiom from
/// `emptiness_mana_spent_evoke_5943`; no other waiting state is expected here.
fn pass_until(runner: &mut GameRunner, mut done: impl FnMut(&GameRunner) -> bool, what: &str) {
    for _ in 0..20 {
        if done(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority must be accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("identity OrderTriggers must succeed");
            }
            other => panic!("unhandled waiting state while {what}: {other:?}"),
        }
    }
    panic!("drive loop exhausted while {what}");
}

/// The caster's most recent turn-scoped cast-history record (CR 601.2i). Takes the
/// state directly so a test can read it while a `CastCommit` still owns the runner.
fn last_record_in(state: &GameState) -> SpellCastRecord {
    state
        .spells_cast_this_turn_by_player
        .get(&P0)
        .and_then(|records| records.back())
        .cloned()
        .expect("the cast must be recorded in the turn-scoped history")
}

/// [`last_record_in`] over a runner with no live cast commit.
fn last_record(runner: &GameRunner) -> SpellCastRecord {
    last_record_in(runner.state())
}

/// Reach guard shared by every leg: exactly two snapshots, every one naming the
/// expected source and none naming the other, and every one carrying that source's
/// snapshot subtype. Without this, a `false` from the matcher could be the
/// degenerate empty-vector case rather than the intended non-matching payment.
fn assert_leg_snapshots(
    record: &SpellCastRecord,
    expected: ObjectId,
    other: ObjectId,
    subtype: &str,
) {
    assert_eq!(
        record.mana_spent_source_snapshots.len(),
        2,
        "each leg pays {{2}} with two one-mana units, so both units must stamp a source snapshot"
    );
    for snapshot in &record.mana_spent_source_snapshots {
        assert_eq!(
            snapshot.source_id, expected,
            "every snapshot must name this leg's mana source"
        );
        assert_ne!(
            snapshot.source_id, other,
            "no snapshot may name the other source"
        );
        assert!(
            snapshot.lki.subtypes.iter().any(|s| s == subtype),
            "the snapshot must carry the source's captured subtype ({subtype})"
        );
    }
}

/// One `{2}` Grizzly Bears in hand, a fresh scenario at a main phase.
fn bears_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bears = {
        let mut builder = scenario.add_creature_to_hand(P0, BEARS_NAME, 2, 2);
        builder.with_mana_cost(ManaCost::generic(2));
        builder.id()
    };
    (scenario, bears)
}

/// A second copy of the subject, for the leg that must cast twice.
fn add_second_bears(scenario: &mut GameScenario) -> ObjectId {
    let mut builder = scenario.add_creature_to_hand(P0, BEARS_NAME, 2, 2);
    builder.with_mana_cost(ManaCost::generic(2));
    builder.id()
}

// ---------------------------------------------------------------------------
// R2 — the spell-history record reader
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 601.2h + CR 400.7d: the cast-history record answers the property
/// from the payment-time snapshots the cast write projected. A cast paid by two
/// Treasure units answers true; the second copy of the same subject, paid by two
/// Forests, answers false — with a reach guard proving its own snapshots are present
/// and name the Forest, so the `false` is not the empty-vector degenerate case.
///
/// Reverting §5.4's `mana_spent_source_snapshots: obj.mana_spent_source_snapshots.clone()`
/// leaves the record's vector empty and turns the `true` into `false`.
#[test]
fn record_answers_source_qualified_payment() {
    let (mut scenario, treasure_bears) = bears_scenario();
    let forest_bears = add_second_bears(&mut scenario);
    let mut runner = scenario.build();

    let treasure = treasure_source(&mut runner, 9001);
    let forest = forest_source(&mut runner, 9002);
    let filter = mana_from_treasure_filter();
    let empty_state = GameState::default();

    // --- Leg 1: Treasure-funded. ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    runner.cast(treasure_bears).commit();
    let treasure_record = last_record(&runner);
    assert_leg_snapshots(&treasure_record, treasure, forest, "Treasure");
    assert!(
        spell_record_matches_filter(
            &treasure_record,
            &filter,
            P0,
            runner.state(),
            &FilterContext::neutral(),
        ),
        "a Treasure-funded cast must satisfy 'mana from a Treasure was spent to cast'"
    );

    // Resolve the spell so the second copy can be cast at sorcery speed (CR 307.1 +
    // CR 117.1a: no other spell may be on the stack).
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Treasure-funded Grizzly Bears",
    );

    // --- Leg 2: Forest-funded (the paired negative). ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, forest);
    runner.cast(forest_bears).commit();
    let forest_record = last_record(&runner);
    assert_leg_snapshots(&forest_record, forest, treasure, "Forest");
    assert!(
        !spell_record_matches_filter(
            &forest_record,
            &filter,
            P0,
            runner.state(),
            &FilterContext::neutral(),
        ),
        "a cast paid only by Forests must not satisfy the Treasure-sourced property"
    );

    // The unredacted record is state-independent: the same answer holds against a
    // state whose creature-type catalog is empty (the property reads no catalog).
    assert!(!spell_record_matches_filter(
        &forest_record,
        &filter,
        P0,
        &empty_state,
        &FilterContext::neutral(),
    ));
}

// ---------------------------------------------------------------------------
// R3 — the live object reader
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 601.2h: the stamps live on the cast object from payment until it
/// leaves the Stack/Battlefield (CR 400.7), so the live reader answers the same
/// question on the spell while it is on the stack.
///
/// Reverting the `matches_filter_prop` arm would make the live reader fail through
/// to the property's fail-closed group and turn both answers `false`.
#[test]
fn live_stack_spell_answers_source_qualified_payment() {
    let (mut scenario, treasure_bears) = bears_scenario();
    let forest_bears = add_second_bears(&mut scenario);
    let mut runner = scenario.build();

    let treasure = treasure_source(&mut runner, 9001);
    let forest = forest_source(&mut runner, 9002);
    let filter = mana_from_treasure_filter();

    // --- Leg 1: Treasure-funded, asserted on the stack. ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    {
        let commit = runner.cast(treasure_bears).commit();
        assert_eq!(
            commit.state().objects[&treasure_bears]
                .mana_spent_source_snapshots
                .len(),
            2,
            "both Treasure units must snapshot their source on the cast object at payment"
        );
        assert_eq!(
            commit.state().objects[&treasure_bears].zone,
            Zone::Stack,
            "the committed spell is on the stack"
        );
        let ctx = FilterContext::from_source_with_controller(treasure, P0);
        assert!(
            matches_target_filter(commit.state(), treasure_bears, &filter, &ctx),
            "the live stack spell answers the source-qualified payment"
        );
    }
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Treasure-funded Grizzly Bears",
    );

    // --- Leg 2: Forest-funded (the paired negative), same live reader. ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, forest);
    {
        let commit = runner.cast(forest_bears).commit();
        // Reach guard: the fixture is NOT the degenerate empty-snapshot case.
        assert_eq!(
            commit.state().objects[&forest_bears]
                .mana_spent_source_snapshots
                .len(),
            2,
            "both Forest units must snapshot their source on the cast object at payment"
        );
        assert!(
            commit.state().objects[&forest_bears]
                .mana_spent_source_snapshots
                .iter()
                .all(|s| s.source_id == forest),
            "every snapshot must name the Forest"
        );
        let ctx = FilterContext::from_source_with_controller(forest, P0);
        assert!(
            !matches_target_filter(commit.state(), forest_bears, &filter, &ctx),
            "a Forest-funded stack spell must not satisfy the Treasure-sourced property"
        );
    }
}

// ---------------------------------------------------------------------------
// R3b — the live-projected record reader
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 601.2h: `spell_object_matches_filter_from_state` projects the spell
/// object into the record the live cost-modifier and cast-keyword-grant filters read.
/// That projection runs through `restrictions::spell_cast_record_for`, the single
/// record authority, which now carries the payment snapshots — so this seam answers
/// the property too. This is the route a battlefield static's `affected` filter takes,
/// i.e. the route Rain of Riches' granted cascade reaches.
///
/// Reverting §5.4's projection line leaves the projected record's vector empty and
/// turns the `true` into `false`.
#[test]
fn live_projected_record_answers_source_qualified_payment() {
    let (mut scenario, treasure_bears) = bears_scenario();
    let forest_bears = add_second_bears(&mut scenario);
    let mut runner = scenario.build();

    let treasure = treasure_source(&mut runner, 9001);
    let forest = forest_source(&mut runner, 9002);
    let filter = mana_from_treasure_filter();
    // The printed object-quality reach guard: proof the property loop was reached
    // at all, so the `true` below is not a short-circuited pass.
    let named_filter =
        TargetFilter::Typed(TypedFilter::card().properties(vec![FilterProp::Named {
            name: BEARS_NAME.to_string(),
        }]));

    // --- Leg 1: Treasure-funded. ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    {
        let commit = runner.cast(treasure_bears).commit();
        let state = commit.state();
        let spell = &state.objects[&treasure_bears];
        // `source_id` must exist in `state.objects` (the announced-X entry returns
        // early otherwise) and the filter's `controller` is `None`, so no
        // controller gate can dominate the verdict.
        assert!(
            spell_object_matches_filter_from_state(
                state,
                spell,
                Zone::Hand,
                P0,
                &filter,
                treasure,
                &state.all_creature_types,
            ),
            "the live-projected record answers the source-qualified payment"
        );
        assert!(
            spell_object_matches_filter_from_state(
                state,
                spell,
                Zone::Hand,
                P0,
                &named_filter,
                treasure,
                &state.all_creature_types,
            ),
            "reach guard: the same entry point answers a plain record-only property"
        );
    }
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Treasure-funded Grizzly Bears",
    );

    // --- Leg 2: Forest-funded (the paired negative). ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, forest);
    {
        let commit = runner.cast(forest_bears).commit();
        let state = commit.state();
        let spell = &state.objects[&forest_bears];
        assert_leg_snapshots(&last_record_in(state), forest, treasure, "Forest");
        assert!(
            !spell_object_matches_filter_from_state(
                state,
                spell,
                Zone::Hand,
                P0,
                &filter,
                forest,
                &state.all_creature_types,
            ),
            "a Forest-funded cast must not satisfy the Treasure-sourced property"
        );
        assert!(
            spell_object_matches_filter_from_state(
                state,
                spell,
                Zone::Hand,
                P0,
                &named_filter,
                forest,
                &state.all_creature_types,
            ),
            "reach guard: the entry point still answers a plain record-only property"
        );
    }
}

// ---------------------------------------------------------------------------
// R4 — the gate reader
// ---------------------------------------------------------------------------

/// CR 604.1 + CR 611.3a: a first-qualified-spell gate counts qualifying casts, so
/// the count must be the number of casts the property accepts — not zero. Reverting
/// the threading makes the count silently `0`, which is always one less than the
/// ordinal and therefore makes the gate vacuously true on every spell of the turn.
///
/// The unfiltered reach guard is order-dependent by construction: it holds only
/// after the first cast and before the second.
#[test]
fn spells_cast_this_turn_counts_source_qualified_cast() {
    let (mut scenario, treasure_bears) = bears_scenario();
    let forest_bears = add_second_bears(&mut scenario);
    let mut runner = scenario.build();

    let treasure = treasure_source(&mut runner, 9001);
    let forest = forest_source(&mut runner, 9002);
    let filtered = QuantityExpr::Ref {
        qty: QuantityRef::SpellsCastThisTurn {
            scope: CountScope::Controller,
            filter: Some(mana_from_treasure_filter()),
        },
    };
    let unfiltered = QuantityExpr::Ref {
        qty: QuantityRef::SpellsCastThisTurn {
            scope: CountScope::Controller,
            filter: None,
        },
    };

    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    runner.cast(treasure_bears).commit();

    assert_eq!(
        resolve_quantity(runner.state(), &filtered, P0, treasure_bears),
        1,
        "the Treasure-funded cast is counted by the source-qualified gate"
    );
    // Reach guard, asserted here on purpose: after the negative cast the same
    // unfiltered count is 2.
    assert_eq!(
        resolve_quantity(runner.state(), &unfiltered, P0, treasure_bears),
        1,
        "exactly one spell has been cast so far"
    );

    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Treasure-funded Grizzly Bears",
    );

    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, forest);
    runner.cast(forest_bears).commit();
    assert_leg_snapshots(&last_record(&runner), forest, treasure, "Forest");

    assert_eq!(
        resolve_quantity(runner.state(), &filtered, P0, forest_bears),
        1,
        "the Forest-funded cast is not counted — the gate still reads 1, not 2"
    );
    assert_eq!(
        resolve_quantity(runner.state(), &unfiltered, P0, forest_bears),
        2,
        "two spells were cast in total"
    );
}

// ---------------------------------------------------------------------------
// R5 — existential matching, and the quantity metric's preservation
// ---------------------------------------------------------------------------

/// CR 106.3: one source per mana unit and several matching units still answer ONE
/// boolean, so neither two units from one source nor a mixed payment multiply the
/// answer. The preservation half is the sibling fold: the `FromSource` quantity
/// metric, which now shares the extracted per-unit predicate, still COUNTS the
/// matching units (2 for two Treasure units, 1 for a mixed payment).
#[test]
fn two_units_from_one_source_and_mixed_sources_still_answer_true() {
    let (mut scenario, both_treasure_bears) = bears_scenario();
    let mixed_bears = add_second_bears(&mut scenario);
    let mut runner = scenario.build();

    let treasure = treasure_source(&mut runner, 9001);
    let forest = forest_source(&mut runner, 9002);
    let filter = mana_from_treasure_filter();
    let metric = |filter: TargetFilter| QuantityExpr::Ref {
        qty: QuantityRef::ManaSpentToCast {
            scope: CastManaObjectScope::SelfObject,
            // `CastManaSpentMetric::FromSource` carries the filter unboxed, unlike
            // `FilterProp::ManaSpentFromSource` (which boxes it to break the
            // FilterProp -> TargetFilter -> TypedFilter -> FilterProp size cycle).
            metric: CastManaSpentMetric::FromSource {
                source_filter: filter,
            },
        },
    };

    // --- Leg 1: two units from ONE source. ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    runner.cast(both_treasure_bears).commit();
    let record = last_record(&runner);
    assert_leg_snapshots(&record, treasure, forest, "Treasure");
    assert!(
        spell_record_matches_filter(
            &record,
            &filter,
            P0,
            runner.state(),
            &FilterContext::neutral()
        ),
        "two units from one matching source answer true"
    );
    // Preservation (row RR1): the COUNT is unchanged by the leaf extraction.
    assert_eq!(
        resolve_quantity(
            runner.state(),
            &metric(treasure_source_filter()),
            P0,
            both_treasure_bears
        ),
        2,
        "the FromSource metric still counts both matching units"
    );

    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Treasure-funded Grizzly Bears",
    );

    // --- Leg 2: a mixed payment (one Treasure, one Forest). ---
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 1, treasure);
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 1, forest);
    runner.cast(mixed_bears).commit();
    let record = last_record(&runner);
    assert_eq!(
        record.mana_spent_source_snapshots.len(),
        2,
        "the mixed payment stamps both units"
    );
    assert!(
        record
            .mana_spent_source_snapshots
            .iter()
            .any(|s| s.source_id == treasure),
        "the mixed payment's Treasure unit is present"
    );
    assert!(
        record
            .mana_spent_source_snapshots
            .iter()
            .any(|s| s.source_id == forest),
        "the mixed payment's Forest unit is present"
    );
    assert!(
        spell_record_matches_filter(
            &record,
            &filter,
            P0,
            runner.state(),
            &FilterContext::neutral()
        ),
        "one matching unit among the payment is sufficient"
    );
    assert_eq!(
        resolve_quantity(
            runner.state(),
            &metric(treasure_source_filter()),
            P0,
            mixed_bears
        ),
        1,
        "exactly one unit of the mixed payment came from the Treasure"
    );
}

// ---------------------------------------------------------------------------
// R6 — a departed source matches from its snapshot
// ---------------------------------------------------------------------------

/// CR 400.7 + CR 400.7d: once the source has left, the payment is answered from the
/// latched snapshot — the record's snapshots survive, and the cast object's stamps
/// were latched with the cast — never from a live re-read of whatever object used to
/// hold that id.
///
/// Hostile multi-authority row H3: the removed Treasure is the source of BOTH legs'
/// provenance question, so a rescan of live objects would answer false everywhere.
#[test]
fn departed_source_still_matches_from_snapshot() {
    let (mut scenario, treasure_bears) = bears_scenario();
    let forest_bears = add_second_bears(&mut scenario);
    let mut runner = scenario.build();

    let treasure = treasure_source(&mut runner, 9001);
    let forest = forest_source(&mut runner, 9002);
    let filter = mana_from_treasure_filter();

    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    runner.cast(treasure_bears).commit();
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Treasure-funded Grizzly Bears",
    );
    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, forest);
    runner.cast(forest_bears).commit();
    pass_until(
        &mut runner,
        |r| r.state().stack.is_empty(),
        "resolving the Forest-funded Grizzly Bears",
    );

    // Both records exist before the source departs.
    let records = runner
        .state()
        .spells_cast_this_turn_by_player
        .get(&P0)
        .cloned()
        .expect("both casts are recorded");
    assert_eq!(records.len(), 2);
    assert_leg_snapshots(&records[0], treasure, forest, "Treasure");
    assert_leg_snapshots(&records[1], forest, treasure, "Forest");
    let ctx = FilterContext::from_source_with_controller(treasure, P0);
    assert!(
        matches_target_filter(runner.state(), treasure_bears, &filter, &ctx),
        "the Treasure-funded cast object answers the property before the source departs"
    );

    // The Treasure leaves the battlefield — a sacrificed token (CR 400.7).
    {
        let state = runner.state_mut();
        state.objects.remove(&treasure);
        state.lki_cache.remove(&treasure);
    }
    assert!(
        runner.state().objects.get(&treasure).is_none(),
        "the sacrificed source is gone from the object map"
    );

    // The record still answers true from its latched snapshots.
    assert!(
        spell_record_matches_filter(
            &records[0],
            &filter,
            P0,
            runner.state(),
            &FilterContext::neutral()
        ),
        "the Treasure-funded record answers from its snapshot after the source departs"
    );
    // The cast object still answers true: its stamps were latched with the cast.
    assert!(
        matches_target_filter(runner.state(), treasure_bears, &filter, &ctx),
        "the Treasure-funded cast object still answers from its latched stamps"
    );
    // Paired negative: the Forest-funded record answered false before the departure
    // and still does, with its own reach guard.
    assert_leg_snapshots(&records[1], forest, treasure, "Forest");
    assert!(
        !spell_record_matches_filter(
            &records[1],
            &filter,
            P0,
            runner.state(),
            &FilterContext::neutral()
        ),
        "the Forest-funded record still answers false after the Treasure departs"
    );
}

// ---------------------------------------------------------------------------
// R7 — an unprovable record fails closed
// ---------------------------------------------------------------------------

/// CR 106.3 + CR 601.2h: an empty snapshot vector means "cannot prove", never
/// "proved false" — so it answers `false`. A wildcard or `true` default would make
/// every old record satisfy every source-qualified predicate, which is a vacuous
/// true.
#[test]
fn record_without_payment_provenance_answers_false() {
    let (scenario, bears) = bears_scenario();
    let mut runner = scenario.build();

    let treasure = treasure_source(&mut runner, 9001);
    let filter = mana_from_treasure_filter();

    add_mana_from_source(&mut runner, P0, ManaType::Colorless, 2, treasure);
    runner.cast(bears).commit();
    let proved = last_record(&runner);
    assert_eq!(
        proved.mana_spent_source_snapshots.len(),
        2,
        "reach guard: the same filter shape answers true for a record WITH provenance"
    );
    assert!(spell_record_matches_filter(
        &proved,
        &filter,
        P0,
        runner.state(),
        &FilterContext::neutral()
    ));

    // A record with no payment provenance (an old save, or a spell evaluated before
    // payment) cannot prove the payment and answers false.
    let unprovable = SpellCastRecord {
        name: BEARS_NAME.to_string(),
        mana_value: 2,
        ..Default::default()
    };
    assert!(
        unprovable.mana_spent_source_snapshots.is_empty(),
        "the fixture record really carries no provenance"
    );
    assert!(
        !spell_record_matches_filter(
            &unprovable,
            &filter,
            P0,
            runner.state(),
            &FilterContext::neutral()
        ),
        "an unprovable record must fail closed, not answer a vacuous true"
    );
}
