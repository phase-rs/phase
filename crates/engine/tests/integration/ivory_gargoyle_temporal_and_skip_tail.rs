//! CR 603.7a + CR 614.1b — one root cause, two swallowed clauses.
//!
//! Oracle (verified at source, MTGJSON `AtomicCards.json`, both cards identical):
//!   Ivory Gargoyle  / Molten Firebird —
//!     "When this creature dies, return it to the battlefield under its owner's
//!      control at the beginning of the next end step and you skip your next
//!      draw step."
//!   (note: there is NO comma before "and you skip".)
//!
//! `strip_temporal_suffix` only strips a temporal phrase that sits at the END of
//! the clause. Here a conjoined clause follows it, so the phrase is INFIX: the
//! stripper never matched, and the `return it to the battlefield` imperative
//! swallowed the whole remainder. Two independent clauses died together:
//!
//!   1. CR 603.7a — the delayed-return TIMING. The trigger lowered to a bare
//!      `ChangeZone{destination: Battlefield}`, so the creature came back
//!      IMMEDIATELY on death instead of at the next end step.
//!   2. CR 614.1b — the SKIP ("Effects that use the word 'skip' are replacement
//!      effects"). The "and you skip your next draw step" tail was dropped
//!      outright; no `Effect::SkipNextStep` was ever produced.
//!
//! The two witnesses below are deliberately INDEPENDENT — each was watched red
//! on its own, so a fix that recovered only one half could not hide behind the
//! other.

use engine::game::sba::check_state_based_actions;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::triggers::process_triggers;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IVORY_GARGOYLE: &str = "Flying\nWhen this creature dies, return it to the battlefield under its owner's control at the beginning of the next end step and you skip your next draw step.\n{4}{W}: Exile this creature.";

/// Put the Gargoyle on the battlefield, kill it, run SBAs so it dies, process
/// the dies-trigger and resolve the stack. Returns the runner and the Gargoyle's
/// (zone-stable) ObjectId.
fn kill_gargoyle() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gargoyle = scenario
        .add_creature_from_oracle(P0, "Ivory Gargoyle", 2, 2, IVORY_GARGOYLE)
        .id();
    let mut runner = scenario.build();

    runner
        .state_mut()
        .objects
        .get_mut(&gargoyle)
        .expect("gargoyle exists")
        .damage_marked = 99;

    let mut events = Vec::new();
    check_state_based_actions(runner.state_mut(), &mut events);
    process_triggers(runner.state_mut(), &events);
    runner.advance_until_stack_empty();
    (runner, gargoyle)
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Option<Zone> {
    runner.state().objects.get(&id).map(|o| o.zone)
}

/// WITNESS 1 — CR 603.7a: the return is DELAYED to the next end step.
///
/// RED before the fix: the temporal phrase was swallowed, so the trigger
/// resolved to an immediate `ChangeZone` and the Gargoyle was already back on
/// the battlefield the moment the dies-trigger finished resolving.
#[test]
fn ivory_gargoyle_return_is_delayed_to_the_next_end_step() {
    let (mut runner, gargoyle) = kill_gargoyle();

    assert_eq!(
        zone_of(&runner, gargoyle),
        Some(Zone::Graveyard),
        "CR 603.7a: the dies-trigger only SCHEDULES the return for the next end \
         step — the Gargoyle must still be in the graveyard right after it \
         resolves. Finding it on the battlefield here means the \"at the \
         beginning of the next end step\" phrase was swallowed and the return \
         fired immediately."
    );

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        zone_of(&runner, gargoyle),
        Some(Zone::Battlefield),
        "the delayed trigger must fire at the next end step and return the \
         Gargoyle to the battlefield"
    );
}

/// WITNESS 2 — CR 614.1b: the "and you skip your next draw step" tail is a
/// replacement effect and must be registered.
///
/// RED before the fix: the tail was dropped outright — no `SkipNextStep` was
/// produced, so `steps_to_skip` stayed empty and the controller drew normally.
#[test]
fn ivory_gargoyle_skips_the_controllers_next_draw_step() {
    let (runner, _gargoyle) = kill_gargoyle();

    let pending = runner.state().steps_to_skip[P0.0 as usize]
        .get(&Phase::Draw)
        .copied()
        .unwrap_or(0);

    assert_eq!(
        pending, 1,
        "CR 614.1b: \"you skip your next draw step\" is a replacement effect — \
         resolving the dies-trigger must register exactly one pending Draw-step \
         skip for the controller. Zero means the conjoined tail was swallowed \
         with the temporal phrase."
    );
}
