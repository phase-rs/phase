//! Strefan, Maurer Progenitor — the end-step Blood-token count is "for each
//! player who lost life this turn", an all-players population.
//!
//! Oracle (verbatim, Scryfall):
//!   Flying
//!   At the beginning of your end step, create a Blood token for each player
//!   who lost life this turn.
//!   Whenever Strefan attacks, you may sacrifice two Blood tokens. If you do,
//!   you may put a Vampire card from your hand onto the battlefield tapped and
//!   attacking. It gains indestructible until end of turn.
//!
//! CR 608.2h + CR 119.3: the token count is read when the trigger resolves,
//! over every player (including the controller) whose life was lost this turn.
//! The parse must lower "for each player who lost life this turn" to a dynamic
//! `PlayerCount{PlayerAttribute{relation: All, attr: LifeLostThisTurn, GE 1}}`
//! count, not a frozen `Fixed(1)`.
//!
//! Revert map: with the opponent-only for-each grammar reverted, the clause does
//! not parse, `try_parse_for_each_effect` declines, and the `Token` keeps
//! `count: Fixed(1)` — so this test observes 1 Blood token instead of 2. That is
//! the discriminating assertion.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::PlayerId;

const STREFAN: &str = "Flying\nAt the beginning of your end step, create a Blood token for each player who lost life this turn.\nWhenever Strefan attacks, you may sacrifice two Blood tokens. If you do, you may put a Vampire card from your hand onto the battlefield tapped and attacking. It gains indestructible until end of turn.";

const P2: PlayerId = PlayerId(2);

fn blood_tokens_on_battlefield(state: &engine::types::game_state::GameState) -> u32 {
    state
        .objects
        .values()
        .filter(|object| {
            object.is_token && object.zone == Zone::Battlefield && object.name == "Blood"
        })
        .count() as u32
}

#[test]
fn strefan_end_step_creates_a_blood_token_per_player_who_lost_life() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PostCombatMain);
    scenario.add_creature_from_oracle(P0, "Strefan, Maurer Progenitor", 3, 2, STREFAN);
    let mut runner = scenario.build();

    // CR 119.3: seed this-turn life-loss history. The controller and P1 lost
    // life; P2 did not. "Each player" includes the controller, so the expected
    // count is 2 — an opponent-only read would answer 1.
    {
        let state = runner.state_mut();
        state.players[P0.0 as usize].life_lost_this_turn = 1;
        state.players[P1.0 as usize].life_lost_this_turn = 1;
        state.players[P2.0 as usize].life_lost_this_turn = 0;
    }

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    assert_eq!(
        blood_tokens_on_battlefield(runner.state()),
        2,
        "one Blood token per player who lost life this turn (controller + P1), \
         not a frozen one; waiting_for = {:?}",
        runner.state().waiting_for
    );
}
