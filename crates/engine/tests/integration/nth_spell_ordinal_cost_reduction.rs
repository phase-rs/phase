//! CR 601.2f regression (runtime cast pipeline): an ordinal "the second spell you
//! cast each turn costs {N} less" cost reducer (Highspire Bell-Ringer, Uthros
//! Psionicist, Monk Class, Raging Battle Mouse, Alisaie Leveilleur) must discount
//! ONLY the second qualifying spell of the turn — not the first, not the third.
//!
//! Before the parser fix, `parse_nth_qualified_spell_filter` matched only the
//! literal "the first " prefix, so "the second spell you cast each turn costs {1}
//! less" fell through to the generic cost-modifier path and emitted a filterless,
//! conditionless reducer — discounting EVERY spell the controller cast. The fix
//! parameterizes the ordinal seam so "second" lowers to
//! `SpellsCastThisTurn(you) == 1`, applied here through the real cost pipeline
//! (`collect_battlefield_cost_modifiers` → `evaluate_cost_mod_static_condition`).
//!
//! This test drives `GameScenario` → `cast().resolve()` and measures the mana
//! actually spent per cast. On the pre-fix behavior the first cast is discounted
//! and this test fails (spent1 == 1, not 2).

use engine::game::scenario::{GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;

#[test]
fn second_spell_ordinal_reduction_discounts_only_the_second_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The static source: a battlefield permanent that reduces the SECOND spell
    // its controller casts each turn by {1}. Verbatim Oracle text so the whole
    // parse → layers → cost pipeline is exercised (no card-DB dependency).
    scenario.add_creature_from_oracle(
        P0,
        "Ordinal Bell-Ringer",
        2,
        1,
        "The second spell you cast each turn costs {1} less to cast.",
    );

    // Three {2}-generic vanilla creature spells to cast in sequence.
    let s1 = scenario
        .add_creature_to_hand(P0, "Spell One", 1, 1)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let s2 = scenario
        .add_creature_to_hand(P0, "Spell Two", 1, 1)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let s3 = scenario
        .add_creature_to_hand(P0, "Spell Three", 1, 1)
        .with_mana_cost(ManaCost::generic(2))
        .id();
    let mut runner = scenario.build();

    // Exactly enough colorless mana for full {2} + reduced {1} + full {2} = 5.
    for _ in 0..5 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    // Cast 1 (first spell this turn): gate is `SpellsCastThisTurn == 1`, but zero
    // spells have been cast, so NO discount — pays full {2}.
    let before1 = runner.state().players[0].mana_pool.total();
    runner.cast(s1).resolve();
    let spent1 = before1 - runner.state().players[0].mana_pool.total();
    assert_eq!(
        spent1, 2,
        "the FIRST spell pays full {{2}} (the second-spell gate is not yet satisfied)"
    );

    // Cast 2 (second spell): one spell already cast this turn ⇒ gate satisfied ⇒
    // discounted by {1} ⇒ pays {1}.
    let before2 = runner.state().players[0].mana_pool.total();
    runner.cast(s2).resolve();
    let spent2 = before2 - runner.state().players[0].mana_pool.total();
    assert_eq!(spent2, 1, "the SECOND spell is discounted by {{1}} (2 - 1)");

    // Cast 3 (third spell): two spells already cast ⇒ gate (`== 1`) no longer
    // satisfied ⇒ full {2} again. This is what distinguishes the ordinal gate
    // from a plain "spells you cast cost {1} less" reducer.
    let before3 = runner.state().players[0].mana_pool.total();
    runner.cast(s3).resolve();
    let spent3 = before3 - runner.state().players[0].mana_pool.total();
    assert_eq!(
        spent3, 2,
        "the THIRD spell pays full {{2}} again (only the second spell is discounted)"
    );
}
