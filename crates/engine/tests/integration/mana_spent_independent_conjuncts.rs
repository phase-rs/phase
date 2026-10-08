//! CR 608.2c: each instruction of a spell is followed in written order and owns
//! its own gate. Two instructions each gated on "if {C} was spent to cast this
//! spell" are independent: a false first gate must not suppress the second.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

fn add_mana(runner: &mut GameRunner, mana_type: ManaType, amount: u32) {
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool;
    for _ in 0..amount {
        pool.add(ManaUnit::new(mana_type, ObjectId(0), false, vec![]));
    }
}

/// Build a synthetic instant with the given Oracle text costing `shards`, plus a
/// library card so a draw is observable.
fn scenario_with_spell(oracle: &str, shards: Vec<ManaCostShard>) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_card_to_library_top(P0, "Filler Card");
    let spell = {
        let mut b = scenario.add_spell_to_hand_from_oracle(P0, "Gated Spell", false, oracle);
        b.with_mana_cost(ManaCost::Cost { shards, generic: 0 });
        b.id()
    };
    (scenario.build(), spell)
}

/// Cast paying exactly the given mana and resolve.
fn cast_paying(
    oracle: &str,
    shards: Vec<ManaCostShard>,
    payment: &[ManaType],
) -> engine::game::scenario::CastOutcome {
    let (mut runner, spell) = scenario_with_spell(oracle, shards);
    assert!(
        runner.state().objects[&spell]
            .abilities
            .iter()
            .all(|a| a.effect.unimplemented_description().is_none()),
        "spell must parse without Unimplemented gaps: {:?}",
        runner.state().objects[&spell].abilities
    );
    for &mana in payment {
        add_mana(&mut runner, mana, 1);
    }
    runner.cast(spell).resolve()
}

/// Comma-joined conjuncts with different effects; only the second color spent.
#[test]
fn comma_and_conjunct_resolves_when_only_second_color_spent() {
    let outcome = cast_paying(
        "You gain 3 life if {R} was spent to cast this spell, and you draw a card if {G} was spent to cast this spell.",
        vec![ManaCostShard::Green],
        &[ManaType::Green],
    );
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, 0);
}

/// Separate-sentence form (each sentence carries its own gate).
#[test]
fn separate_sentence_gates_resolve_when_only_second_color_spent() {
    let outcome = cast_paying(
        "You gain 3 life if {R} was spent to cast this spell. You draw a card if {G} was spent to cast this spell.",
        vec![ManaCostShard::Green],
        &[ManaType::Green],
    );
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, 0);
}

/// Elided-head gain-life form: only the second color spent pays only the
/// second amount.
#[test]
fn gain_life_conjunct_pays_only_the_spent_branch() {
    let outcome = cast_paying(
        "You gain 2 life if {R} was spent to cast this spell and 5 life if {G} was spent to cast this spell.",
        vec![ManaCostShard::Green],
        &[ManaType::Green],
    );
    outcome.assert_life_delta(P0, 5);
}

/// Both colors spent: both independent branches resolve.
#[test]
fn gain_life_conjunct_pays_both_branches_when_both_colors_spent() {
    let outcome = cast_paying(
        "You gain 2 life if {R} was spent to cast this spell and 5 life if {G} was spent to cast this spell.",
        vec![ManaCostShard::Red, ManaCostShard::Green],
        &[ManaType::Red, ManaType::Green],
    );
    outcome.assert_life_delta(P0, 7);
}

/// First color only: the first branch resolves and the second does not.
#[test]
fn first_branch_only_when_only_first_color_spent() {
    let outcome = cast_paying(
        "You gain 2 life if {R} was spent to cast this spell and 5 life if {G} was spent to cast this spell.",
        vec![ManaCostShard::Red],
        &[ManaType::Red],
    );
    outcome.assert_life_delta(P0, 2);
}
