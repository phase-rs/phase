//! Surface parser near-miss: an "another target ..." subject on an imperative
//! effect was not subject-stripped.
//!
//! The subject-strip dispatch gate `starts_with_subject_prefix`
//! (`oracle_effect/subject.rs`) listed the bare "target " subject prefix but not
//! "another target ". So "target creature phases out" was stripped to the
//! `Effect::PhaseOut` imperative, while "another target ... phases out" fell
//! through to first-word dispatch on "another" and lowered to
//! `Effect::Unimplemented` -- nothing happened at resolution and the whole
//! ability rendered unsupported.
//!
//! The one-arm fix adds the "another target " prefix alongside the existing
//! "target " arm, routing the clause through the same `parse_subject_application`
//! "another target" handler (which applies the Another / source-exclusion filter
//! property, exactly as the bare "target" sibling does) and the existing
//! imperative effect + runtime. No new effect variant and no runtime path.
//!
//! The fix flips three real cards from unsupported to fully supported --
//!   * Tovolar's Packleader: "{2}{G}{G}: Another target Wolf or Werewolf you
//!     control fights target creature you don't control."
//!   * Cybernetica Datasmith: "... Another target player creates a 4/4 ... Robot
//!     ... creature token ..."
//!   * Feral Contest: "... Another target creature blocks it this turn if able."
//!     -- and resolves the phase-out clause of The Phasing of Zhalfir's Chapter I/II
//!     ("Another target nonland permanent phases out.").
//!
//! This test drives the phase-out variant because it is the cleanest single
//! runtime-state delta (phased in -> phased out, no combat or damage). It drives
//! the REAL cast -> resolve -> PhaseOut pipeline and FAILS on `main`: the clause
//! parses to `Unimplemented`, so the targeted permanent never phases out.

use engine::game::game_object::{PhaseOutCause, PhaseStatus};
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;

// The Phasing of Zhalfir Chapter I/II effect, exercised at sorcery speed as a
// standalone one-shot (the subject-strip fix is context-independent: the same
// clause parse runs inside a Saga chapter, a spell, or an activated ability).
const PHASE_OUT_ANOTHER: &str = "Another target nonland permanent phases out.";

/// Casting an "another target nonland permanent phases out" clause at an
/// opponent's nonland permanent must phase that permanent out. On `main` the
/// clause lowered to `Unimplemented`, so the target stayed phased in and this
/// assertion failed.
#[test]
fn another_target_nonland_permanent_phases_out() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // An opponent's nonland permanent — the phase-out target.
    let victim = scenario.add_creature(P1, "Serra Angel", 4, 4).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Zhalfir Phase Trial", false, PHASE_OUT_ANOTHER)
        .id();

    let mut runner = scenario.build();

    assert!(
        !runner.state().objects[&victim].is_phased_out(),
        "precondition: the targeted permanent starts phased in"
    );

    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();

    // End-to-end runtime delta: the targeted nonland permanent's status is now
    // "phased out", directly (CR 702.26b; direct vs indirect per CR 702.26g). On
    // `main` this is still phased in because the clause never parsed to
    // `PhaseOut`.
    assert!(
        matches!(
            outcome.state().objects[&victim].phase_status,
            PhaseStatus::PhasedOut {
                cause: PhaseOutCause::Directly
            }
        ),
        "the \"another target ... phases out\" clause must phase out the targeted \
         nonland permanent; got phase_status={:?} (waiting_for={:?})",
        outcome.state().objects[&victim].phase_status,
        outcome.state().waiting_for
    );
}
