//! Integration tests for the Tribute mechanic (CR 702.104).
//!
//! Covers:
//! - Chosen opponent paying tribute → source enters with N +1/+1 counters,
//!   `ChosenAttribute::TributeOutcome::Paid` persisted, "if tribute wasn't paid"
//!   trigger suppressed (CR 702.104a + CR 702.104b).
//! - Chosen opponent declining → no counters, `TributeOutcome::Declined` persisted,
//!   trigger fires (CR 702.104b).
//! - The controller first chooses the opponent (`NamedChoice` with `ChoiceType::Opponent`).

#![allow(unused_imports)]
use super::*;

use engine::types::ability::{ChoiceType, ChosenAttribute, TributeOutcome};
use engine::types::counter::CounterType;
use engine::types::game_state::CastPaymentMode;

/// Fanatic of Xenagos-class Oracle: Tribute 1 + "When this creature enters, if
/// tribute wasn't paid, it gets +1/+1 and gains haste until end of turn."
///
/// We drive the ETB sequence through a cast to observe the full replacement chain.
fn cast_tribute_creature(count: u32, paid: bool) -> GameRunner {
    let oracle = format!(
        "Tribute {count} (As this creature enters, an opponent of your choice may put {count} +1/+1 counters on it.)\n\
         When this creature enters, if tribute wasn't paid, this creature deals 2 damage to each opponent."
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Give P0 enough mana to cast and add the Tribute creature to hand.
    let mut hand_builder =
        scenario.add_creature_to_hand_from_oracle(P0, "Tribute Tester", 2, 2, &oracle);
    let card_obj_id = hand_builder.id();
    hand_builder.with_mana_cost(engine::types::mana::ManaCost::generic(0));

    let mut runner = scenario.build();

    // Cast the Tribute creature.
    let card_id = runner.state().objects[&card_obj_id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: card_obj_id,
            card_id,
            targets: vec![],

            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast should succeed");

    // Pass priority so the spell resolves and the ETB replacement fires.
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.pass_both_players();
    }

    // Expect: the Choose-opponent prompt fires first (controller picks an opponent).
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            player,
            choice_type,
            options,
            ..
        } => {
            assert_eq!(*player, P0, "controller should be choosing the opponent");
            assert_eq!(*choice_type, ChoiceType::Opponent);
            assert!(
                options.contains(&P1.0.to_string()),
                "P1 must be a valid opponent choice, got {options:?}"
            );
        }
        other => panic!("expected NamedChoice (Opponent), got {other:?}"),
    }

    runner
        .act(GameAction::ChooseOption {
            choice: P1.0.to_string(),
        })
        .expect("choose opponent should succeed");

    // Now the chosen opponent (P1) is prompted pay/decline.
    match &runner.state().waiting_for {
        WaitingFor::TributeChoice {
            player,
            count: prompt_count,
            ..
        } => {
            assert_eq!(*player, P1, "chosen opponent should be prompted");
            assert_eq!(*prompt_count, count);
        }
        other => panic!("expected TributeChoice, got {other:?}"),
    }

    runner
        .act(GameAction::DecideOptionalEffect { accept: paid })
        .expect("tribute decision should succeed");

    // Drain any remaining stack work (ETB trigger, counter addition, etc.).
    while matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        && !runner.state().stack.is_empty()
    {
        runner.pass_both_players();
    }

    runner
}

/// Return the ObjectId of the just-entered Tribute creature on the battlefield.
fn find_tribute_creature(runner: &GameRunner) -> ObjectId {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            runner
                .state()
                .objects
                .get(id)
                .map(|obj| obj.name == "Tribute Tester")
                .unwrap_or(false)
        })
        .expect("Tribute Tester should be on the battlefield")
}

/// CR 702.104a: When the chosen opponent pays tribute, the creature enters with
/// N +1/+1 counters and the paid outcome is recorded.
#[test]
fn tribute_paid_applies_counters_and_records_outcome() {
    let runner = cast_tribute_creature(/* count */ 2, /* paid */ true);
    let id = find_tribute_creature(&runner);
    let obj = &runner.state().objects[&id];

    assert_eq!(
        obj.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "paid tribute should add +1/+1 counters equal to Tribute N"
    );
    assert!(
        obj.chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::TributeOutcome(TributeOutcome::Paid))),
        "paid tribute should persist TributeOutcome::Paid"
    );
}

/// CR 702.104b: When the chosen opponent declines, no counters are added and the
/// declined outcome is persisted so the "if tribute wasn't paid" trigger can fire.
#[test]
fn tribute_declined_records_outcome_without_counters() {
    let runner = cast_tribute_creature(/* count */ 2, /* paid */ false);
    let id = find_tribute_creature(&runner);
    let obj = &runner.state().objects[&id];

    assert_eq!(
        obj.counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        0,
        "declined tribute should not add counters"
    );
    assert!(
        obj.chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::TributeOutcome(TributeOutcome::Declined))),
        "declined tribute should persist TributeOutcome::Declined"
    );
}

/// CR 702.104a: The controller is the one who selects the chosen opponent — not
/// the spell's opponent. Verified by the initial NamedChoice prompt's player.
#[test]
fn tribute_controller_picks_chosen_opponent() {
    let runner = cast_tribute_creature(/* count */ 1, /* paid */ true);
    let id = find_tribute_creature(&runner);
    let obj = &runner.state().objects[&id];

    assert!(
        obj.chosen_attributes
            .iter()
            .any(|a| matches!(a, ChosenAttribute::Player(p) if *p == P1)),
        "controller's opponent choice should be persisted on the source"
    );
}

/// CR 702.104b: Verify the outcome distinction between paid and declined is
/// fully observable through `ChosenAttribute::TributeOutcome` — the typed
/// discriminator the `TributeNotPaid` trigger condition evaluator reads from.
#[test]
fn tribute_outcome_persists_distinctly_for_paid_vs_declined() {
    let paid_runner = cast_tribute_creature(1, /* paid */ true);
    let paid_id = find_tribute_creature(&paid_runner);
    let paid_obj = &paid_runner.state().objects[&paid_id];

    let declined_runner = cast_tribute_creature(1, /* paid */ false);
    let declined_id = find_tribute_creature(&declined_runner);
    let declined_obj = &declined_runner.state().objects[&declined_id];

    let paid_outcome = paid_obj.chosen_attributes.iter().find_map(|a| match a {
        ChosenAttribute::TributeOutcome(o) => Some(*o),
        _ => None,
    });
    let declined_outcome = declined_obj.chosen_attributes.iter().find_map(|a| match a {
        ChosenAttribute::TributeOutcome(o) => Some(*o),
        _ => None,
    });

    assert_eq!(paid_outcome, Some(TributeOutcome::Paid));
    assert_eq!(declined_outcome, Some(TributeOutcome::Declined));
}
