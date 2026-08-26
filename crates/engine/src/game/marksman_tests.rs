//! Production-path tests for repeated optional payments and their reflexive modal.

#![cfg(test)]

use crate::game::ability_utils::build_resolved_from_def;
use crate::game::effects::resolve_ability_chain;
use crate::game::scenario::GameScenario;
use crate::parser::oracle::{parse_oracle_text, ParsedAbilities};
use crate::types::ability::{AbilityCondition, Effect, QuantityExpr, QuantityRef, TargetRef};
use crate::types::actions::GameAction;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::mana::{ManaType, ManaUnit};
use crate::types::player::PlayerId;
use crate::types::resolution::ResolutionStateWire;
use crate::types::triggers::TriggerMode;

const HAWKEYE_ORACLE: &str = "First strike, reach\n\
     Trick Arrows — Whenever Hawkeye becomes tapped, you may pay {1} up to three times. \
     When you do, choose up to that many.\n\
     • Net — Target creature can't block this turn.\n\
     • Explosive — Hawkeye deals 2 damage to target player.\n\
     • Boomerang — Discard a card, then draw a card.";
const FRILLBACK_ORACLE: &str = "When this creature enters, you may pay {G} up to three times. \
     When you pay this cost one or more times, choose up to that many —\n\
     • Destroy target artifact or enchantment.\n\
     • Exile target player's graveyard.\n\
     • You gain 4 life.";
const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

fn parse_hawkeye() -> ParsedAbilities {
    parse_oracle_text(
        HAWKEYE_ORACLE,
        "Hawkeye, Master Marksman",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[],
    )
}

fn hawkeye_runtime(mana: usize, hand: &[&str]) -> crate::game::scenario::GameRunner {
    let mut scenario = GameScenario::new();
    scenario.with_life(P1, 20);
    if !hand.is_empty() {
        scenario.with_cards_in_hand(P0, hand);
    }
    let hawkeye = scenario
        .add_creature_from_oracle(P0, "Hawkeye, Master Marksman", 3, 3, HAWKEYE_ORACLE)
        .id();
    if mana > 0 {
        scenario.with_mana_pool(
            P0,
            vec![
                ManaUnit::new(
                    ManaType::Colorless,
                    crate::types::identifiers::ObjectId(9_999),
                    false,
                    vec![],
                );
                mana
            ],
        );
    }
    let mut runner = scenario.build();
    let parsed = parse_hawkeye();
    let execute = parsed
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::Taps)
        .and_then(|trigger| trigger.execute.as_ref())
        .expect("Hawkeye Taps trigger has an execute");
    let resolved = build_resolved_from_def(execute, hawkeye, P0);
    resolve_ability_chain(runner.state_mut(), &resolved, &mut Vec::new(), 0)
        .expect("resolve Hawkeye trigger");
    runner
}

fn decide(runner: &mut crate::game::scenario::GameRunner, accept: bool) {
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .expect("optional-payment choice accepted");
}

fn payment_count(state: &GameState) -> u32 {
    state
        .active_repeated_optional_payment_frame()
        .map_or(0, |frame| frame.optional_cost_payments_this_resolution)
}

fn modal_cap(state: &GameState) -> Option<(usize, usize)> {
    match &state.waiting_for {
        WaitingFor::AbilityModeChoice { modal, .. } => Some((modal.min_choices, modal.max_choices)),
        _ => None,
    }
}

#[test]
fn parser_preserves_the_dynamic_reflexive_modal_cap() {
    let hawkeye = parse_hawkeye();
    let trigger = hawkeye
        .triggers
        .iter()
        .find(|trigger| trigger.mode == TriggerMode::Taps)
        .expect("Hawkeye Taps trigger");
    let modal = trigger
        .execute
        .as_ref()
        .and_then(|execute| execute.sub_ability.as_ref())
        .and_then(|reflexive| reflexive.modal.as_ref())
        .expect("reflexive modal");
    assert_eq!(modal.min_choices, 0);
    assert_eq!(modal.mode_count, 3);
    assert_eq!(
        modal.dynamic_max_choices,
        Some(QuantityExpr::Ref {
            qty: QuantityRef::TimesCostPaidThisResolution
        })
    );
}

#[test]
fn tranquil_frillback_uses_the_sequential_payment_flow() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 20);
    let frillback = scenario
        .add_creature_from_oracle(P0, "Tranquil Frillback", 3, 3, FRILLBACK_ORACLE)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Green,
            crate::types::identifiers::ObjectId(9_999),
            false,
            vec![],
        )],
    );
    let mut runner = scenario.build();
    let parsed = parse_oracle_text(
        FRILLBACK_ORACLE,
        "Tranquil Frillback",
        &[],
        &["Creature".to_string()],
        &["Dinosaur".to_string()],
    );
    let execute = parsed.triggers[0].execute.as_deref().expect("ETB execute");
    assert!(matches!(execute.effect.as_ref(), Effect::PayCost { .. }));
    assert!(execute.optional);
    assert_eq!(execute.repeat_for, Some(QuantityExpr::Fixed { value: 3 }));
    assert_eq!(
        execute
            .sub_ability
            .as_deref()
            .and_then(|reflexive| reflexive.condition.as_ref()),
        Some(&AbilityCondition::WhenYouDo)
    );
    resolve_ability_chain(
        runner.state_mut(),
        &build_resolved_from_def(execute, frillback, P0),
        &mut Vec::new(),
        0,
    )
    .expect("resolve Frillback ETB");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    decide(&mut runner, true);
    decide(&mut runner, false);
    assert_eq!(modal_cap(runner.state()), Some((0, 1)));
    runner
        .act(GameAction::SelectModes { indices: vec![2] })
        .expect("select life mode");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].life, 24);
}

#[test]
fn payments_are_offered_before_the_reflexive_modal_and_k_caps_it() {
    let mut runner = hawkeye_runtime(3, &[]);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    decide(&mut runner, true);
    decide(&mut runner, true);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
    decide(&mut runner, true);
    assert_eq!(payment_count(runner.state()), 3);
    assert_eq!(modal_cap(runner.state()), Some((0, 3)));
}

#[test]
fn failed_later_payment_preserves_earlier_successes_and_triggers_once() {
    let mut runner = hawkeye_runtime(1, &[]);
    decide(&mut runner, true);
    decide(&mut runner, true);
    assert_eq!(payment_count(runner.state()), 1);
    assert_eq!(modal_cap(runner.state()), Some((0, 1)));
}

#[test]
fn successful_payments_then_normal_trigger_targeting_resolve_once() {
    let mut runner = hawkeye_runtime(2, &[]);
    decide(&mut runner, true);
    decide(&mut runner, true);
    decide(&mut runner, false);
    assert_eq!(modal_cap(runner.state()), Some((0, 2)));
    runner
        .act(GameAction::SelectModes { indices: vec![1] })
        .expect("choose Explosive after its reflexive trigger is on the stack");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::TriggerTargetSelection { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose Explosive target");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[1].life, 18);
}

#[test]
fn sequential_payment_snapshot_round_trips_mid_loop() {
    let mut runner = hawkeye_runtime(3, &[]);
    decide(&mut runner, true);
    decide(&mut runner, true);
    let wire = serde_json::to_value(ResolutionStateWire::from_game_state(runner.state().clone()))
        .expect("sequential payment prompt serializes");
    let restored = serde_json::from_value::<ResolutionStateWire>(wire)
        .expect("sequential payment prompt restores")
        .into_game_state();
    assert_eq!(payment_count(&restored), 2);
    assert!(matches!(
        restored.waiting_for,
        WaitingFor::OptionalEffectChoice { .. }
    ));
}
