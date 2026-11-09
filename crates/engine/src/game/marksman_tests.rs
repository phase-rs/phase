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

// No printed card yet pairs a repeated payment with a NON-modal reflexive (the
// "any number of times" Adversary cycle is not a fixed-count parse), so these
// fixtures use the Hawkeye/Frillback payment clause with a plain rider.
const PAY_THEN_GAIN_ORACLE: &str =
    "You may pay {1} up to three times. When you do, you gain 3 life.";
const ETB_PAY_THEN_DRAW_ORACLE: &str =
    "When this creature enters, you may pay {1} up to three times. When you do, you draw a card.";

/// Four Forests for P0: one pays the {G} cast cost, the rest are available
/// for the resolution-time {1} payments.
fn non_modal_runtime(
    add_card: impl FnOnce(&mut GameScenario) -> crate::types::identifiers::ObjectId,
) -> (
    crate::game::scenario::GameRunner,
    crate::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
    let card = add_card(&mut scenario);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C"]);
    for _ in 0..4 {
        scenario.add_basic_land(P0, crate::types::mana::ManaColor::Green);
    }
    (scenario.build(), card)
}

fn green_cost() -> crate::types::mana::ManaCost {
    crate::types::mana::ManaCost::Cost {
        generic: 0,
        shards: vec![crate::types::mana::ManaCostShard::Green],
    }
}

/// Pass priority until the resolving object offers its first repeated payment.
fn pass_until_payment_offer(runner: &mut crate::game::scenario::GameRunner) {
    for _ in 0..6 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority toward the payment offer");
    }
    panic!(
        "no repeated-payment offer reached; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

fn tapped_lands(state: &GameState, player: PlayerId) -> usize {
    state
        .battlefield
        .iter()
        .filter(|id| state.objects[id].tapped && state.objects[id].controller == player)
        .count()
}

fn assert_settled(state: &GameState) {
    assert!(state.stack.is_empty(), "stack must be empty");
    assert!(state.deferred_triggers.is_empty(), "no parked reflexive");
    assert!(state.resolution_stack.is_empty(), "no retained frame");
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
}

/// CR 603.12 + CR 603.12a: after two of three offered payments, a non-modal
/// reflexive on a resolving spell triggers exactly once, reaches the stack, and
/// resolves (issue #9555 — it used to stay parked in `deferred_triggers`
/// behind the completed repeated-payment frame).
#[test]
fn spell_non_modal_reflexive_triggers_once_and_resolves() {
    let (mut runner, spell) = non_modal_runtime(|scenario| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Paid Rider", false, PAY_THEN_GAIN_ORACLE)
            .with_mana_cost(green_cost())
            .id()
    });
    runner.cast(spell).commit();
    assert_eq!(tapped_lands(runner.state(), P0), 1, "cast cost paid");
    pass_until_payment_offer(&mut runner);
    decide(&mut runner, true);
    decide(&mut runner, true);
    decide(&mut runner, false);
    assert_eq!(tapped_lands(runner.state(), P0), 3, "two {{1}} payments");
    // Positive reach guard: the reflexive is on the stack, not parked.
    assert!(runner.state().deferred_triggers.is_empty());
    assert_eq!(runner.state().stack.len(), 1, "one reflexive trigger");
    assert_eq!(runner.state().players[0].life, 20, "not yet resolved");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].life, 23, "gained 3 life once");
    assert_settled(runner.state());
}

/// CR 603.12a: paying all three times still triggers the reflexive only once.
#[test]
fn spell_non_modal_reflexive_after_every_payment_triggers_once() {
    let (mut runner, spell) = non_modal_runtime(|scenario| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Paid Rider", false, PAY_THEN_GAIN_ORACLE)
            .with_mana_cost(green_cost())
            .id()
    });
    runner.cast(spell).commit();
    pass_until_payment_offer(&mut runner);
    decide(&mut runner, true);
    decide(&mut runner, true);
    decide(&mut runner, true);
    assert_eq!(tapped_lands(runner.state(), P0), 4, "three {{1}} payments");
    assert_eq!(runner.state().stack.len(), 1, "one reflexive trigger");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[0].life, 23);
    assert_settled(runner.state());
}

/// CR 603.12: declining the first payment means the trigger event never
/// occurred, so the reflexive never triggers and nothing is left parked.
#[test]
fn spell_non_modal_reflexive_without_payment_never_triggers() {
    let (mut runner, spell) = non_modal_runtime(|scenario| {
        scenario
            .add_spell_to_hand_from_oracle(P0, "Paid Rider", false, PAY_THEN_GAIN_ORACLE)
            .with_mana_cost(green_cost())
            .id()
    });
    runner.cast(spell).commit();
    pass_until_payment_offer(&mut runner);
    decide(&mut runner, false);
    runner.advance_until_stack_empty();
    assert_eq!(tapped_lands(runner.state(), P0), 1, "only the cast cost");
    assert_eq!(runner.state().players[0].life, 20);
    assert_settled(runner.state());
}

/// CR 603.12a: the same reflexive created by a resolving ETB trigger (not a
/// spell) also reaches the stack once and resolves.
#[test]
fn etb_trigger_non_modal_reflexive_triggers_once_and_resolves() {
    let (mut runner, creature) = non_modal_runtime(|scenario| {
        scenario
            .add_creature_to_hand_from_oracle(
                P0,
                "Paid Rider Beast",
                2,
                2,
                ETB_PAY_THEN_DRAW_ORACLE,
            )
            .with_mana_cost(green_cost())
            .id()
    });
    runner.cast(creature).commit();
    let hand_after_cast = runner.state().players[0].hand.len();
    pass_until_payment_offer(&mut runner);
    decide(&mut runner, true);
    decide(&mut runner, false);
    assert_eq!(tapped_lands(runner.state(), P0), 2, "one {{1}} payment");
    assert_eq!(runner.state().stack.len(), 1, "one reflexive trigger");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_after_cast + 1,
        "drew exactly one card"
    );
    assert_settled(runner.state());
}

/// CR 603.12 + CR 603.3d: a TARGETED non-modal reflexive is put on the stack
/// directly and asks for its target; releasing the completed payment frame
/// while that prompt is open must not disturb the target choice or resolution.
#[test]
fn spell_targeted_non_modal_reflexive_chooses_target_and_resolves() {
    let (mut runner, spell) = non_modal_runtime(|scenario| {
        scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Paid Rider",
                false,
                "You may pay {1} up to three times. When you do, target player gains 3 life.",
            )
            .with_mana_cost(green_cost())
            .id()
    });
    runner.cast(spell).commit();
    pass_until_payment_offer(&mut runner);
    decide(&mut runner, true);
    decide(&mut runner, false);
    assert_eq!(tapped_lands(runner.state(), P0), 2, "one {{1}} payment");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "reflexive asks for its target; got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("choose the reflexive's target");
    assert_eq!(runner.state().stack.len(), 1, "one reflexive trigger");
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().players[1].life, 23, "target gained 3 once");
    assert_eq!(runner.state().players[0].life, 20);
    assert_settled(runner.state());
}
