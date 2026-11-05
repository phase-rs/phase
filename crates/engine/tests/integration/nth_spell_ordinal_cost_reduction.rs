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
//! The second test covers the same consumer's post-timing qualifier (Head of the
//! Class): the ordinal gate must count the spells the DISCOUNT applies to, not
//! every spell of the turn.
//!
//! These tests drive `GameScenario` → `cast().resolve()` and measure the mana
//! actually spent per cast. On the pre-fix behavior the first cast is discounted
//! and this test fails (spent1 == 1, not 2).

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Head of the Class — verbatim Oracle text (both sentences; the second is the
/// CR 118.7c rider that keeps the reduction on colored mana).
const HEAD_OF_THE_CLASS_ORACLE: &str = "The first spell you cast during each of your turns that targets a creature costs {W}{B} less to cast. This effect reduces only the amount of colored mana you pay.";

/// Doom Blade — a printed `{1}{B}` instant that targets a creature, so Head of the
/// Class's discount is measurable as a pool delta: full `{1}{B}` spends 2, the
/// discounted `{1}` spends 1 (the `{W}` half is lost because the cost has no white
/// pip, per the CR 118.7c rider).
fn doom_blade_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Black],
        generic: 1,
    }
}

/// The Head of the Class board: the creature (a printed 2/2 `{W}{B}` Kithkin
/// Cleric — its cost-modification static is what is under test, and the body is
/// the card's real body), two creature victims (the first is destroyed by a prior
/// targeting cast, so the second targeting spell still has a legal creature
/// target), and two Doom Blades in hand.
struct HeadBoard {
    runner: GameRunner,
    blade_a: ObjectId,
    blade_b: ObjectId,
    victim_a: ObjectId,
    victim_b: ObjectId,
}

fn head_of_the_class_board() -> HeadBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Head of the Class", 2, 2, HEAD_OF_THE_CLASS_ORACLE);
    let victim_a = scenario.add_vanilla(P0, 1, 1);
    let victim_b = scenario.add_vanilla(P0, 1, 1);
    let blade_a = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, "Destroy target nonblack creature.")
        .with_mana_cost(doom_blade_cost())
        .id();
    let blade_b = scenario
        .add_spell_to_hand_from_oracle(P0, "Doom Blade", true, "Destroy target nonblack creature.")
        .with_mana_cost(doom_blade_cost())
        .id();
    HeadBoard {
        runner: scenario.build(),
        blade_a,
        blade_b,
        victim_a,
        victim_b,
    }
}

/// Fill P0's pool with `black` black and `colorless` colorless mana.
fn fill_pool(runner: &mut GameRunner, black: usize, colorless: usize) {
    for _ in 0..black {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Black,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    for _ in 0..colorless {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

/// Cast `spell` (targeting `victim`) and report the mana spent.
fn cast_and_measure(runner: &mut GameRunner, spell: ObjectId, victim: ObjectId) -> usize {
    let before = runner.state().players[0].mana_pool.total();
    runner.cast(spell).target_objects(&[victim]).resolve();
    before - runner.state().players[0].mana_pool.total()
}

#[test]
fn second_spell_ordinal_reduction_discounts_only_the_second_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // The static source: Highspire Bell-Ringer with its printed body and whole
    // Oracle text (a 1/4 with flying that reduces the SECOND spell its controller
    // casts each turn by {1}). Verbatim text so the whole parse → layers → cost
    // pipeline is exercised (no card-DB dependency).
    scenario.add_creature_from_oracle(
        P0,
        "Highspire Bell-Ringer",
        1,
        4,
        "Flying\nThe second spell you cast each turn costs {1} less to cast.",
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

/// CR 601.2f + CR 115.1 + CR 601.2c: Head of the Class — "The first spell you cast
/// during each of your turns that targets a creature costs {W}{B} less to cast."
///
/// The discount filter has always carried the printed target clause; before this
/// fix the ordinal GATE was built from a narrower, qualifier-free filter, so it
/// counted every spell of the turn and a prior non-targeting spell spent the
/// discount. The gate now counts the set the discount applies to (charter
/// decision 8's one-value rule).
///
/// Three legs, each in its own game because each needs a different cast history:
///   leg 0 — no prior spell: the discount applies (reach guard; green on both the
///           pre- and post-fix trees, since the gate is satisfied either way).
///   leg 1 — a prior NON-targeting creature spell: the targeting spell still pays
///           the discounted `{1}`. Pre-fix the gate counted that prior spell
///           (`1 != 0`) and the same cast paid `{2}` — this is the assertion that
///           flips when the fix is reverted.
///   leg 2 — a prior TARGETING spell: it *does* spend the discount, so the second
///           targeting spell pays `{2}`. This is decision 8's "a prior matching one
///           consumes it", and it also fails if the record matcher answers `false`
///           for every declared-target fact (a vacuously-true gate).
#[test]
fn head_of_the_class_discount_survives_a_prior_nontargeting_spell() {
    // Leg 0: no prior spell of any kind.
    let mut leg0 = head_of_the_class_board();
    fill_pool(&mut leg0.runner, 2, 4);
    let spent = cast_and_measure(&mut leg0.runner, leg0.blade_a, leg0.victim_a);
    assert_eq!(
        spent, 1,
        "with no prior spell the first creature-targeting spell pays the discounted {{1}}"
    );

    // Leg 1: a prior non-targeting creature spell must not spend the discount.
    {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_creature_from_oracle(P0, "Head of the Class", 2, 2, HEAD_OF_THE_CLASS_ORACLE);
        let victim_a = scenario.add_vanilla(P0, 1, 1);
        let prior = scenario
            .add_creature_to_hand(P0, "Prior Bystander", 1, 1)
            .with_mana_cost(ManaCost::generic(1))
            .id();
        let blade = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Doom Blade",
                true,
                "Destroy target nonblack creature.",
            )
            .with_mana_cost(doom_blade_cost())
            .id();
        let mut runner = scenario.build();
        fill_pool(&mut runner, 2, 4);

        // The prior spell targets nothing, so it is not a qualifying spell: it pays
        // its own {1} and leaves the gate satisfied.
        let before = runner.state().players[0].mana_pool.total();
        runner.cast(prior).resolve();
        assert_eq!(
            before - runner.state().players[0].mana_pool.total(),
            1,
            "the prior vanilla creature spell pays its own {{1}} (the discount never applies to it)"
        );

        let spent = cast_and_measure(&mut runner, blade, victim_a);
        assert_eq!(
            spent, 1,
            "a prior NON-targeting spell must not spend the discount: the first creature-targeting spell still pays {{1}} (pre-fix: the gate counted that prior spell and this paid {{2}})"
        );
    }

    // Leg 2: a prior targeting spell does spend the discount.
    {
        let mut leg2 = head_of_the_class_board();
        fill_pool(&mut leg2.runner, 2, 4);
        let first = cast_and_measure(&mut leg2.runner, leg2.blade_a, leg2.victim_a);
        assert_eq!(
            first, 1,
            "the first creature-targeting spell of the turn is the one that gets the discount"
        );
        let second = cast_and_measure(&mut leg2.runner, leg2.blade_b, leg2.victim_b);
        assert_eq!(
            second, 2,
            "a prior TARGETING spell does spend the discount: the second one pays full {{1}}{{B}}"
        );
    }
}
