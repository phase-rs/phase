//! Phase 1 (Myrkul/Crew) — all-zone incarnation semantics (CR 400.7).
//!
//! State-level drivers for the §4 bump seams: `move_to_zone`'s else-arm bump
//! (`from != to`), `move_to_library_at_index`'s `from != Zone::Library` bump, and
//! the negative cases (within-library reposition, unrelated object). These pin the
//! all-zone broadening: reverting any bump makes the matching assertion fail.

use engine::game::scenario::GameScenario;
use engine::game::zones::{move_to_library_at_index, move_to_zone};
use engine::types::ability::{Effect, QuantityExpr, ResolvedAbility, TargetFilter};
use engine::types::game_state::{ResolutionSourceRelatch, StackEntry, StackEntryKind};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::{CardId, ObjectId};

const P0: PlayerId = PlayerId(0);

/// A drawing ability whose source self-reference was captured at `stamp`.
fn drawing_ability(source: ObjectId, stamp: u64) -> ResolvedAbility {
    let mut a = ResolvedAbility::new(
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );
    a.set_test_trigger_source_recursive(stamp, CardId(0));
    a
}

// T-zone×6 / T-blink: every genuine zone change advances `incarnation`, including
// the non-battlefield moves the old battlefield-only bump left untouched. Fails
// under the battlefield-only bump (BF→GY and GY→Exile would not increase).
#[test]
fn non_battlefield_zone_changes_bump_incarnation() {
    let mut scenario = GameScenario::new();
    let creature = scenario.add_vanilla(P0, 2, 2);
    let mut runner = scenario.build();
    let mut events = Vec::new();

    let start = runner.state().objects[&creature].incarnation;

    move_to_zone(runner.state_mut(), creature, Zone::Graveyard, &mut events);
    let after_gy = runner.state().objects[&creature].incarnation;
    assert!(after_gy > start, "BF→GY bumps incarnation (else-arm)");

    move_to_zone(runner.state_mut(), creature, Zone::Exile, &mut events);
    let after_exile = runner.state().objects[&creature].incarnation;
    assert!(
        after_exile > after_gy,
        "GY→Exile bumps incarnation (neither zone is battlefield)"
    );

    move_to_zone(runner.state_mut(), creature, Zone::Battlefield, &mut events);
    let after_bf = runner.state().objects[&creature].incarnation;
    assert!(
        after_bf > after_exile,
        "Exile→BF (blink) bumps incarnation (battlefield arm via reset)"
    );

    move_to_library_at_index(runner.state_mut(), creature, None, &mut events);
    let after_lib = runner.state().objects[&creature].incarnation;
    assert!(
        after_lib > after_bf,
        "BF→Library bumps incarnation (move_to_library_at_index, from != Library)"
    );
}

// T-libstay: a within-Library reposition (reveal / scry bottom placement) is zero
// moves (CR 701.20b) and must NOT bump. Fails (spurious bump) without the
// `from != Zone::Library` guard in `move_to_library_at_index`.
#[test]
fn within_library_reposition_does_not_bump_incarnation() {
    let mut scenario = GameScenario::new();
    let card = scenario.add_card_to_library_top(P0, "Plains");
    let mut runner = scenario.build();
    let mut events = Vec::new();

    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Library,
        "reach-guard: the card really starts in the library"
    );
    let before = runner.state().objects[&card].incarnation;
    // Reposition to the bottom of the SAME library (from == Library).
    move_to_library_at_index(runner.state_mut(), card, None, &mut events);
    let after = runner.state().objects[&card].incarnation;
    assert_eq!(
        after, before,
        "within-Library reposition (from == Library) must not bump"
    );
}

// T-stay: the bump is per-object-move, not per-event — moving object A must not
// advance an unrelated object B's incarnation.
#[test]
fn unrelated_object_incarnation_not_advanced_by_another_move() {
    let mut scenario = GameScenario::new();
    let a = scenario.add_vanilla(P0, 1, 1);
    let b = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();
    let mut events = Vec::new();

    let b_before = runner.state().objects[&b].incarnation;
    move_to_zone(runner.state_mut(), a, Zone::Graveyard, &mut events);
    assert_eq!(
        runner.state().objects[&b].incarnation,
        b_before,
        "moving A must not bump B's incarnation"
    );
}

