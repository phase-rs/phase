//! Mana burn — the pre-M10 rule a custom format can opt back into.
//!
//! The current rules have no such rule; the glossary entry "Mana Burn
//! (Obsolete)" records that "unspent mana caused a player to lose life."
//! Two things separate it from the modern behavior, and both are asserted
//! here against a real phase advance rather than against the helpers:
//!
//! 1. **Pools empty at the end of a PHASE (CR 500.1's five), not every step.**
//!    Modern CR 106.4 / CR 500.5 empty at the end of each step AND phase.
//! 2. **Emptying costs life** equal to the mana lost.
//!
//! Every assertion is paired against the same scenario under a modern format,
//! so a failure to burn and a failure to set the scenario up are
//! distinguishable.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::custom_format::{old_school_93_94, swedish_old_school};
use engine::types::format::FormatConfig;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const POOL: usize = 2;

fn pool(count: usize) -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(9_001), false, vec![]); count]
}

/// A game sitting in the upkeep step with `POOL` unspent mana, under `format`.
///
/// Upkeep is chosen deliberately: it is inside the beginning phase (CR 501.1)
/// alongside untap and draw, so the untap → upkeep → draw run exercises steps
/// the OLD `EndOfCombat` retention could never have covered. A combat-only
/// implementation would pass a combat-based test while failing this one.
fn game_in_upkeep_with_mana(format: FormatConfig) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    // CR 704.5b: the draw step draws a card, and an empty library loses the
    // game — which ends the turn before any phase boundary is reached. Stocking
    // the library keeps the game alive long enough to observe the boundary.
    scenario.with_library_top(P0, &["Mountain", "Mountain", "Mountain"]);
    scenario.with_mana_pool(P0, pool(POOL));
    let mut runner = scenario.build();
    runner.state_mut().format_config = format;
    runner
}

fn unspent(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("player 0 exists")
        .mana_pool
        .total()
}

/// Old School 93/94 declares `mana_burn: Obsolete`, so this is the shipped
/// preset's own behavior, not a synthetic config.
#[test]
fn mana_survives_steps_inside_a_phase_and_burns_when_the_phase_ends() {
    let format = FormatConfig::for_custom_rules(&old_school_93_94().rules);
    let mut runner = game_in_upkeep_with_mana(format);
    let life_before = runner.life(P0);
    assert_eq!(unspent(&runner), POOL, "scenario starts with unspent mana");

    // Upkeep -> Draw: a step boundary INSIDE the beginning phase (CR 501.1).
    // Modern rules would empty the pool here; the pre-M10 rule does not.
    runner.advance_to_phase(Phase::Draw);
    assert_eq!(runner.state().phase, Phase::Draw);
    assert_eq!(
        unspent(&runner),
        POOL,
        "mana must survive a step boundary within one phase"
    );
    assert_eq!(
        runner.life(P0),
        life_before,
        "no life is lost until the phase itself ends"
    );

    // Draw -> PreCombatMain: a real CR 500.1 phase crossing. The pool empties
    // and the emptied count is the life lost.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "did not reach the phase boundary; halted waiting on {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        unspent(&runner),
        0,
        "the pool empties at the phase boundary"
    );
    assert_eq!(
        runner.life(P0),
        life_before - POOL as i32,
        "mana burn costs one life per unspent mana"
    );

    // NOTE: the `GameEvent::ManaBurn` narration is deliberately NOT asserted
    // here. `advance_to_phase` discards the events it drives, and the log is
    // carried on `ActionResult` rather than `GameState`, so there is no honest
    // way to observe it from this harness — asserting something weaker and
    // calling it event coverage would be worse than saying so. Life total and
    // pool contents below are the player-visible behavior either way.
}

/// The paired control: the same scenario under a modern format. Without this,
/// every assertion above would still pass against an engine that emptied pools
/// at the wrong time or never emptied them at all.
#[test]
fn a_modern_format_empties_every_step_and_costs_no_life() {
    let mut runner = game_in_upkeep_with_mana(FormatConfig::standard());
    let life_before = runner.life(P0);

    runner.advance_to_phase(Phase::Draw);
    assert_eq!(
        unspent(&runner),
        0,
        "CR 106.4 / CR 500.5: modern pools empty at the end of every step"
    );
    assert_eq!(runner.life(P0), life_before, "emptying costs nothing");

    // The same second advance the mana-burn test makes. If the harness cannot
    // cross this boundary even with an empty pool, that is a scenario-driver
    // limitation and not a mana-burn defect — this is what tells them apart.
    runner.advance_to_phase(Phase::PreCombatMain);
    assert_eq!(
        runner.state().phase,
        Phase::PreCombatMain,
        "halted waiting on {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.life(P0), life_before);
}

/// A custom format that does NOT declare the axis behaves like a built-in.
/// Swedish Old School is the shipped preset that proves it: an old card pool
/// played under fully modern rules, which is exactly what its source says.
#[test]
fn a_custom_format_without_the_axis_does_not_burn() {
    let format = FormatConfig::for_custom_rules(&swedish_old_school().rules);
    let mut runner = game_in_upkeep_with_mana(format);
    let life_before = runner.life(P0);

    runner.advance_to_phase(Phase::Draw);
    assert_eq!(
        unspent(&runner),
        0,
        "a custom format is not automatically an old-rules format"
    );
    assert_eq!(runner.life(P0), life_before);
}
