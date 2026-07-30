//! Regression coverage for Oblivion's Hunger — the conditional draw
//! ("Draw a card **if that creature has a +1/+1 counter on it**") whose
//! condition was previously SWALLOWED, so the draw fired unconditionally
//! (coverage gap `Swallow:Condition_If`).
//!
//! The demonstrative "that creature" denotes the spell's TARGET object
//! (CR 115.1), so the counter check must resolve against the targeted creature
//! (`ObjectScope::Target`), not the spell (`ObjectScope::Source`). The parser fix
//! recognizes the demonstrative subject in non-trigger context and threads the
//! `Target` scope into the `QuantityCheck`; everything downstream (the
//! `CountersOn { scope: Target }` resolution via `object_id_for_scope`) already
//! ships.
//!
//! CARD TEXT: the Oracle string below is Oblivion's Hunger's verbatim Oracle
//! text — an instant, "Target creature you control gains indestructible until
//! end of turn. Draw a card if that creature has a +1/+1 counter on it."

use engine::game::scenario::GameScenario;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P0: PlayerId = PlayerId(0);

/// Oblivion's Hunger — verbatim Oracle text.
const ORACLE: &str = "Target creature you control gains indestructible until end of turn. \
Draw a card if that creature has a +1/+1 counter on it.";

/// Setup A — POSITIVE REACH-GUARD. The targeted creature HAS a +1/+1 counter, so
/// the conditional draw fires (CR 121.1: draw a card; CR 115.1: "that creature"
/// is the target). Proves the draw path is genuinely reached when the condition
/// holds — the non-vacuous sibling for the revert-failing negative below.
#[test]
fn oblivions_hunger_draws_when_target_has_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Seed a card so the draw has something to pull (CR 121.1).
    scenario.with_library_top(P0, &["Filler A", "Filler B"]);

    let creature = scenario
        .add_creature(P0, "Counter Bearer", 2, 2)
        .with_plus_counters(1)
        .id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Oblivion's Hunger", true, ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[creature]).resolve();

    // CR 115.1 + CR 121.1: target has a +1/+1 counter → the conditional draw fires.
    outcome.assert_hand_drawn(P0, 1);
}

/// Setup B — REVERT-FAILING NEGATIVE. The targeted creature has NO counter, while
/// a DIFFERENT P0 creature DOES (a decoy). Because "that creature" is the TARGET
/// (CR 115.1), the counter check evaluates against the bare target and the draw
/// must NOT fire. Before the fix the condition was swallowed and the draw was
/// unconditional (drew 1) — this assertion flips to a failure when the fix is
/// reverted. The decoy makes it discriminating: a naive "any creature has a
/// counter" reading would still draw here.
#[test]
fn oblivions_hunger_no_draw_when_target_lacks_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Filler A", "Filler B"]);

    // Decoy: a P0 creature WITH a +1/+1 counter that is NOT the spell's target.
    scenario
        .add_creature(P0, "Decoy Counter Bearer", 2, 2)
        .with_plus_counters(1);
    // The actual target: a bare creature with no counter.
    let bare = scenario.add_creature(P0, "Bare Creature", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Oblivion's Hunger", true, ORACLE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[bare]).resolve();

    // CR 115.1: the condition binds to the TARGET (no counter) — not the decoy —
    // so the draw is suppressed. Reverting the parser fix (condition swallowed →
    // unconditional draw) makes this assertion fail.
    outcome.assert_hand_drawn(P0, 0);
}
