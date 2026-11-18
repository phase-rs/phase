//! Regression tests for GitHub issue #10269: Fangorn, Tree Shepherd's
//! "Whenever one or more Treefolk you control attack, add twice that much {G}."
//! must add 2N green mana for N attacking Treefolk.
//!
//! CR 603.2c: the batched trigger fires once; its event-context amount is the
//! number of attacking subjects that matched the Treefolk-you-control filter.
//! CR 608.2h: the effect doubles the stored count when it resolves.
//! CR 106.4: the mana goes into the controller's mana pool.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const FANGORN_ORACLE: &str = "Treefolk you control have vigilance.\n\
Whenever one or more Treefolk you control attack, add twice that much {G}.\n\
You don't lose unspent green mana as steps and phases end.";

fn add_fangorn(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Fangorn, Tree Shepherd", 4, 10, FANGORN_ORACLE)
        .with_subtypes(vec!["Treefolk"])
        .id()
}

fn add_treefolk(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature(P0, name, 2, 2)
        .with_subtypes(vec!["Treefolk"])
        .id()
}

fn add_soldier(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature(P0, "Lowly Soldier", 1, 1)
        .with_subtypes(vec!["Soldier"])
        .id()
}

fn green_mana(runner: &GameRunner) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .expect("P0 exists")
        .mana_pool
        .count_color(ManaType::Green)
}

/// Declare the attackers and resolve the attack trigger, stopping before
/// blockers so the assertion isolates mana production.
fn declare_and_resolve(runner: &mut GameRunner, fangorn: ObjectId, attackers: &[ObjectId]) {
    runner.pass_both_players();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&id| (id, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("DeclareAttackers should succeed");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert_eq!(
        runner.state().stack.iter().filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { source_id, .. } if source_id == fangorn)
        }).count(),
        1,
        "one Fangorn trigger represents the entire attack declaration"
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "Fangorn's trigger must resolve"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// CR 603.2c + CR 608.2h: one attacking Treefolk (Fangorn itself) → 2 green.
#[test]
fn fangorn_one_attacking_treefolk_adds_two_green() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fangorn = add_fangorn(&mut scenario);

    let mut runner = scenario.build();
    assert_eq!(green_mana(&runner), 0, "precondition: empty mana pool");

    declare_and_resolve(&mut runner, fangorn, &[fangorn]);

    assert_eq!(
        green_mana(&runner),
        2,
        "CR 603.2c + CR 608.2h: one Treefolk attack → 2 green"
    );
}

/// CR 603.2c: three Treefolk + a non-Treefolk attacker → the trigger counts
/// only the three Treefolk, then doubles → 6 green (not 8).
#[test]
fn fangorn_three_attacking_treefolk_add_six_green_and_exclude_non_treefolk() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let fangorn = add_fangorn(&mut scenario);
    let t1 = add_treefolk(&mut scenario, "Treefolk A");
    let t2 = add_treefolk(&mut scenario, "Treefolk B");
    let soldier = add_soldier(&mut scenario);

    let mut runner = scenario.build();
    declare_and_resolve(&mut runner, fangorn, &[fangorn, t1, t2, soldier]);

    assert_eq!(
        green_mana(&runner),
        6,
        "CR 603.2c: only the three Treefolk count; twice that much → 6 green"
    );
}
