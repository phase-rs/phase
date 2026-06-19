//! Issue #3681: Inferno Titan's "Whenever this creature enters or attacks, it
//! deals 3 damage divided as you choose among one, two, or three targets"
//! trigger must let the controller choose up to three targets and split the 3
//! damage among them.
//!
//! The engine surfaces a `TriggerTargetSelection` with three slots (one
//! required, two optional) and then a `DistributeAmong { total: 3 }` step. This
//! test drives the full ETB flow end-to-end through the real parser + runtime:
//! cast Inferno Titan, choose three distinct targets (two creatures and a
//! player), distribute 1/1/1, and assert each target takes exactly its share.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;

/// Canonical Oracle text (verified against client/public/card-data.json).
const INFERNO_ORACLE: &str =
    "Whenever this creature enters or attacks, it deals 3 damage divided as you choose among one, two, or three targets.";

/// Give `player` `count` red mana units so the {4}{R}{R} cast auto-pays.
fn add_red_mana(
    runner: &mut engine::game::scenario::GameRunner,
    player: engine::types::PlayerId,
    count: usize,
) {
    let dummy = engine::types::identifiers::ObjectId(0);
    let pool = &mut runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(ManaType::Red, dummy, false, vec![]));
    }
}

/// Advance through the cast/payment flow until the ETB trigger surfaces its
/// target-selection prompt, passing priority as needed.
fn advance_to_trigger_target_selection(
    runner: &mut engine::game::scenario::GameRunner,
) -> WaitingFor {
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(
            guard < 80,
            "Inferno Titan ETB trigger never surfaced a target prompt; last waiting_for = {:?}",
            runner.state().waiting_for
        );
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { .. } => {
                return runner.state().waiting_for.clone();
            }
            WaitingFor::Priority { .. } => {
                runner.pass_both_players();
            }
            other => {
                // Any other interactive state is unexpected for this scenario.
                panic!("unexpected waiting_for while reaching ETB trigger: {other:?}");
            }
        }
    }
}

#[test]
fn inferno_titan_etb_divides_damage_across_three_targets() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Two legal creature targets controlled by the opponent, plus the opponent
    // player themselves — three distinct "any target" choices.
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let elf = scenario.add_creature(P1, "Elf", 1, 1).id();

    let titan = scenario
        .add_creature_to_hand_from_oracle(P0, "Inferno Titan", 6, 6, INFERNO_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 4,
        })
        .id();

    let mut runner = scenario.build();
    add_red_mana(&mut runner, P0, 8);

    // Begin casting Inferno Titan (auto-pay the {4}{R}{R}).
    let card_id = runner.state().objects[&titan].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: titan,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Inferno Titan should be accepted");

    // Reach the ETB trigger's target-selection prompt.
    let waiting_for = advance_to_trigger_target_selection(&mut runner);
    let WaitingFor::TriggerTargetSelection { target_slots, .. } = &waiting_for else {
        panic!("expected TriggerTargetSelection, got {waiting_for:?}");
    };

    // CR 601.2d + CR 603.3d: "among one, two, or three targets" surfaces exactly
    // three slots — one required, two optional — so the controller may divide
    // among up to three targets.
    assert_eq!(
        target_slots.len(),
        3,
        "Inferno Titan must offer three target slots (1 required + 2 optional)"
    );
    assert!(
        !target_slots[0].optional,
        "first slot must be required (minimum one target)"
    );
    assert!(target_slots[1].optional, "second slot must be optional");
    assert!(target_slots[2].optional, "third slot must be optional");

    // Choose all three distinct targets in one selection.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![
                TargetRef::Object(bear),
                TargetRef::Object(elf),
                TargetRef::Player(P1),
            ],
        })
        .expect("selecting three distinct targets should be accepted");

    // After targets are chosen, the engine must prompt for the division.
    match runner.state().waiting_for.clone() {
        WaitingFor::DistributeAmong { total, targets, .. } => {
            assert_eq!(total, 3, "damage pool to divide must be 3");
            assert_eq!(
                targets.len(),
                3,
                "all three chosen targets must participate in the distribution"
            );
        }
        other => panic!("expected DistributeAmong after target selection, got {other:?}"),
    }

    // Distribute 1 / 1 / 1 across the three targets.
    runner
        .act(GameAction::DistributeAmong {
            distribution: vec![
                (TargetRef::Object(bear), 1),
                (TargetRef::Object(elf), 1),
                (TargetRef::Player(P1), 1),
            ],
        })
        .expect("1/1/1 distribution should be accepted");

    // Let the triggered ability resolve.
    runner.advance_until_stack_empty();

    // CR 120.3: each target takes exactly its assigned portion.
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        1,
        "Bear must take 1 damage"
    );
    assert_eq!(
        runner.state().objects[&elf].damage_marked,
        1,
        "Elf must take 1 damage"
    );
    let p1_life = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .map(|p| p.life)
        .expect("P1 must exist");
    assert_eq!(p1_life, 19, "opponent must lose 1 life (20 - 1 damage)");
}

/// CR 601.2d: the controller may also choose a SINGLE target and assign the
/// full 3 damage to it. This locks the "one target" branch of the divided-damage
/// trigger against regressions.
#[test]
fn inferno_titan_etb_can_assign_all_damage_to_one_target() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let bear = scenario.add_creature(P1, "Bear", 0, 5).id();

    let titan = scenario
        .add_creature_to_hand_from_oracle(P0, "Inferno Titan", 6, 6, INFERNO_ORACLE)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
            generic: 4,
        })
        .id();

    let mut runner = scenario.build();
    add_red_mana(&mut runner, P0, 8);

    let card_id = runner.state().objects[&titan].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: titan,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Inferno Titan should be accepted");

    advance_to_trigger_target_selection(&mut runner);

    // Choose only the one required target, skipping the two optional slots.
    runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(bear)],
        })
        .expect("selecting a single target should be accepted");

    // With a single target the engine assigns the full pool (no division prompt).
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        3,
        "single target must take the full 3 damage"
    );
}
