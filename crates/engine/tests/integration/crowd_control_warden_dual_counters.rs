//! Runtime coverage for Crowd-Control Warden's dual replacement:
//!   "As this creature enters or is turned face up, put X +1/+1 counters on it,
//!    where X is the number of other creatures you control."
//!
//! The parser lowers this single sentence to TWO replacements sharing one
//! `PutCounter { P1P1, ObjectCount{Creature, You, [Another]}, SelfRef }` execute:
//! a `Moved`/Battlefield ETB arm (CR 614.1c) and a `TurnFaceUp` arm (CR 708.11).
//! The +1/+1 counters raise power and toughness (CR 122.1a).
//!
//! These tests drive the PRODUCTION runtime, not hand-built ability chains:
//!   - the ETB arm folds counters as the Warden resolves off the stack and enters
//!     (`GameRunner::cast(..).resolve()`);
//!   - the face-up arm fires through the real morph/disguise turn-up path
//!     (`morph::turn_face_up` → `ProposedEvent::TurnFaceUp` → the TurnFaceUp
//!     replacement applier).
//!
//! Discrimination: reverting the parser recognizer regresses the line to
//! `Unimplemented("replacement_structure")` — no replacement is emitted, so the
//! Warden gains ZERO counters and the positive-count assertions below fail.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

// Verbatim replacement line. The separate "Disguise {3}{G/W}{G/W}" line is
// omitted for the ETB-cast tests so the Warden resolves as a plain creature (no
// alternative-cast variant prompt); the enters line parses byte-identically
// either way, and the full-Oracle shape (with Disguise) is covered by the
// crate-internal parser-shape test.
const WARDEN_ETB_ORACLE: &str = "As this creature enters or is turned face up, put X +1/+1 \
     counters on it, where X is the number of other creatures you control.";

// Full verbatim Oracle (with the Disguise line) for the turn-up arm — turning a
// face-down Disguise creature up is the real path that raises TurnFaceUp.
const WARDEN_FULL_ORACLE: &str = "As this creature enters or is turned face up, put X +1/+1 \
     counters on it, where X is the number of other creatures you control.\n\
     Disguise {3}{G/W}{G/W}";

fn plus_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// R-a + R-d: cast the Warden with two OTHER creatures on P0's battlefield → it
/// enters with exactly two +1/+1 counters (one per other creature), NOT three —
/// `FilterProp::Another` excludes the Warden itself. This is the ETB-arm
/// revert-tripwire: without the recognizer the enters line is Unimplemented and
/// the Warden enters with zero counters, flipping the assertion.
#[test]
fn etb_gains_one_counter_per_other_creature_excluding_self() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear A", 2, 2);
    scenario.add_creature(P0, "Bear B", 2, 2);
    let warden = scenario
        .add_creature_to_hand_from_oracle(P0, "Crowd-Control Warden", 3, 3, WARDEN_ETB_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.cast(warden).resolve();

    assert_eq!(
        runner.state().objects[&warden].zone,
        engine::types::zones::Zone::Battlefield,
        "the Warden must have entered the battlefield"
    );
    assert_eq!(
        plus_counters(&runner, warden),
        2,
        "Warden enters with one +1/+1 counter per OTHER creature (2), not 3 (self-exclusion)"
    );
}

/// R-c (edge): with NO other creatures, X = 0 → the Warden enters with no +1/+1
/// counters. Positive reach-guard: the Warden is on the battlefield (the cast
/// resolved), so the zero is a resolved X=0, not a short-circuit. (This edge does
/// not discriminate the recognizer on its own — a reverted recognizer also yields
/// zero; the ETB-arm revert-tripwire is
/// `etb_gains_one_counter_per_other_creature_excluding_self`.)
#[test]
fn etb_with_no_other_creatures_gains_zero_counters() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let warden = scenario
        .add_creature_to_hand_from_oracle(P0, "Crowd-Control Warden", 3, 3, WARDEN_ETB_ORACLE)
        .id();
    let mut runner = scenario.build();

    runner.cast(warden).resolve();

    assert_eq!(
        runner.state().objects[&warden].zone,
        engine::types::zones::Zone::Battlefield,
        "reach-guard: the Warden resolved and entered the battlefield"
    );
    assert_eq!(
        plus_counters(&runner, warden),
        0,
        "no other creatures → X = 0 → no +1/+1 counters"
    );
}