// T-relatch-chain (§4.3): a double self-move in one resolution (BF→Exile→BF)
// keeps the source findable through both hops. The write chains: the first hop
// sets `original_stamp`; the second keeps it fixed and only advances
// `current_incarnation`. Fails (record stuck at the first post-move value, or no
// relatch consulted) without the chaining write + `source_is_current` read.
#[test]
fn self_move_relatch_chains_across_two_hops() {
    let mut scenario = GameScenario::new();
    let src = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();
    let mut events = Vec::new();

    let captured = runner.state().objects[&src].incarnation;

    // Mark `src` as the currently-resolving ability's OWN source at stamp `captured`.
    runner.state_mut().resolving_stack_entry = Some(StackEntry {
        id: ObjectId(9001),
        source_id: src,
        controller: P0,
        kind: StackEntryKind::ActivatedAbility {
            source_id: src,
            ability: drawing_ability(src, captured),
        },
    });

    // Hop 1: BF → Exile (else-arm bump + first relatch write).
    move_to_zone(runner.state_mut(), src, Zone::Exile, &mut events);
    // Hop 2: Exile → BF (battlefield-arm bump + chained relatch write).
    move_to_zone(runner.state_mut(), src, Zone::Battlefield, &mut events);

    let consumer = drawing_ability(src, captured);
    assert!(
        consumer.source_is_current(runner.state()),
        "the chained relatch re-finds the twice-moved source (CR 400.7j + g/h)"
    );

    let record = runner
        .state()
        .resolution_source_relatch
        .expect("relatch was recorded");
    assert_eq!(record.object_id, src);
    assert_eq!(
        record.original_stamp, captured,
        "original_stamp stays fixed across chained hops"
    );
    assert_eq!(
        record.current_incarnation,
        runner.state().objects[&src].incarnation,
        "current_incarnation tracks the latest post-move value"
    );
}

// T-relatch-stale (§4.3): a stale-stamped delayed trigger for the same object_id
// must NOT ride a live relatch record; the read is bound to `original_stamp`.
// The positive reach-guard (an ability that DID capture original_stamp) proves the
// negative is not vacuous. Fails (spurious relatch) if the read drops the
// `original_stamp == captured` binding.
#[test]
fn stale_stamped_ability_does_not_ride_live_relatch() {
    let mut scenario = GameScenario::new();
    let src = scenario.add_vanilla(P0, 1, 1);
    let mut runner = scenario.build();

    // Object is currently at incarnation 2 (as if it moved once); a live record
    // names original_stamp 1 → current 2.
    runner
        .state_mut()
        .objects
        .get_mut(&src)
        .unwrap()
        .incarnation = 2;
    runner.state_mut().resolution_source_relatch = Some(ResolutionSourceRelatch {
        object_id: src,
        original_stamp: 1,
        current_incarnation: 2,
    });

    // A stale-stamped ability captured at M = 5 (≠ original_stamp) must not relatch.
    let stale = drawing_ability(src, 5);
    assert!(
        !stale.source_is_current(runner.state()),
        "a stale-stamped ability (M != original_stamp) must not ride the record"
    );

    // Positive reach-guard: the ability that captured original_stamp DOES relatch.
    let fresh = drawing_ability(src, 1);
    assert!(
        fresh.source_is_current(runner.state()),
        "the ability that captured original_stamp relatches (non-vacuous negative)"
    );
}

// T-merge-split (§4.2): when a merged permanent leaves the battlefield, each
// absorbed component becomes a CR 730.3 / CR 400.7 NEW object in its owner's zone
// via `put_component_into_zone`, which must bump its incarnation. A delayed trigger
// that captured the component's pre-split stamp N must then read
// `source_is_current == false`. This is the ONLY coverage of the merge.rs
// leave-split bump — reverting it is caught by nothing else.
#[test]
fn merge_leave_split_component_becomes_new_object() {
    use engine::game::merge::{merge_object_onto, split_merged_permanent_on_leave, MergeSide};

    let mut scenario = GameScenario::new();
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let rider = scenario.add_creature(P0, "Rider", 4, 4).id();
    let mut runner = scenario.build();
    let mut events = Vec::new();

    // The rider (component B) is absorbed at stamp N. Mutate ENTRY (CR 730.2b/c) is
    // excluded from the bump, so absorbing does not advance the rider's incarnation.
    let n = runner.state().objects[&rider].incarnation;
    merge_object_onto(runner.state_mut(), rider, host, MergeSide::Top, &mut events);
    assert_eq!(
        runner.state().objects[&rider].incarnation,
        n,
        "reach-guard: absorbing the rider does not bump (mutate ENTRY is excluded)"
    );

    // A delayed trigger on the rider captured at stamp N is current while merged.
    let captured = drawing_ability(rider, n);
    assert!(
        captured.source_is_current(runner.state()),
        "reach-guard: the stamp-N trigger matches the still-merged rider"
    );

    // Kill the merged permanent → leave-split routes the rider to the graveyard as
    // a NEW object via the bumped `put_component_into_zone`.
    split_merged_permanent_on_leave(runner.state_mut(), host, Zone::Graveyard, &mut events);

    assert_eq!(
        runner.state().objects[&rider].zone,
        Zone::Graveyard,
        "non-vacuity: the rider actually left-split to the graveyard (not an early return)"
    );
    assert!(
        runner.state().objects[&rider].incarnation > n,
        "CR 730.3 + CR 400.7: the split-out component is a new object (incarnation bumped)"
    );
    assert!(
        !captured.source_is_current(runner.state()),
        "the stamp-N delayed trigger no longer matches the CR-400.7-new component"
    );
}
