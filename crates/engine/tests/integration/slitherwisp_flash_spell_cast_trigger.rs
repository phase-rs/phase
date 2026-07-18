//! Runtime regression for issue #4754 — Slitherwisp.
//!
//! Oracle: "Flash\nWhenever you cast another spell that has flash, you draw a
//! card and each opponent loses 1 life."
//!
//! The parser previously dropped the "that has flash" clause, leaving the
//! spell-cast trigger's `valid_card` with only the `Another` prop — so it fired
//! on EVERY other spell (the reported bug: casting a counterspell without flash
//! wrongly triggered Slitherwisp). The parser now emits `FilterProp::WithKeyword`
//! for the flash restriction; a parser-shape test alone can't prove the runtime
//! honors it, so these drive the real cast pipeline and assert the trigger's
//! observable effect (each opponent loses 1 life) fires ONLY for a flash spell.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const SLITHERWISP: &str = "Flash\nWhenever you cast another spell that has flash, you draw a \
card and each opponent loses 1 life.";

/// Casting a spell WITH flash triggers Slitherwisp: its controller draws a card
/// and each opponent loses 1 life.
#[test]
fn slitherwisp_triggers_on_flash_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Slitherwisp", 1, 3, SLITHERWISP);
    let flash_spell = scenario
        .add_spell_to_hand(P0, "Flash Bolt", true)
        .from_oracle_text_with_keywords(&["Flash"], "Flash\nYou gain 1 life.")
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_library_top(P0, &["P0 Library A"]);

    let mut runner = scenario.build();
    let p1_life_before = runner.state().players[P1.0 as usize].life;
    let p0_hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner.cast(flash_spell).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before - 1,
        "casting a flash spell must trigger Slitherwisp — each opponent loses 1 life"
    );
    // Hand: the flash spell left hand (-1) and Slitherwisp's draw added one (+1),
    // so the net is the pre-cast count.
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        p0_hand_before,
        "Slitherwisp's controller must draw a card (net hand unchanged after the cast)"
    );
}

/// Casting a spell WITHOUT flash must NOT trigger Slitherwisp — no opponent life
/// loss, no draw. This is the exact reported bug (a non-flash spell wrongly
/// triggering it).
#[test]
fn slitherwisp_does_not_trigger_on_nonflash_spell_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Slitherwisp", 1, 3, SLITHERWISP);
    let plain_spell = scenario
        .add_spell_to_hand(P0, "Plain Bolt", true)
        .from_oracle_text("You gain 1 life.")
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_library_top(P0, &["P0 Library A"]);

    let mut runner = scenario.build();
    let p1_life_before = runner.state().players[P1.0 as usize].life;

    runner.cast(plain_spell).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        p1_life_before,
        "casting a non-flash spell must NOT trigger Slitherwisp — opponent life unchanged"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        0,
        "no Slitherwisp draw for a non-flash spell — the P0 library card stays in the library"
    );
}
