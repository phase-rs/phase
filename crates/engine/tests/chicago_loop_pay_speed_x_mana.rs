//! Chicago Loop (#4140 deferred type 2), ability #2:
//! `Pay X speed: Add X mana in any combination of colors.`
//!
//! This is a mana ability whose activation cost is `PaySpeed { amount: Ref(X) }`
//! and whose production is `AnyCombination { count: Ref(X) }`. The player
//! announces X (bounded above by their current speed, CR 702.179e/f), and that
//! single value is bound to BOTH the speed cost and the produced-mana count
//! (CR 107.3a/.3c).
//!
//! Drives the real activation pipeline:
//!   ActivateAbility → PayAmountChoice{resource: Speed} (announce X)
//!     → SubmitPayAmount → ChooseManaColor{AnyCombination count=X}
//!       → X mana produced + X speed paid.
//!
//! CR references (verified against docs/MagicCompRules.txt):
//!   - CR 107.3a: controller announces X for an activation cost with X.
//!   - CR 107.3c: once chosen, X is fixed for both cost and effect.
//!   - CR 702.179e: max speed bounds the declaration; CR 702.179f: no speed = 0.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::ability::{AbilityCost, QuantityExpr, QuantityRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, ManaChoice, PayableResource, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const CHICAGO_LOOP_PAY_SPEED: &str = "Pay X speed: Add X mana in any combination of colors.";

fn pay_speed_index(state: &GameState, id: ObjectId) -> usize {
    let obj = state.objects.get(&id).expect("Chicago Loop exists");
    obj.abilities
        .iter()
        .position(|a| matches!(&a.cost, Some(cost) if cost_is_pay_speed_x(cost)))
        .expect("Chicago Loop has a Pay X speed mana ability")
}

fn cost_is_pay_speed_x(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::PaySpeed {
            amount:
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable { name },
                },
        } => name == "X",
        AbilityCost::Composite { costs } => costs.iter().any(cost_is_pay_speed_x),
        _ => false,
    }
}

fn set_speed(runner: &mut GameRunner, player: PlayerId, speed: u8) {
    runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .speed = Some(speed);
}

/// Chicago Loop is a land, not a creature. The scenario creature helper is used
/// only to parse the ability onto a battlefield permanent; convert it to a pure
/// land and clear P/T so the 0/0 stub is not destroyed as an SBA (CR 704.5f)
/// after the first activation resolves (which would strand the second
/// activation with "object not on battlefield").
fn make_land(runner: &mut GameRunner, id: ObjectId) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![engine::types::card_type::CoreType::Land];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
}

fn build_loop(speed: u8) -> (GameRunner, ObjectId, usize) {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);
    // Chicago Loop is a land; the creature helper is used only to parse the
    // ability onto a battlefield permanent. The `Pay X speed` ability has no
    // {T} component, so summoning-sickness is irrelevant.
    let loop_id = scenario
        .add_creature_from_oracle(P0, "Chicago Loop", 0, 0, CHICAGO_LOOP_PAY_SPEED)
        .id();
    let mut runner = scenario.build();
    make_land(&mut runner, loop_id);
    set_speed(&mut runner, P0, speed);
    let idx = pay_speed_index(runner.state(), loop_id);
    (runner, loop_id, idx)
}

#[test]
fn chicago_loop_pay_speed_x_produces_x_mana_and_pays_x_speed() {
    let (mut runner, loop_id, idx) = build_loop(2);

    runner
        .act(GameAction::ActivateAbility {
            source_id: loop_id,
            ability_index: idx,
        })
        .expect("activation accepted");

    // CR 107.3a + CR 702.179e: announce X, bounded by speed.
    let max = match runner.state().waiting_for.clone() {
        WaitingFor::PayAmountChoice {
            resource: PayableResource::Speed,
            min,
            max,
            ..
        } => {
            assert_eq!(min, 0, "X may be 0");
            max
        }
        other => panic!("Expected PayAmountChoice{{Speed}}, got {other:?}"),
    };
    assert_eq!(max, 2, "max X bounded by current speed 2");

    // Announce X = 2.
    runner
        .act(GameAction::SubmitPayAmount { amount: 2 })
        .expect("X announcement accepted");

    // CR 605.3b + CR 107.3c: production prompt for X=2 colors.
    let count = match runner.state().waiting_for.clone() {
        WaitingFor::ChooseManaColor { choice, .. } => match choice {
            engine::types::game_state::ManaChoicePrompt::AnyCombination { count, .. } => count,
            other => panic!("Expected AnyCombination prompt, got {other:?}"),
        },
        other => panic!("Expected ChooseManaColor, got {other:?}"),
    };
    assert_eq!(count, 2, "production count bound to announced X=2");

    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Red, ManaType::Blue]),
            count: 1,
        })
        .expect("combination accepted");

    // CR 107.3c: X=2 bound to BOTH axes.
    assert_eq!(
        runner.state().players[0].speed,
        Some(0),
        "speed reduced by 2 (2 → 0)"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        2,
        "exactly 2 mana produced"
    );
}

#[test]
fn chicago_loop_pay_speed_zero_when_no_speed() {
    let (mut runner, loop_id, idx) = build_loop(0);

    runner
        .act(GameAction::ActivateAbility {
            source_id: loop_id,
            ability_index: idx,
        })
        .expect("activation accepted");

    // CR 702.179f: no speed → max X = 0; only X=0 is legal.
    match runner.state().waiting_for.clone() {
        WaitingFor::PayAmountChoice {
            resource: PayableResource::Speed,
            min,
            max,
            ..
        } => {
            assert_eq!(min, 0);
            assert_eq!(max, 0, "no speed → X bounded to 0");
        }
        other => panic!("Expected PayAmountChoice{{Speed}}, got {other:?}"),
    }

    runner
        .act(GameAction::SubmitPayAmount { amount: 0 })
        .expect("X=0 accepted");

    // X=0 → AnyCombination count 0 → no color prompt; activation completes with
    // 0 mana produced and 0 speed paid (no panic).
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        0,
        "X=0 produces no mana"
    );
    assert_eq!(runner.state().players[0].speed, Some(0), "no speed paid");
}

#[test]
fn chicago_loop_pay_speed_x_is_per_activation() {
    // X is per-PendingManaAbility, not a global — two sequential activations with
    // different X produce different amounts.
    let (mut runner, loop_id, idx) = build_loop(4);

    // First activation: X = 1.
    runner
        .act(GameAction::ActivateAbility {
            source_id: loop_id,
            ability_index: idx,
        })
        .expect("activate 1");
    runner
        .act(GameAction::SubmitPayAmount { amount: 1 })
        .expect("X=1");
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Green]),
            count: 1,
        })
        .expect("1 color");
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        1,
        "first activation: 1 mana"
    );
    assert_eq!(runner.state().players[0].speed, Some(3), "speed 4 → 3");

    // Second activation: X = 3.
    runner
        .act(GameAction::ActivateAbility {
            source_id: loop_id,
            ability_index: idx,
        })
        .expect("activate 2");
    runner
        .act(GameAction::SubmitPayAmount { amount: 3 })
        .expect("X=3");
    runner
        .act(GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(vec![ManaType::Red, ManaType::White, ManaType::Black]),
            count: 1,
        })
        .expect("3 colors");
    assert_eq!(
        runner.state().players[0].mana_pool.mana.len(),
        4,
        "second activation adds 3 more (1 + 3 = 4 total)"
    );
    assert_eq!(runner.state().players[0].speed, Some(0), "speed 3 → 0");
}
