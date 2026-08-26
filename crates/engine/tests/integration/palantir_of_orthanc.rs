//! Palantir of Orthanc — CR 608.2c tracked mill set, CR 119.3 life loss, and
//! the opponent-controlled optional draw branch.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::{ObjectId, TrackedSetId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PALANTIR: &str = "At the beginning of your end step, put an influence counter on Palantir of Orthanc and scry 2. Then target opponent may have you draw a card. If that player doesn't, you mill X cards, where X is the number of influence counters on Palantir of Orthanc, and that player loses life equal to the total mana value of those cards.";

fn setup(
    preexisting_influence: u32,
    library_mana_values: &[u32],
) -> (GameRunner, i32, usize, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let palantir = scenario
        .add_artifact_from_oracle(P0, "Palantir of Orthanc", PALANTIR)
        .id();
    scenario.with_counter(
        palantir,
        CounterType::Generic("influence".to_string()),
        preexisting_influence,
    );
    // `add_spell_to_library_top` inserts each card at index zero. Iterate the
    // fixture in reverse so callers can state the intended library top first.
    for (index, mana_value) in library_mana_values.iter().rev().enumerate() {
        scenario
            .add_spell_to_library_top(P0, &format!("Library Card {index}"), false)
            .with_mana_cost(ManaCost::generic(*mana_value));
    }
    let stale = scenario
        .add_spell_to_hand(P0, "Stale Twenty", false)
        .with_mana_cost(ManaCost::generic(20))
        .id();
    let runner = scenario.build();
    let opponent_life = runner.life(P1);
    let controller_hand = hand_count(&runner);
    (runner, opponent_life, controller_hand, stale)
}

fn resolve_end_step(runner: &mut GameRunner, accept_draw: bool) {
    runner.advance_to_end_step();
    for _ in 0..128 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            }
            | WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let choice = target_slots[selection.current_slot]
                    .legal_targets
                    .iter()
                    .find(|target| **target == TargetRef::Player(P1))
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: choice })
                    .expect("choose Palantir's target opponent");
            }
            WaitingFor::ScryChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards { cards })
                    .expect("keep the scryed cards on top");
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                assert_eq!(player, P1, "the targeted opponent controls the draw choice");
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: accept_draw,
                    })
                    .expect("answer Palantir's draw choice");
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while Palantir resolves");
            }
            other => panic!("unexpected Palantir resolution prompt: {other:?}"),
        }
    }
}

fn milled_count(runner: &GameRunner) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|object| object.owner == P0 && object.zone == Zone::Graveyard)
        .count()
}

fn hand_count(runner: &GameRunner) -> usize {
    runner.state().players[P0.0 as usize].hand.len()
}

#[test]
fn decline_mills_influence_count_and_loses_exact_mana_value_sum() {
    let (mut runner, life_before, hand_before, _) = setup(2, &[2, 5, 1, 9]);
    let controller_life_before = runner.life(P0);
    resolve_end_step(&mut runner, false);
    assert_eq!(milled_count(&runner), 3);
    assert_eq!(runner.life(P1), life_before - 8);
    assert_eq!(runner.life(P0), controller_life_before);
    assert_eq!(hand_count(&runner), hand_before);
}

#[test]
fn accept_draws_one_and_skips_mill_and_life_loss() {
    let (mut runner, life_before, hand_before, _) = setup(2, &[2, 5, 1, 9]);
    resolve_end_step(&mut runner, true);
    assert_eq!(milled_count(&runner), 0);
    assert_eq!(runner.life(P1), life_before);
    assert_eq!(hand_count(&runner), hand_before + 1);
}

#[test]
fn short_library_loses_only_mana_value_of_cards_actually_milled() {
    let (mut runner, life_before, _, _) = setup(4, &[4, 7]);
    resolve_end_step(&mut runner, false);
    assert_eq!(milled_count(&runner), 2);
    assert_eq!(runner.life(P1), life_before - 11);
}

#[test]
fn empty_library_mills_zero_and_loses_zero_life() {
    let (mut runner, life_before, _, _) = setup(2, &[]);
    resolve_end_step(&mut runner, false);
    assert_eq!(milled_count(&runner), 0);
    assert_eq!(runner.life(P1), life_before);
}

#[test]
fn stale_tracked_set_does_not_pollute_current_mill_sum() {
    let (mut runner, life_before, _, stale) = setup(0, &[3]);
    let stale_set = TrackedSetId(runner.state().next_tracked_set_id);
    runner.state_mut().next_tracked_set_id += 1;
    runner
        .state_mut()
        .tracked_object_sets
        .insert(stale_set, vec![stale]);

    resolve_end_step(&mut runner, false);
    assert_eq!(milled_count(&runner), 1);
    assert_eq!(runner.life(P1), life_before - 3);
}
