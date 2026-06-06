//! Issues #1486 / #1533 — Obeka, Splitter of Seconds drops "that many".
//!
//! Oracle (Obeka, Splitter of Seconds):
//! > Menace
//! > Whenever Obeka deals combat damage to a player, you get that many
//! > additional upkeep steps after this phase.
//!
//! Before the fix, the parser produced `Effect::AdditionalPhase` with no count
//! field, so the resolver pushed exactly one extra upkeep step regardless of
//! the combat damage delivered. After the fix, the parser threads
//! `QuantityRef::EventContextAmount` into `Effect::AdditionalPhase { count, .. }`
//! and the resolver pushes one bundle per point of damage.
//!
//! CR 500.8 (extra phases), CR 510.2 (combat damage dealt), CR 503.1
//! (upkeep step).
//!
//! This test drives an unblocked 5/2 Obeka into P1, then asserts that the
//! resolved trigger queued 5 additional upkeep steps — discriminating against
//! the broken behaviour (1 step).

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::ExtraPhase;
use engine::types::phase::Phase;

use super::rules::run_combat;

const OBEKA_ORACLE: &str = "Menace\nWhenever Obeka deals combat damage to a player, \
you get that many additional upkeep steps after this phase.";

/// CR 500.8 + CR 510.2: Five combat damage from Obeka schedules five additional
/// upkeep steps (one ExtraPhase per damage point), not a single step.
#[test]
fn obeka_combat_damage_pushes_one_extra_upkeep_per_damage_point() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // 5 power so a single unblocked swing delivers 5 combat damage to P1.
    let obeka = scenario
        .add_creature_from_oracle(P0, "Obeka, Splitter of Seconds", 5, 2, OBEKA_ORACLE)
        .id();
    let mut runner = scenario.build();

    // Unblocked attack → 5 combat damage to P1 → trigger fires.
    run_combat(&mut runner, vec![obeka], vec![]);
    runner.advance_until_stack_empty();

    // CR 500.8: each "additional upkeep step" entry uses (anchor=Upkeep, phase=Upkeep).
    let expected = ExtraPhase {
        anchor: Phase::Upkeep,
        phase: Phase::Upkeep,
    };
    assert_eq!(
        runner.state().extra_phases,
        vec![expected, expected, expected, expected, expected],
        "Obeka dealing 5 combat damage should schedule exactly 5 additional upkeep steps",
    );
}
