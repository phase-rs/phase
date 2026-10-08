//! Frodo Baggins (ltr) — "As long as Frodo Baggins is your Ring-bearer, it must
//! be blocked if able." must be GATED on the Ring-bearer designation.
//!
//! Before the parser fix, this line split badly: the requirement was cut out
//! mid-clause and the leftover "~ is your Ring-bearer, it" landed in
//! `StaticCondition::Unrecognized`, which the layer system evaluates as
//! ALWAYS TRUE. Frodo therefore had to be blocked even when he was not the
//! Ring-bearer (CR 604.1 — a static ability's condition is simply true or false;
//! an unrecognized one must not silently become unconditional).
//!
//! These tests drive the real public enforcement entry point,
//! `combat::validate_blockers_for_player` (CR 509.1c), against a Frodo built
//! from VERBATIM Oracle text, so the whole production path is exercised:
//! Oracle text -> static parser -> `StaticDefinition.condition` ->
//! `evaluate_condition_with_recipient` -> `is_current_ring_bearer` -> blocker
//! legality.

use engine::game::combat::{validate_blockers_for_player, AttackerInfo, CombatState};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Verbatim Frodo Baggins Oracle text (MTGJSON AtomicCards, set `ltr`).
const FRODO_ORACLE: &str = "Whenever Frodo Baggins or another legendary creature you control enters, the Ring tempts you.\nAs long as Frodo Baggins is your Ring-bearer, it must be blocked if able.";

struct Setup {
    runner: engine::game::scenario::GameRunner,
    frodo: ObjectId,
    /// A second creature P0 controls, so the Ring-bearer designation has
    /// somewhere real to MOVE to (CR 701.54a) in the multi-authority fixture.
    samwise: ObjectId,
    blocker: ObjectId,
}

/// P0's Frodo (1/3) attacks P1, who controls one untapped creature able to block
/// him. P0 also controls a second creature (Samwise) that is NOT attacking.
///
/// The blocker is deliberately POWER 1, not a vanilla 2/2. CR 701.54c (the Ring
/// emblem — "your Ring-bearer ... can't be blocked by creatures with greater
/// power") is enforced independently at
/// `ring_bearer_unblockable_by_greater_power`, so a 2/2 CANNOT legally block a
/// Ring-bearer Frodo — and then the "if able" in CR 509.1c makes an empty
/// declaration legal for a reason that has nothing to do with the condition
/// under test, rendering every positive assertion here silently vacuous. Power 1
/// is not GREATER than Frodo's 1, so the block is legal and the requirement is
/// genuinely enforceable.
fn setup_frodo_attacking() -> Setup {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareBlockers);
    let frodo = scenario
        .add_creature_from_oracle(P0, "Frodo Baggins", 1, 3, FRODO_ORACLE)
        .id();
    let samwise = scenario.add_creature(P0, "Samwise Gamgee", 1, 2).id();
    let blocker = scenario.add_creature(P1, "Able Blocker", 1, 4).id();

    let mut runner = scenario.build();
    runner.state_mut().combat = Some(CombatState {
        attackers: vec![AttackerInfo::attacking_player(frodo, P1)],
        ..Default::default()
    });
    Setup {
        runner,
        frodo,
        samwise,
        blocker,
    }
}

/// Reach-guard for the whole file: the parsed Frodo really does carry exactly
/// one MustBeBlocked static gated on a TYPED (non-Unrecognized) condition.
/// Without this, every `is_ok()` below could pass because Frodo parsed to
/// nothing at all.
#[test]
fn frodo_parses_to_a_single_condition_gated_must_be_blocked_static() {
    let s = setup_frodo_attacking();
    let statics = &s.runner.state().objects[&s.frodo].static_definitions;
    let gated: Vec<_> = statics
        .iter_unchecked()
        .filter(|d| {
            matches!(
                d.mode,
                engine::types::statics::StaticMode::MustBeBlocked { .. }
            )
        })
        .collect();
    assert_eq!(
        gated.len(),
        1,
        "exactly one MustBeBlocked static: {statics:#?}"
    );
    assert_eq!(
        gated[0].condition,
        Some(engine::types::ability::StaticCondition::IsRingBearer),
        "CR 701.54e: the lure must be gated on the Ring-bearer designation, \
         not on an always-true Unrecognized condition"
    );
}

/// #1 — CR 509.1c + CR 701.54e: Frodo IS the Ring-bearer and an able blocker is
/// available, so declaring no blockers is ILLEGAL.
#[test]
fn must_be_blocked_enforced_when_frodo_is_ring_bearer() {
    let mut s = setup_frodo_attacking();
    s.runner.state_mut().ring_bearer.insert(P0, Some(s.frodo));

    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_err(),
        "CR 509.1c: an able blocker must block the Ring-bearer Frodo"
    );
}

