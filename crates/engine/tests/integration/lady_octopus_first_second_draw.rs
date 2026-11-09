//! Lady Octopus, Inspired Inventor — "Whenever you draw your first or second
//! card each turn, put an ingenuity counter on Lady Octopus."
//!
//! The runtime test drives real draw events through the existing trigger
//! pipeline, then crosses a turn boundary before checking that the draw ordinal
//! resets.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const LADY_OCTOPUS: &str = "Whenever you draw your first or second card each turn, put an \
ingenuity counter on Lady Octopus.\n\
{T}: You may cast an artifact spell from your hand with mana value less than or equal to the \
number of ingenuity counters on Lady Octopus without paying its mana cost.";
const OPT: &str = "Scry 1. (Look at the top card of your library. You may put that card on the \
bottom.)\nDraw a card.";

fn ingenuity_counters(runner: &GameRunner, lady: ObjectId) -> u32 {
    runner.state().objects[&lady]
        .counters
        .get(&CounterType::Generic("ingenuity".to_string()))
        .copied()
        .unwrap_or(0)
}

fn cast_opt(runner: &mut GameRunner, opt: ObjectId) {
    let outcome = runner.cast(opt).resolve();
    outcome.assert_hand_drawn(P0, 1);
    assert!(
        runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "Opt and its draw trigger should finish at priority"
    );
}

fn advance_to_next_turn(runner: &mut GameRunner) {
    runner.advance_to_phase(Phase::PostCombatMain);
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareAttackers { .. }
    ) {
        runner
            .declare_attackers(&[])
            .expect("an empty attack declaration advances the turn");
        runner.advance_to_phase(Phase::PostCombatMain);
    }
    runner.advance_to_phase(Phase::PreCombatMain);
}

#[test]
fn lady_octopus_triggers_for_first_and_second_draw_only_and_resets_each_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lady = scenario
        .add_creature_from_oracle(P0, "Lady Octopus, Inspired Inventor", 0, 2, LADY_OCTOPUS)
        .as_legendary()
        .with_subtypes(vec!["Human", "Scientist", "Villain"])
        .id();
    let first_opt = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
        .id();
    let second_opt = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
        .id();
    let third_opt = scenario
        .add_spell_to_hand_from_oracle(P0, "Opt", true, OPT)
        .id();
    for index in 0..5 {
        scenario.add_card_to_library_top(P0, &format!("Draw {index}"));
    }
    scenario.add_card_to_library_top(P1, "Opponent draw");
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]),
        ],
    );
    let mut runner = scenario.build();

    assert_eq!(ingenuity_counters(&runner, lady), 0);
    cast_opt(&mut runner, first_opt);
    assert_eq!(
        ingenuity_counters(&runner, lady),
        1,
        "the first draw of a turn puts an ingenuity counter on Lady Octopus"
    );
    cast_opt(&mut runner, second_opt);
    assert_eq!(
        ingenuity_counters(&runner, lady),
        2,
        "the second draw of a turn also puts an ingenuity counter on Lady Octopus"
    );
    cast_opt(&mut runner, third_opt);
    assert_eq!(
        ingenuity_counters(&runner, lady),
        2,
        "the third draw of a turn does not trigger the ability"
    );

    let turn_before = runner.state().turn_number;
    advance_to_next_turn(&mut runner);
    assert!(
        runner.state().turn_number > turn_before,
        "the test crossed a real turn boundary"
    );
    assert_eq!(
        runner.state().players[P1.0 as usize].hand.len(),
        1,
        "the opponent's turn-based draw ran during the transition"
    );
    assert_eq!(
        ingenuity_counters(&runner, lady),
        2,
        "the opponent's draw does not satisfy Lady Octopus's 'you draw' trigger"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the next turn begins at a completed priority boundary"
    );
    let hand_before_next_turn = runner.state().players[P0.0 as usize].hand.len();
    advance_to_next_turn(&mut runner);
    assert_eq!(runner.state().active_player, P0);
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before_next_turn + 1,
        "P0's draw step ran on the next turn"
    );
    assert_eq!(
        ingenuity_counters(&runner, lady),
        3,
        "the first draw of the next turn triggers again"
    );
}
