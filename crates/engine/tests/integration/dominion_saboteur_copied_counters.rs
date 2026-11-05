//! Dominion Saboteur: "You may have this creature enter as a copy of any
//! artifact or creature on the battlefield, except it isn't legendary. If you
//! do, it enters with additional counters on it equal to the same number and
//! kinds of counters the copied permanent has on it."
//!
//! CR 707.2: counters are not copiable values, so the rider reproduces the
//! copied permanent's counters explicitly (CR 122.6 entry counters), and only
//! when the copy happens (CR 614.12a choice).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::Supertype;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DOMINION_SABOTEUR: &str = "You may have this creature enter as a copy of any artifact or creature on the battlefield, except it isn't legendary. If you do, it enters with additional counters on it equal to the same number and kinds of counters the copied permanent has on it.";

fn cast_to_copy_offer(runner: &mut GameRunner, saboteur: ObjectId) {
    let card_id = runner.state().objects[&saboteur].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: saboteur,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Dominion Saboteur");
    runner.advance_until_stack_empty();
    let WaitingFor::ReplacementChoice { candidates, .. } = &runner.state().waiting_for else {
        panic!(
            "Dominion Saboteur must offer its optional enter-as-copy replacement, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        candidates.len(),
        2,
        "copy offer includes an explicit decline"
    );
}

fn accept_copy_of(runner: &mut GameRunner, donor: ObjectId) {
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the copy replacement");
    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &runner.state().waiting_for else {
        panic!(
            "accepting must prompt for the copy source, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(valid_targets.contains(&donor));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(donor)),
        })
        .expect("choose the copy source");
}

fn assert_settled(runner: &mut GameRunner) {
    runner.advance_until_stack_empty();
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "entry did not reach priority: {:?}",
        runner.state().waiting_for
    );
    assert!(runner.state().stack.is_empty());
}

#[test]
fn copying_a_legendary_creature_drops_legendary_and_mirrors_its_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P1, "Legendary Bear", 2, 2, "")
        .as_legendary()
        .id();
    scenario.with_counter(donor, CounterType::Plus1Plus1, 2);
    let saboteur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let mut runner = scenario.build();

    cast_to_copy_offer(&mut runner, saboteur);
    accept_copy_of(&mut runner, donor);
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Legendary Bear");
    assert!(
        !entered
            .card_types
            .supertypes
            .contains(&Supertype::Legendary),
        "CR 707.9b: the copy exception removes legendary"
    );
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(2),
        "the copy enters with the donor's two +1/+1 counters"
    );
    assert_eq!(entered.counters.len(), 1);
    assert_eq!((entered.power, entered.toughness), (Some(4), Some(4)));
    // The donor keeps its own counters; the rider adds, it does not move.
    assert_eq!(
        runner.state().objects[&donor]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2)
    );
    // The legend rule never applies: the copy is not legendary.
    assert_eq!(runner.state().objects[&donor].zone, Zone::Battlefield);
}

#[test]
fn copying_an_artifact_mirrors_every_counter_kind() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_artifact_from_oracle(P0, "Charged Relic", "")
        .id();
    let charge = CounterType::Generic("charge".to_string());
    scenario.with_counter(donor, charge.clone(), 3);
    scenario.with_counter(donor, CounterType::Plus1Plus1, 1);
    let saboteur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let mut runner = scenario.build();

    cast_to_copy_offer(&mut runner, saboteur);
    accept_copy_of(&mut runner, donor);
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Charged Relic");
    assert_eq!(entered.counters.get(&charge).copied(), Some(3));
    assert_eq!(
        entered.counters.get(&CounterType::Plus1Plus1).copied(),
        Some(1)
    );
    assert_eq!(entered.counters.len(), 2);
}

#[test]
fn declining_the_copy_enters_a_plain_creature_without_counters() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let donor = scenario
        .add_creature_from_oracle(P1, "Legendary Bear", 2, 2, "")
        .as_legendary()
        .id();
    scenario.with_counter(donor, CounterType::Plus1Plus1, 2);
    let saboteur = scenario
        .add_creature_to_hand_from_oracle(P0, "Dominion Saboteur", 3, 3, DOMINION_SABOTEUR)
        .id();
    let mut runner = scenario.build();

    cast_to_copy_offer(&mut runner, saboteur);
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline the copy replacement");
    assert_settled(&mut runner);

    let entered = &runner.state().objects[&saboteur];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(entered.name, "Dominion Saboteur");
    assert!(
        entered.counters.is_empty(),
        "\"If you do\": no counters without the copy, got {:?}",
        entered.counters
    );
    assert_eq!((entered.power, entered.toughness), (Some(3), Some(3)));
}