/// #2 — positive reach-guard for #1: the requirement is SATISFIABLE. Proves #1's
/// Err is a requirement violation, not a permanently-illegal declaration.
#[test]
fn must_be_blocked_satisfied_when_frodo_is_blocked() {
    let mut s = setup_frodo_attacking();
    s.runner.state_mut().ring_bearer.insert(P0, Some(s.frodo));

    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[(s.blocker, s.frodo)]).is_ok(),
        "CR 509.1c: blocking Frodo obeys the requirement"
    );
}

/// #3 — THE BUG, AND THE REVERT-FAILING CORE. Frodo is NOT the Ring-bearer, so
/// the static's condition is FALSE and no requirement exists: declaring no
/// blockers is LEGAL.
///
/// REVERT-FAIL: on unfixed code the condition parses to
/// `StaticCondition::Unrecognized { text: "~ is your Ring-bearer, it" }`, which
/// the layer system evaluates as always-true, so this returns Err. Paired with
/// #1 (same fixture, designation set) so it cannot pass vacuously, and with #7
/// (no able blocker) so it cannot pass for the wrong reason.
#[test]
fn must_be_blocked_not_enforced_when_frodo_is_not_ring_bearer() {
    let s = setup_frodo_attacking();
    // ring_bearer deliberately unset for P0.
    assert!(
        s.runner.state().ring_bearer.get(&P0).copied().flatten() != Some(s.frodo),
        "reach-guard: Frodo must NOT hold the designation in this scenario"
    );

    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_ok(),
        "CR 604.1 + CR 701.54e: with the condition false there is no requirement, \
         so an empty blocker declaration is legal"
    );
}

/// #4 — MULTI-AUTHORITY / live-binding fixture. CR 611.3a: a static ability's
/// continuous effect is NOT locked in. When the Ring tempts again and the
/// designation MOVES OFF Frodo mid-combat, the requirement must stop applying
/// with no re-parse.
#[test]
fn must_be_blocked_follows_ring_bearer_designation_when_it_moves() {
    let mut s = setup_frodo_attacking();
    // Designation starts on Frodo: requirement enforced.
    s.runner.state_mut().ring_bearer.insert(P0, Some(s.frodo));
    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_err(),
        "reach-guard: enforced while Frodo holds the designation"
    );

    // CR 701.54a: the Ring tempts again and ANOTHER creature P0 controls becomes
    // the Ring-bearer. Frodo is still attacking; nothing is re-parsed.
    s.runner.state_mut().ring_bearer.insert(P0, Some(s.samwise));

    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_ok(),
        "CR 611.3a + CR 701.54a: the gate is live, not snapshotted — once the \
         designation leaves Frodo the requirement stops applying"
    );
}

/// #5 — HOSTILE, wrong controller. CR 701.54e: "your Ring-bearer" is the
/// STATIC'S SOURCE CONTROLLER's designation. An OPPONENT designating their own
/// creature must not switch Frodo's lure on.
#[test]
fn must_be_blocked_not_enforced_by_an_opponents_ring_bearer_designation() {
    let mut s = setup_frodo_attacking();
    // P1 (the defender) has a Ring-bearer; P0 (Frodo's controller) does not.
    s.runner.state_mut().ring_bearer.insert(P1, Some(s.blocker));

    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_ok(),
        "CR 701.54e: only the static source controller's designation counts"
    );
}

/// #6 — HOSTILE, zone gate. CR 701.54e: the designation is valid only while the
/// creature is on the battlefield under that player's control. A stale
/// designation pointing at a Frodo that has left play must not enforce.
#[test]
fn must_be_blocked_not_enforced_when_designated_frodo_left_the_battlefield() {
    let mut s = setup_frodo_attacking();
    s.runner.state_mut().ring_bearer.insert(P0, Some(s.frodo));
    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_err(),
        "reach-guard: enforced while Frodo is on the battlefield"
    );

    // Frodo leaves play but the designation still points at his ObjectId.
    s.runner.state_mut().objects.get_mut(&s.frodo).unwrap().zone = Zone::Graveyard;

    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_ok(),
        "CR 701.54e: is_current_ring_bearer re-checks zone on every call"
    );
}

/// #7 — HOSTILE, "if able" discriminator. CR 509.1c: a requirement is only
/// violated when it COULD have been obeyed. With the designation ON but the
/// defender's only creature tapped, the empty declaration is legal — which is
/// why #3 must be read as "the gate is off", not "no able blocker existed".
#[test]
fn must_be_blocked_not_violated_when_no_blocker_is_able() {
    let mut s = setup_frodo_attacking();
    s.runner.state_mut().ring_bearer.insert(P0, Some(s.frodo));
    s.runner
        .state_mut()
        .objects
        .get_mut(&s.blocker)
        .unwrap()
        .tapped = true;

    assert!(
        validate_blockers_for_player(s.runner.state(), P1, &[]).is_ok(),
        "CR 509.1c: a tapped creature cannot block, so the requirement is not \
         being disobeyed"
    );
}