/// R-b: put the Warden onto the battlefield FACE DOWN (Disguise-style 2/2), then
/// turn it face up through the production morph turn-up path with two other
/// creatures present.
///
/// CR 708.3 + CR 708.2a: the face-down entry SUPPRESSES the Warden's own ETB
/// `Moved` counter arm — a face-down permanent is a 2/2 with no text, turned face
/// down BEFORE it enters, so its own "As ~ enters" replacement has no effect —
/// hence `before == 0`. Turning it face up restores the real characteristics and
/// fires the SEPARATE TurnFaceUp arm, which ADDS exactly two +1/+1 counters (one
/// per other creature). The turn-up is measured as the DELTA so it isolates the
/// TurnFaceUp replacement (reverting that arm makes the delta zero), while
/// `before == 0` is a second revert-tripwire for the entry-suppression guard
/// (reverting the guard in `object_replacement_candidate_applies` folds the ETB
/// arm during the face-down entry → `before == 2` → the `before` assertion reds).
#[test]
fn turned_face_up_adds_one_counter_per_other_creature() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear A", 2, 2);
    scenario.add_creature(P0, "Bear B", 2, 2);
    let warden = scenario
        .add_creature_to_hand_from_oracle(P0, "Crowd-Control Warden", 3, 3, WARDEN_FULL_ORACLE)
        .id();
    let mut runner = scenario.build();

    let mut events: Vec<GameEvent> = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), P0, warden, &mut events)
        .expect("play the Warden face down");
    let before = plus_counters(&runner, warden);
    assert_eq!(
        before, 0,
        "CR 708.3 + CR 708.2a: the face-down entry suppresses the Warden's own ETB counter \
         arm (a face-down permanent has no text); reverting the suppression guard yields 2"
    );

    engine::game::morph::turn_face_up(runner.state_mut(), P0, warden, &mut events)
        .expect("turn the Warden face up");
    let after = plus_counters(&runner, warden);

    assert_eq!(
        after,
        before + 2,
        "turning the Warden face up adds one +1/+1 counter per other creature (2) via the \
         TurnFaceUp arm; before={before}, after={after}"
    );
    assert!(
        after > before,
        "the TurnFaceUp arm must place at least one counter (reach-guard)"
    );
}

/// RIDER 4 — THE DISCRIMINATOR for the CR 708.3/708.2a suppression guard.
///
/// Play the Warden onto the battlefield FACE DOWN with two OTHER creatures
/// present. A face-down permanent is a 2/2 with no text (CR 708.2a), turned face
/// down BEFORE it enters (CR 708.3), so its own "As ~ enters ... put X counters"
/// replacement has no effect → ZERO counters.
///
/// REVERT-TO-RED: comment out the `is_entering && ZoneChange{face_down_profile:
/// Some}` guard in `object_replacement_candidate_applies` → the Warden's own
/// `Moved` arm resolves `ObjectCount` = 2 during the face-down entry → 2 counters
/// → this assertion reds. This is the primary revert-tripwire for the fix.
#[test]
fn warden_played_face_down_gains_zero_counters() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P0, "Bear A", 2, 2);
    scenario.add_creature(P0, "Bear B", 2, 2);
    let warden = scenario
        .add_creature_to_hand_from_oracle(P0, "Crowd-Control Warden", 3, 3, WARDEN_FULL_ORACLE)
        .id();
    let mut runner = scenario.build();

    let mut events: Vec<GameEvent> = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), P0, warden, &mut events)
        .expect("play the Warden face down");

    let obj = &runner.state().objects[&warden];
    assert_eq!(
        obj.zone,
        engine::types::zones::Zone::Battlefield,
        "reach-guard: the face-down entry was delivered to the battlefield"
    );
    assert!(
        obj.face_down,
        "reach-guard: the Warden entered FACE DOWN (CR 708.3) — the suppression path"
    );
    assert_eq!(
        plus_counters(&runner, warden),
        0,
        "face-down entry suppresses the Warden's OWN ETB counter replacement (CR 708.2a); \
         reverting the guard yields 2"
    );
}

