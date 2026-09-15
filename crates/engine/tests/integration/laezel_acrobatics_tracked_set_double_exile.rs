//! Row 1.M / claim C1.8 — MEASURE AND RECORD ONLY.
//!
//! # RECORDED VERDICT: A DEFECT EXISTS. It is Contingent Unit 7, DEFERRED(phase 4).
//!
//! Phase 1 is parser-only. It restored Lae'zel's Acrobatics' printed result
//! table (the `1—9` row was swallowed by the spell-resolution continuation loop
//! and only `10—20` survived), and this file then drives the restored branches
//! through the real cast pipeline to answer claim C1.8.
//!
//! **What was measured at this tip:** on BOTH bands the exiled creatures are
//! never returned. They are still in `Zone::Exile` after the next end step, so
//! the cards are LOST. The assertions below PIN THAT DEFECTIVE OUTCOME on
//! purpose, because a parser-only phase must not repair a resolver defect and a
//! permanently red test cannot ship. When Contingent Unit 7 lands in phase 4
//! these assertions WILL FAIL — that is their job. Replace the pinned
//! `Zone::Exile` census with `Zone::Battlefield` then, deliberately.
//!
//! # TRACKED-SET-RETURN-DEFECT — mechanism, measured. Follow-up work, not fixed here.
//!
//! Grep that tag to find every note about this defect.
//!
//! **The published set is CORRECT. The CONSUME side is what fails.**
//!
//! An earlier revision of this note claimed the set held dead pre-exile ids that
//! CR 400.7 had invalidated. That was inferred, not measured, and it is WRONG:
//! `zones.rs:1438` mutates `obj_mut.zone` on the SAME `GameObject` under the
//! same `state.objects` key, the only `ObjectId` allocator is `create_object`
//! (`zones.rs:731`) which the move path never reaches, and CR 400.7 is modeled
//! by bumping `GameObject::incarnation` rather than by issuing a new id. So the
//! published ids stay live, and `filter.rs` evaluates
//! `TargetFilter::TrackedSet` as a bare `set.contains(&object_id)` with no
//! incarnation check and no zone gate.
//!
//! **What actually fails (verified against source).** `change_zone.rs:1982`
//! re-derives the scan zones after the tracked-set sentinel binds, but gates
//! that on `matches!(&ability.effect, Effect::ChangeZoneAll { origin: None, .. })`.
//! A SINGULAR `Effect::ChangeZone` — which is exactly what "Return those cards
//! to the battlefield" lowers to — falls to the `else` branch and keeps the
//! battlefield default, so it scans the BATTLEFIELD for members that are sitting
//! in EXILE and moves nothing. The reason the default is never corrected is that
//! `TargetFilter::extract_in_zone` (`types/ability.rs:19644`) has arms for
//! `Typed`, `Or`/`And`/`Not`, `ExiledBySource` and stack filters but NO
//! tracked-set arm, so a tracked-set filter answers `None`. That gap is already
//! documented in-repo at `put_on_top.rs:341`, where a `Zone::Library` fallback
//! is called out as "LOAD BEARING, NOT DECORATIVE" for the same reason.
//!
//! A `ChangeZoneAll` consumer takes the working re-derivation branch, which is
//! why Mass Polymorph, Synthetic Destiny and Worlds Within Worlds are fine.
//!
//! **Isolating probe (reproduces with NO die table anywhere in the card).** A
//! card whose entire text is
//! `"Exile all nontoken creatures you control. Return those cards to the
//! battlefield under their owner's control."`
//! also fails to return them — inline and delayed-return spellings alike — while
//! the published set is `{TrackedSetId(1): [ObjectId(1), ObjectId(2)]}`, i.e.
//! the correct, still-live ids of the two stranded creatures. So the defect is
//! NOT specific to die tables, not specific to the double exile, and NOT
//! introduced by this phase. It pre-dates this run.
//!
//! **Deliberately NOT repaired by a parser-side `uses_tracked_set` flip.** An
//! earlier revision of this phase marked `CreateDelayedTrigger.uses_tracked_set`
//! on standalone-parsed bodies. That was withdrawn: `TrackedSet { id: 0 }` is a
//! LATE-BOUND SENTINEL which `targeting::resolve_tracked_set_sentinel` — "the
//! single authority for sentinel binding" — already resolves at fire time using
//! the same `chain_tracked_set_id` → `latest_tracked_set_id` lookup the eager
//! flag path uses. The flag therefore bought no behavior here (measured: the
//! creatures are lost identically with and without it), while
//! `delayed_trigger.rs`'s ChangeZone arm has a catch-all
//! (`_ => TargetFilter::TrackedSet { id: real_id }`) that REPLACES a compound
//! target filter wholesale once the flag is true — so flipping it can discard a
//! body's restrictions and turn a WORKING card wrong. Net benefit zero, net risk
//! real; the flip was reverted. It also would not have helped: the failure is on
//! the SCAN-ZONE side, not the binding side.
//!
//! **What was NOT established:** which of the two seams a fix should take — give
//! `extract_in_zone` a tracked-set arm that derives the members' actual zone, or
//! widen `change_zone.rs:1982`'s re-derivation gate to cover singular
//! `ChangeZone`. Both are runtime changes, out of this phase's scope, and the
//! choice is not guessed here.
//!
//! Identity is checked by CARD NAME rather than `ObjectId` so the assertions
//! stay readable, and never by cardinality alone (a run that dropped one card and
//! returned a different one could satisfy a matching count by accident).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::events::GameEvent;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;

