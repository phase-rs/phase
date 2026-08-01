//! Fevered Visions resolves its printed end-step instructions in order:
//! the active player draws, then the post-draw opponent/hand-size gate decides
//! whether that player takes 2 damage.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const FEVERED_VISIONS_ORACLE: &str = "At the beginning of each player's end step, that player draws a card. If the player is your opponent and has four or more cards in hand, this enchantment deals 2 damage to that player.";

fn hand_count(runner: &engine::game::scenario::GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|candidate| candidate.id == player)
        .expect("scenario player")
        .hand
        .len()
}

/// Drive the production turn transition (postcombat main -> end step), trigger
/// placement, priority passes, and stack resolution. Returns `(draw_delta,
/// life_delta)` for the active player.
fn resolve_fevered_end_step(active_player: PlayerId, pre_draw_hand: usize) -> (isize, i32) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    for index in 0..pre_draw_hand {
        scenario.add_card_to_hand(active_player, &format!("Hand Card {index}"));
    }
    scenario.with_library_top(active_player, &["Fevered Draw"]);
    scenario
        .add_creature(P0, "Fevered Visions", 0, 0)
        .as_enchantment()
        .from_oracle_text(FEVERED_VISIONS_ORACLE);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = active_player;
        state.priority_player = active_player;
        state.waiting_for = WaitingFor::Priority {
            player: active_player,
        };
    }
    let hand_before = hand_count(&runner, active_player);
    let life_before = runner.life(active_player);

    // CR 513.1 + CR 603.2b: pass real priority from postcombat main into the
    // end step, where the beginning-of-step trigger is created.
    for _ in 0..8 {
        if runner.state().phase == Phase::End {
            break;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected prompt before end step: {:?}",
            runner.state().waiting_for
        );
        runner
            .act(GameAction::PassPriority)
            .expect("priority pass toward end step");
    }
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "real turn machinery must enter the end step"
    );
    assert!(
        !runner.state().stack.is_empty()
            || matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }),
        "Fevered Visions trigger must be pending at the end-step boundary"
    );

    // CR 608.2c: priority passes resolve the real triggered-ability stack entry,
    // executing Draw before its conditional DealDamage child.
    runner.advance_until_stack_empty();
    assert!(
        runner.state().stack.is_empty(),
        "Fevered Visions trigger must fully resolve"
    );

    (
        hand_count(&runner, active_player) as isize - hand_before as isize,
        runner.life(active_player) - life_before,
    )
}

#[test]
fn controller_draws_to_four_without_taking_damage() {
    let (draw_delta, life_delta) = resolve_fevered_end_step(P0, 3);
    assert_eq!(draw_delta, 1, "the controller must draw before the gate");
    assert_eq!(
        life_delta, 0,
        "the controller is not their own opponent and must take no damage"
    );
}

#[test]
fn opponent_draws_to_four_then_takes_two_damage() {
    let (draw_delta, life_delta) = resolve_fevered_end_step(P1, 3);
    assert_eq!(draw_delta, 1, "the opponent must draw before the gate");
    assert_eq!(
        life_delta, -2,
        "the post-draw hand of four must satisfy the damage gate"
    );
}

#[test]
fn opponent_draws_to_three_without_taking_damage() {
    let (draw_delta, life_delta) = resolve_fevered_end_step(P1, 2);
    assert_eq!(
        draw_delta, 1,
        "the opponent must still draw below threshold"
    );
    assert_eq!(
        life_delta, 0,
        "the post-draw hand of three must fail the damage gate"
    );
}