// --- Masked-class regression guards (CR 708.3/708.2a) — Hooded Hydra (Megamorph) ---
//
// Hooded Hydra's ETB arm is "enters with X +1/+1 counters" (X = 0 when played
// face down — no X is announced), so its face-down count is ZERO both with AND
// without the suppression guard: a REGRESSION guard, NOT a discriminator (§10.3).
// The masked-class DISCRIMINATION comes from the turn-up test (the Megamorph
// "put five" TurnFaceUp arm is unaffected by the entry guard) plus the Warden NEG
// test above. The unrelated dies-trigger line is omitted for focus; the ETB and
// turn-up lines are verbatim.
const HOODED_HYDRA_ORACLE: &str = "Hooded Hydra enters the battlefield with X +1/+1 \
     counters on it.\n\
     Megamorph {3}{G}{G}\n\
     As Hooded Hydra is turned face up, put five +1/+1 counters on it.";

/// REGRESSION guard (§10.3): yields 0 both pre- and post-fix (X = 0 face down),
/// so it does NOT red on guard revert. The reach-guard proves the entry reached
/// the face-down battlefield path with a real X-counter ETB arm present; the
/// masked-class discrimination lives in `hooded_hydra_turn_up_adds_five` + the
/// Warden NEG test.
#[test]
fn hooded_hydra_morph_face_down_zero_counters() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = scenario
        .add_creature_to_hand_from_oracle(P0, "Hooded Hydra", 1, 1, HOODED_HYDRA_ORACLE)
        .id();
    let mut runner = scenario.build();

    let mut events: Vec<GameEvent> = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), P0, hydra, &mut events)
        .expect("play Hooded Hydra face down");

    let obj = &runner.state().objects[&hydra];
    assert_eq!(
        obj.zone,
        engine::types::zones::Zone::Battlefield,
        "reach-guard: entered the battlefield"
    );
    assert!(obj.face_down, "reach-guard: entered FACE DOWN (CR 708.3)");
    assert_eq!(
        plus_counters(&runner, hydra),
        0,
        "face-down X-counter ETB arm contributes no counters (X = 0, and the arm is suppressed)"
    );
}

/// Masked-class turn-up breadth (§10.3): the Megamorph "As ~ is turned face up,
/// put five +1/+1 counters" TurnFaceUp arm is UNAFFECTED by the entry guard,
/// which matches only `ZoneChange { face_down_profile: Some }`. `before == 0`
/// (entry suppressed), turn-up delta == 5. Does NOT red on guard revert (the
/// guard is inert for TurnFaceUp); the discriminator is the Warden NEG test.
///
/// The engine does not model megamorph's inherent +1/+1 counter (pre-existing,
/// out of scope), so the delta is exactly the parsed "put five" arm.
#[test]
fn hooded_hydra_turn_up_adds_five() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let hydra = scenario
        .add_creature_to_hand_from_oracle(P0, "Hooded Hydra", 1, 1, HOODED_HYDRA_ORACLE)
        .id();
    let mut runner = scenario.build();

    let mut events: Vec<GameEvent> = Vec::new();
    engine::game::morph::play_face_down(runner.state_mut(), P0, hydra, &mut events)
        .expect("play Hooded Hydra face down");
    let before = plus_counters(&runner, hydra);
    assert_eq!(
        before, 0,
        "face-down entry suppresses the ETB counter arm (CR 708.3)"
    );

    engine::game::morph::turn_face_up(runner.state_mut(), P0, hydra, &mut events)
        .expect("turn Hooded Hydra face up");
    let after = plus_counters(&runner, hydra);

    assert_eq!(
        after - before,
        5,
        "the Megamorph TurnFaceUp arm places five +1/+1 counters; before={before}, after={after}"
    );
}
