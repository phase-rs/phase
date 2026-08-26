//! Runtime regression for issue #6065 — Inspiring Call.
//!
//! Oracle: "Draw a card for each creature you control with a +1/+1 counter on
//! it. Those creatures gain indestructible until end of turn."
//!
//! The draw half worked, but "those creatures" lowered to `ParentTarget`, which
//! at runtime resolves to the parent Draw effect's target — the CONTROLLER (a
//! player) — so no creatures gained indestructible. The parser now rebinds it to
//! the Draw's count filter (creatures you control with a +1/+1 counter). This
//! drives the real cast pipeline (live-parsed, no card-data dependency) and
//! asserts the grant lands on exactly the countered creatures.

use engine::game::scenario::{GameScenario, P0};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;

const INSPIRING_CALL: &str = "Draw a card for each creature you control with a +1/+1 counter on \
it. Those creatures gain indestructible until end of turn.";

#[test]
fn inspiring_call_grants_indestructible_only_to_countered_creatures() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // A: has a +1/+1 counter → must gain indestructible.
    let countered = scenario
        .add_creature(P0, "Countered Bear", 2, 2)
        .with_plus_counters(1)
        .id();
    // B: no counter → must NOT gain indestructible.
    let plain = scenario.add_creature(P0, "Plain Bear", 2, 2).id();
    let call = scenario
        .add_spell_to_hand(P0, "Inspiring Call", true)
        .from_oracle_text(INSPIRING_CALL)
        .with_mana_cost(ManaCost::zero())
        .id();
    // Library card so the "draw a card for each ..." half has something to draw.
    scenario.with_library_top(P0, &["P0 Lib A", "P0 Lib B"]);

    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    runner.cast(call).resolve();
    runner.advance_until_stack_empty();

    assert!(
        runner.state().objects[&countered].has_keyword(&Keyword::Indestructible),
        "the creature with a +1/+1 counter must gain indestructible"
    );
    assert!(
        !runner.state().objects[&plain].has_keyword(&Keyword::Indestructible),
        "the creature WITHOUT a +1/+1 counter must NOT gain indestructible \
         (the grant must not resolve to all creatures or to the controller)"
    );
}