/// Verbatim Oracle text, including the printed em-dash result rows.
const LAEZELS_ACROBATICS: &str = "Exile all nontoken creatures you control, then roll a d20.\n1\u{2014}9 | Return those cards to the battlefield under their owner's control at the beginning of the next end step.\n10\u{2014}20 | Return those cards to the battlefield under their owner's control, then exile them again. Return those cards to the battlefield under their owner's control at the beginning of the next end step.";

const FIRST: &str = "Tracked Alpha";
const SECOND: &str = "Tracked Beta";

/// Names of P0's objects in `zone`, sorted.
fn names_in_zone(state: &engine::types::game_state::GameState, zone: Zone) -> Vec<String> {
    let mut names: Vec<String> = state
        .objects
        .values()
        .filter(|o| o.zone == zone && o.owner == P0 && o.name != "Lae'zel's Acrobatics")
        .map(|o| o.name.clone())
        .collect();
    names.sort();
    names
}

/// `(name, zone)` for both tracked creatures, sorted. The CENSUS, not a single
/// zone, is what makes the recorded verdict informative: it distinguishes "lost
/// in exile" from "returned to the wrong zone" from "duplicated".
fn tracked_census(runner: &GameRunner) -> Vec<(String, Zone)> {
    let mut census: Vec<(String, Zone)> = runner
        .state()
        .objects
        .values()
        .filter(|o| o.name == FIRST || o.name == SECOND)
        .map(|o| (o.name.clone(), o.zone))
        .collect();
    census.sort();
    census
}

/// Cast the real spell with two distinctly-named nontoken creatures out, force
/// the die result, then run to the next end step so the delayed return can fire.
fn drive(seed: u64) -> (GameRunner, Option<u32>, Vec<String>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, FIRST, 2, 2);
    scenario.add_creature(P0, SECOND, 3, 3);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Lae'zel's Acrobatics", false, LAEZELS_ACROBATICS)
        .id();

    let mut runner = scenario.build();
    let mut committed = runner.cast(spell).commit();
    // The RNG is reset AFTER commit and immediately before resolution, so the
    // seed -> face mapping is a property of the documented seed rather than of
    // setup activity that may evolve independently of this card.
    let state = committed.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = ChaCha20Rng::seed_from_u64(seed);
    let outcome = committed.resolve();
    let face = outcome.events().iter().find_map(|event| match event {
        GameEvent::DieRolled {
            sides: 20,
            result: Some(result),
            ..
        } => Some(*result as u32),
        _ => None,
    });
    // REACH-GUARD 1, taken at resolution: the exile half actually happened, so
    // the end-step census below is describing a real round trip rather than a
    // run where nothing ever left the battlefield.
    let exiled_at_resolution = names_in_zone(outcome.state(), Zone::Exile);

    let mut runner = GameRunner::from_state(outcome.state().clone());
    // CR 508.1: NO explicit combat crossing here. Every nontoken creature P0
    // controls is in exile at this point, so the declare-attackers turn-based
    // action surfaces no prompt and the combat steps auto-skip. Calling
    // `advance_to_combat` would search for a `DeclareAttackers` phase that never
    // arrives, spin its whole bounded loop, and wrap into the FOLLOWING turn.
    runner.advance_to_end_step();
    // REACH-GUARD 2, taken BEFORE draining the stack: `advance_until_stack_empty`
    // resolves the stack and then continues into the following turn, so the End
    // phase is only observable at this point.
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "reach guard: the scenario must actually reach the end step, or the \
         delayed return never gets a chance to fire and the census below is \
         meaningless"
    );
    runner.advance_until_stack_empty();
    (runner, face, exiled_at_resolution)
}

