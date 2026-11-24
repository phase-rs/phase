//! Production regression for Algorithmic Ferocity's mana-value-filtered hand count.
//!
//! CR 202.3 + CR 608.2h + CR 611.2a + CR 701.14: the spell counts qualifying
//! cards once when its effect resolves, grants the resulting power boost and
//! indestructible until end of turn, then makes the targeted creatures fight.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ALGORITHMIC_FEROCITY: &str = "Until end of turn, target creature you control gains indestructible and gets +1/+0 for each card in your hand with mana value 4 or greater. Then it fights target creature an opponent controls.";

fn generic_mana(count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn assert_empty_stack(runner: &GameRunner) {
    assert!(
        runner.state().stack.is_empty(),
        "expected an empty stack, got {:?}",
        runner.state().stack
    );
}

#[test]
fn algorithmic_ferocity_snapshots_filtered_hand_count_until_end_of_turn() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, generic_mana(8));

    let ferocity = scenario
        .add_spell_to_hand_from_oracle(P0, "Algorithmic Ferocity", false, ALGORITHMIC_FEROCITY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let qualifying_spell = scenario
        .add_spell_to_hand(P0, "Qualifying Instant", true)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    let _mv_five = scenario
        .add_spell_to_hand(P0, "MV5 Filler", false)
        .with_mana_cost(ManaCost::generic(5))
        .id();
    let _mv_three = scenario
        .add_spell_to_hand(P0, "MV3 Filler", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();

    let own = scenario
        .add_creature(P0, "Own Base-Two Creature", 2, 1)
        .id();
    let opponent = scenario.add_creature(P1, "Opponent Creature", 2, 4).id();
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
    }

    let mut runner = scenario.build();
    let outcome = runner
        .cast(ferocity)
        .target_objects(&[own, opponent])
        .resolve();
    assert_empty_stack(&runner);
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::DeclareAttackers { .. }
        ),
        "expected a post-resolution action boundary, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(runner.state().objects[&own].power, Some(4));
    assert!(runner.state().objects[&own].has_keyword(&Keyword::Indestructible));
    assert_eq!(runner.state().objects[&own].damage_marked, 2);
    assert_eq!(runner.state().objects[&opponent].zone, Zone::Graveyard);
    assert_eq!(outcome.state().objects[&own].power, Some(4));

    // Reach the end-step priority window, then commit a real qualifying spell
    // through GameRunner. It removes one qualifying card from hand after
    // Ferocity resolves, while the existing +2 proves the bonus was snapshotted.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("no attackers are legal");
    }
    runner.advance_to_end_step();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("no attackers are legal");
        runner.advance_to_end_step();
    }
    assert_empty_stack(&runner);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected end-step priority, got {:?}",
        runner.state().waiting_for
    );
    for unit in generic_mana(4) {
        runner.state_mut().add_mana_to_pool(P0, unit);
    }
    let committed = runner.cast(qualifying_spell).commit();
    assert!(matches!(
        committed.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        committed.state().objects[&qualifying_spell].zone,
        Zone::Stack
    );
    assert_eq!(committed.state().objects[&own].power, Some(4));
    assert!(committed.state().objects[&own].has_keyword(&Keyword::Indestructible));
    committed.resolve();

    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    assert_empty_stack(&runner);
    assert_eq!(runner.state().objects[&own].power, Some(2));
    assert!(!runner.state().objects[&own].has_keyword(&Keyword::Indestructible));
}
