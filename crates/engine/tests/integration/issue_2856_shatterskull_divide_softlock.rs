//! Issue #2856 — Shatterskull Smashing: divided-damage softlock when X is
//! smaller than the printed target cap.
//!
//! Oracle: "Shatterskull Smashing deals X damage divided as you choose among up
//! to two target creatures and/or planeswalkers."
//!
//! With X = 1 and the printed "up to two" target cap, the controller must NOT be
//! offered two target slots — CR 601.2d requires each chosen target to receive
//! at least one of the divided damage, so with only one damage to divide the
//! effective target ceiling collapses to one (CR 601.2c: a divided spell's
//! target count is bounded by the amount to divide). The pre-fix engine offered
//! two slots regardless of X, leaving the player to pick two targets and then
//! deadlock at `DistributeAmong` (1 damage, two targets, each needs ≥1).

use super::rules::{GameRunner, GameScenario, Phase, WaitingFor, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, DistributionUnit};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

const SHATTERSKULL_ORACLE: &str = "Shatterskull Smashing deals X damage divided as you choose among up to two target creatures and/or planeswalkers. If X is 6 or more, Shatterskull Smashing deals twice X damage divided as you choose among them instead.";

fn red_pool(amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

/// The number of target slots a divided-damage spell offers must be bounded by
/// the announced damage pool (X = 1 ⇒ at most one slot), not the printed
/// "up to two" cap. Otherwise the controller can pick two targets and softlock.
#[test]
fn shatterskull_x1_offers_single_target_slot_no_softlock() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let goblin = scenario.add_creature(P1, "Goblin A", 1, 1).id();
    let _ogre = scenario.add_creature(P1, "Ogre B", 3, 3).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Shatterskull Smashing", false, SHATTERSKULL_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Red, ManaCostShard::Red],
            generic: 0,
        })
        .id();
    let card_id = CardId(spell.0);

    // {X}{R}{R} at X=1 → three red mana.
    scenario.with_mana_pool(P0, red_pool(3));

    let mut runner = scenario.build();

    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast announcement should succeed");

    // CR 601.2b/f: X for a divided spell whose target count is bounded by the
    // pool must be announced before targets are chosen.
    drive_choose_x(&mut runner, 1);

    // CR 601.2c/d: with X = 1 only a single target slot may be offered.
    let slot_count = single_target_slot_count(&runner);
    assert_eq!(
        slot_count, 1,
        "X=1 divided among 'up to two' targets must offer exactly one slot (issue #2856 softlock)"
    );

    // Choosing the one allowed target then resolves to a single DistributeAmong
    // (or auto-resolves) without deadlock.
    drive_single_target(&mut runner, goblin);
    let _ = _ogre;

    // The pipeline must reach a DistributeAmong over exactly one target, or have
    // already auto-distributed — never a two-target deadlock.
    if let WaitingFor::DistributeAmong {
        targets,
        total,
        unit,
        ..
    } = runner.state().waiting_for.clone()
    {
        assert_eq!(total, 1, "damage pool equals X=1");
        assert_eq!(unit, DistributionUnit::Damage);
        assert_eq!(targets.len(), 1, "exactly one target to divide among");
        runner
            .act(GameAction::DistributeAmong {
                distribution: targets.iter().map(|t| (t.clone(), 1)).collect(),
            })
            .expect("single-target distribution must be legal");
    }

    runner.advance_until_stack_empty();

    // 1 damage landed on the single chosen creature.
    let dmg = runner
        .state()
        .objects
        .get(&goblin)
        .map(|o| o.damage_marked)
        .unwrap_or(0);
    assert_eq!(
        dmg, 1,
        "all X=1 damage assigned to the single chosen target"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DistributeAmong { .. }
        ),
        "must not be stuck waiting on an impossible distribution"
    );
}

fn drive_choose_x(runner: &mut GameRunner, x: u32) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseXValue { .. } => {
                runner
                    .act(GameAction::ChooseX { value: x })
                    .expect("ChooseX should succeed");
                return;
            }
            WaitingFor::ManaPayment { .. } => {
                // Pool-funded auto-pay shouldn't surface here, but pass priority
                // defensively to keep the harness from hanging.
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::TargetSelection { .. } => {
                panic!("X must be announced before targets for a pool-bounded divided spell (issue #2856)");
            }
            _ => return,
        }
    }
    panic!("never reached ChooseXValue");
}

fn single_target_slot_count(runner: &GameRunner) -> usize {
    match &runner.state().waiting_for {
        WaitingFor::TargetSelection { target_slots, .. } => target_slots.len(),
        other => panic!("expected TargetSelection after ChooseX, got {other:?}"),
    }
}

fn drive_single_target(runner: &mut GameRunner, target: ObjectId) {
    let mut guard = 0;
    while matches!(
        runner.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ) {
        guard += 1;
        assert!(guard < 5, "single-slot target selection must terminate");
        runner
            .act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(target)),
            })
            .expect("ChooseTarget should succeed");
    }
}
