//! Kai Winn, Faithless Leader — a chosen-name cast-or-activate punisher.
//!
//! Oracle text:
//! "As Kai Winn enters, choose a nonland card name.
//! Whenever an opponent casts a spell with the chosen name or activates an
//! ability of a source with the chosen name, that player loses 3 life."
//!
//! CR 603.2: the line names two trigger events (a spell cast and an
//! activation), each narrowed to an object whose name is the chosen name
//! (CR 201.2a). CR 602.2a: the activating player is the one who loses life.
//! The as-enters name choice itself is the established Runed Halo / Pithing
//! Needle path; the fixtures here record the chosen name directly so each test
//! isolates the trigger filter.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::ChosenAttribute;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const KAI_ORACLE: &str = "As Kai Winn enters, choose a nonland card name.\nWhenever an opponent casts a spell with the chosen name or activates an ability of a source with the chosen name, that player loses 3 life.";
const DIVINATION_ORACLE: &str = "Draw two cards.";
const OPT_ORACLE: &str = "Scry 1.\nDraw a card.";
const TOME_ORACLE: &str = "{4}, {T}: Draw a card.";

fn colorless(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
        .collect()
}

fn blue(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]))
        .collect()
}

/// Kai Winn on P0's battlefield; `record_choice` sets its chosen name.
fn kai_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kai = scenario
        .add_creature_from_oracle(P0, "Kai Winn, Faithless Leader", 3, 1, KAI_ORACLE)
        .id();
    scenario.with_library_top(P0, &["Filler A", "Filler B", "Filler C"]);
    scenario.with_library_top(P1, &["Filler D", "Filler E", "Filler F"]);
    (scenario, kai)
}

fn record_choice(runner: &mut GameRunner, kai: ObjectId, chosen: &str) {
    runner
        .state_mut()
        .objects
        .get_mut(&kai)
        .unwrap()
        .chosen_attributes
        .push(ChosenAttribute::CardName(chosen.to_string()));
}

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.active_player = player;
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

fn assert_settled(runner: &GameRunner) {
    assert!(runner.state().stack.is_empty(), "stack must be empty");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "expected Priority, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn opponent_casting_the_chosen_name_loses_three() {
    let (mut scenario, kai) = kai_scenario();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P1, "Divination", false, DIVINATION_ORACLE)
        .id();
    scenario.with_mana_pool(P1, blue(3));
    let mut runner = scenario.build();
    record_choice(&mut runner, kai, "Divination");
    give_priority(&mut runner, P1);

    let outcome = runner.cast(spell).resolve();
    // CR 603.2 + CR 201.2a: a spell with the chosen name triggers Kai.
    outcome.assert_life_delta(P1, -3);
    outcome.assert_life_delta(P0, 0);
    assert_settled(&runner);
}

#[test]
fn opponent_casting_a_different_name_loses_nothing() {
    let (mut scenario, kai) = kai_scenario();
    let opt = scenario
        .add_spell_to_hand_from_oracle(P1, "Opt", true, OPT_ORACLE)
        .id();
    scenario.with_mana_pool(P1, blue(1));
    let mut runner = scenario.build();
    record_choice(&mut runner, kai, "Divination");
    give_priority(&mut runner, P1);

    let outcome = runner.cast(opt).resolve();
    // Reach guard: the spell actually resolved (Opt drew its card).
    outcome.assert_hand_drawn(P1, 1);
    // CR 201.2a: a spell with another name does not match the chosen name.
    outcome.assert_life_delta(P1, 0);
}

#[test]
fn opponent_activating_a_source_with_the_chosen_name_loses_three() {
    let (mut scenario, kai) = kai_scenario();
    let tome = scenario
        .add_artifact_from_oracle(P1, "Jayemdae Tome", TOME_ORACLE)
        .id();
    scenario.with_mana_pool(P1, colorless(4));
    let mut runner = scenario.build();
    record_choice(&mut runner, kai, "Jayemdae Tome");
    give_priority(&mut runner, P1);
    let before = life(&runner, P1);
    let hand_before = runner.state().players[1].hand.len();

    runner.activate(tome, 0).resolve();
    // Reach guard: the Tome's ability resolved.
    assert_eq!(runner.state().players[1].hand.len(), hand_before + 1);
    // CR 602.2a + CR 603.2: the activating opponent loses 3 life.
    assert_eq!(life(&runner, P1), before - 3);
    assert_settled(&runner);
}

#[test]
fn opponent_activating_a_differently_named_source_loses_nothing() {
    let (mut scenario, kai) = kai_scenario();
    let tome = scenario
        .add_artifact_from_oracle(P1, "Jayemdae Tome", TOME_ORACLE)
        .id();
    scenario.with_mana_pool(P1, colorless(4));
    let mut runner = scenario.build();
    record_choice(&mut runner, kai, "Divination");
    give_priority(&mut runner, P1);
    let before = life(&runner, P1);
    let hand_before = runner.state().players[1].hand.len();

    runner.activate(tome, 0).resolve();
    assert_eq!(runner.state().players[1].hand.len(), hand_before + 1);
    assert_eq!(life(&runner, P1), before);
}

#[test]
fn controller_activating_the_chosen_name_loses_nothing() {
    let (mut scenario, kai) = kai_scenario();
    let tome = scenario
        .add_artifact_from_oracle(P0, "Jayemdae Tome", TOME_ORACLE)
        .id();
    scenario.with_mana_pool(P0, colorless(4));
    let mut runner = scenario.build();
    record_choice(&mut runner, kai, "Jayemdae Tome");
    give_priority(&mut runner, P0);
    let before = life(&runner, P0);
    let hand_before = runner.state().players[0].hand.len();

    runner.activate(tome, 0).resolve();
    assert_eq!(runner.state().players[0].hand.len(), hand_before + 1);
    // CR 603.2: only an opponent's activation triggers Kai.
    assert_eq!(life(&runner, P0), before);
    assert_eq!(life(&runner, P1), 20);
}
