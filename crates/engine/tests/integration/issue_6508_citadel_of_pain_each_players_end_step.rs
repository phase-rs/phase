//! Issue #6508 — Citadel of Pain damages each player at their own end step by
//! the number of untapped lands THAT player controls.
//!
//! > At the beginning of each player's end step, this enchantment deals X damage
//! > to that player, where X is the number of untapped lands they control.
//!
//! Reported as "damages its controller instead of opponents". The board gives
//! the two players different untapped-land counts, so the opponent's end step
//! separates the three readings: the correct one (the opponent takes THEIR
//! count), a count bound to the enchantment's controller (the opponent takes
//! the CONTROLLER's count), and a trigger that fires only on the controller's
//! own end step (the opponent takes nothing).

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

// Verbatim Oracle text (Scryfall, 2026-09-15).
const CITADEL_OF_PAIN: &str = "At the beginning of each player's end step, this enchantment deals X damage to that player, where X is the number of untapped lands they control.";

const CONTROLLER_LANDS: i32 = 5;
const OPPONENT_LANDS: i32 = 2;

fn life(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

#[test]
fn citadel_of_pain_hits_each_player_for_their_own_untapped_lands() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for &player in &[P0, P1] {
        scenario.with_library_top(player, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    let citadel = scenario
        .add_creature(P0, "Citadel of Pain", 0, 0)
        .as_enchantment()
        .from_oracle_text(CITADEL_OF_PAIN)
        .id();
    for _ in 0..CONTROLLER_LANDS {
        scenario.add_basic_land(P0, ManaColor::Red);
    }
    for _ in 0..OPPONENT_LANDS {
        scenario.add_basic_land(P1, ManaColor::Blue);
    }
    let mut runner = scenario.build();

    assert!(
        runner.state().objects[&citadel]
            .trigger_definitions
            .iter_unchecked()
            .any(|entry| {
                entry.definition.mode == TriggerMode::Phase
                    && entry.definition.phase == Some(Phase::End)
            }),
        "reach-guard: Citadel of Pain's Oracle text must provide its end-step trigger"
    );
    let (p0_start, p1_start) = (life(&runner, P0), life(&runner, P1));

    // CR 603.2: the end-step trigger fires on the controller's own end step.
    runner.advance_to_end_step();
    assert!(
        runner.state().active_player == P0 && runner.state().phase == Phase::End,
        "must reach P0's end step; stopped at {:?} of {:?}'s turn on {:?}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        life(&runner, P0),
        p0_start - CONTROLLER_LANDS,
        "on P0's end step, P0 takes damage equal to the untapped lands P0 controls"
    );
    assert_eq!(
        life(&runner, P1),
        p1_start,
        "P1 takes nothing on P0's end step"
    );

    // `advance_to_phase` returns at once while already in the end step, so step
    // into P1's turn first.
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.advance_to_end_step();
    assert!(
        runner.state().active_player == P1 && runner.state().phase == Phase::End,
        "must reach P1's end step; stopped at {:?} of {:?}'s turn on {:?}",
        runner.state().phase,
        runner.state().active_player,
        runner.state().waiting_for
    );
    // CR 603.2: it fires again on the opponent's end step. The ability is controlled by P0, but
    // "that player" and "they" are the player whose end step it is.
    assert!(
        runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == citadel && entry.controller == P0),
        "Citadel's P0-controlled trigger must be on the stack at P1's end step; stack={:?}",
        runner.state().stack
    );
    runner.advance_until_stack_empty();

    // CR 120.3a: damage to a player causes that much life loss.
    assert_eq!(
        life(&runner, P1),
        p1_start - OPPONENT_LANDS,
        "on P1's end step, P1 takes damage equal to the untapped lands P1 controls \
         ({OPPONENT_LANDS}), not the lands its controller controls ({CONTROLLER_LANDS})"
    );
    assert_eq!(
        life(&runner, P0),
        p0_start - CONTROLLER_LANDS,
        "the controller takes nothing more on the opponent's end step"
    );
}
