//! Reaper's Scythe — the end-step soul-counter count is "for each player who
//! lost life this turn", an all-players population.
//!
//! Oracle (verbatim, Scryfall):
//!   Job select
//!   At the beginning of your end step, put a soul counter on this Equipment
//!   for each player who lost life this turn.
//!   Equipped creature gets +1/+1 for each soul counter on this Equipment and
//!   is an Assassin in addition to its other types.
//!   Death Sickle — Equip {2}
//!
//! CR 608.2h + CR 119.3: the count is read when the trigger resolves, over
//! every player (including the controller) whose life was lost this turn. The
//! parse must lower "for each player who lost life this turn" to a dynamic
//! `PlayerCount{PlayerAttribute{relation: All, attr: LifeLostThisTurn, GE 1}}`
//! count, not a frozen `Fixed(1)`.
//!
//! Revert map: with the opponent-only for-each grammar reverted, the clause does
//! not parse, `try_parse_for_each_effect` declines, and the `PutCounter` keeps
//! `count: Fixed(1)` — so this test observes 1 counter instead of 2. That is the
//! discriminating assertion.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::PlayerId;

const REAPERS_SCYTHE: &str = "Job select\nAt the beginning of your end step, put a soul counter on this Equipment for each player who lost life this turn.\nEquipped creature gets +1/+1 for each soul counter on this Equipment and is an Assassin in addition to its other types.\nDeath Sickle \u{2014} Equip {2}";

const P2: PlayerId = PlayerId(2);

fn soul_counters(state: &engine::types::game_state::GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Generic("soul".to_string()))
        .copied()
        .unwrap_or(0)
}

#[test]
fn reapers_scythe_end_step_adds_a_soul_counter_per_player_who_lost_life() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    let scythe = scenario
        .add_artifact_from_oracle(P0, "Reaper's Scythe", REAPERS_SCYTHE)
        .id();
    let mut runner = scenario.build();

    // CR 119.3: seed this-turn life-loss history. The controller and P1 lost
    // life; P2 did not. "Each player" includes the controller, so the expected
    // count is 2 — an opponent-only read would answer 1.
    {
        let state = runner.state_mut();
        state.players[P0.0 as usize].life_lost_this_turn = 2;
        state.players[P1.0 as usize].life_lost_this_turn = 1;
        state.players[P2.0 as usize].life_lost_this_turn = 0;
    }

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        soul_counters(runner.state(), scythe),
        2,
        "one soul counter per player who lost life this turn (controller + P1), \
         not a frozen one; waiting_for = {:?}",
        runner.state().waiting_for
    );
}

/// CR 608.2h: the count is fixed only when the trigger resolves — a player who
/// lost life AFTER the trigger was put on the stack still counts.
#[test]
fn reapers_scythe_counts_life_lost_after_the_trigger_was_put_on_the_stack() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    let scythe = scenario
        .add_artifact_from_oracle(P0, "Reaper's Scythe", REAPERS_SCYTHE)
        .id();
    let mut runner = scenario.build();

    // Advance into the end step so the trigger is on the stack, then record the
    // life loss before the stack empties.
    runner.advance_to_end_step();
    {
        let state = runner.state_mut();
        state.players[P1.0 as usize].life_lost_this_turn = 3;
        state.players[P2.0 as usize].life_lost_this_turn = 1;
    }
    runner.advance_until_stack_empty();

    assert_eq!(
        soul_counters(runner.state(), scythe),
        2,
        "the count is read at resolution, not when the trigger was put on the stack"
    );
}