#[test]
fn laezel_high_band_double_exile_loses_both_creatures_recorded_c1_8_defect() {
    let (runner, face, exiled_at_resolution) = drive(15);
    // REACH-GUARD: without a roll in the printed 10-20 band this fixture never
    // reaches the double-exile branch and the census below describes the wrong
    // branch entirely.
    assert_eq!(face, Some(16), "seed 15 must reach its pinned d20 face");
    let face = face.expect("the roll must have happened");
    assert!(
        (10..=20).contains(&face),
        "this fixture must reach the double-exile 10-20 branch, got {face}"
    );

    // REACH-GUARD: the exile half actually happened during resolution.
    assert_eq!(
        exiled_at_resolution,
        vec![FIRST.to_string(), SECOND.to_string()],
        "the 10-20 branch exiles, returns, then exiles again, so both creatures \
         must be in exile when the spell finishes resolving"
    );

    // ===================== RECORDED C1.8 VERDICT =====================
    // This pins a DEFECT, not correct behavior. The printed row says the cards
    // return to the battlefield at the beginning of the next end step; they do
    // not. Contingent Unit 7, DEFERRED(phase 4). When that fix lands, this
    // assertion fails and must be flipped to Zone::Battlefield deliberately.
    assert_eq!(
        tracked_census(&runner),
        vec![
            (FIRST.to_string(), Zone::Exile),
            (SECOND.to_string(), Zone::Exile),
        ],
        "RECORDED DEFECT (Contingent Unit 7, DEFERRED(phase 4)): both creatures \
         are still in exile after the next end step — the printed return never \
         happens. If this assertion fails, the resolver defect has been fixed \
         and this test must be updated to expect Zone::Battlefield"
    );
    assert!(
        runner
            .state()
            .objects
            .values()
            .all(|o| o.zone != Zone::Battlefield || (o.name != FIRST && o.name != SECOND)),
        "no duplicate copy may appear on the battlefield either"
    );
}

#[test]
fn laezel_low_band_single_exile_loses_both_creatures_recorded_c1_8_defect() {
    // SIBLING BAND. The `1—9` row is the one this phase RESTORED (it was
    // swallowed by the continuation loop at base). It exiles once and never
    // re-publishes, so it carries no double-publish hazard at all — and the
    // loss still reproduces. That is why the module doc declines to assert the
    // CR 400.7 double-publish mechanism: the outcome is broader than it.
    let (runner, face, exiled_at_resolution) = drive(6);
    assert_eq!(face, Some(1), "seed 6 must reach its pinned d20 face");
    let face = face.expect("the roll must have happened");
    assert!(
        (1..=9).contains(&face),
        "this fixture must reach the single-exile 1-9 branch, got {face}"
    );

    assert_eq!(
        exiled_at_resolution,
        vec![FIRST.to_string(), SECOND.to_string()],
        "the 1-9 branch exiles both creatures until the next end step"
    );

    // ===================== RECORDED C1.8 VERDICT =====================
    // Same pinned defect as the high band. See that test's note.
    assert_eq!(
        tracked_census(&runner),
        vec![
            (FIRST.to_string(), Zone::Exile),
            (SECOND.to_string(), Zone::Exile),
        ],
        "RECORDED DEFECT (Contingent Unit 7, DEFERRED(phase 4)): the restored \
         1-9 row's delayed return does not put the cards back. If this fails, \
         the fix landed and this test must expect Zone::Battlefield"
    );
}
