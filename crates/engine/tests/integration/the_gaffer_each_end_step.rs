//! Issue #777: The Gaffer — "At the beginning of each end step, if you gained 3 or more life
//! this turn, draw a card." It triggers as each end step begins (CR 500.6 + CR 603.2b), on
//! every turn and not only the first, and only when its controller gained 3 or more life
//! that turn (CR 603.4).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const THE_GAFFER: &str =
    "At the beginning of each end step, if you gained 3 or more life this turn, draw a card.";
const GAIN_THREE: &str = "You gain 3 life.";

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.owner == player && obj.zone == Zone::Hand)
        .count()
}

/// Pass priority, declaring no attackers when asked (The Gaffer is a creature), until
/// `done` holds. Only the public priority pass moves the game, so each step's beginning
/// is processed once, as in play.
fn pass_until(runner: &mut GameRunner, done: impl Fn(&GameState) -> bool) {
    for _ in 0..200 {
        if done(runner.state()) {
            return;
        }
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .declare_attackers(&[])
                    .expect("declaring no attackers is legal");
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority is legal");
            }
            ref other => panic!("unexpected prompt {other:?}"),
        }
    }
    panic!("the game never reached the wanted state");
}

/// Pass to `player`'s next precombat main phase.
fn pass_to_main_of(runner: &mut GameRunner, player: PlayerId) {
    pass_until(runner, |state| {
        state.active_player == player && state.phase == Phase::PreCombatMain
    });
}

/// In P0's precombat main: gain 3 life with `spell`, then finish the turn, and return how
/// many cards P0 drew after the main phase (only The Gaffer draws there).
fn gain_three_then_finish_turn(runner: &mut GameRunner, spell: ObjectId) -> usize {
    let before = runner.life(P0);
    runner.cast(spell).resolve();
    assert_eq!(runner.life(P0), before + 3, "reach: the spell gains 3 life");
    let hand_before = hand_size(runner, P0);
    pass_until(runner, |state| state.active_player == P1);
    hand_size(runner, P0) - hand_before
}

#[test]
fn the_gaffer_draws_at_every_qualifying_end_step_not_only_the_first() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "The Gaffer", 2, 3, THE_GAFFER);
    let first = scenario
        .add_spell_to_hand_from_oracle(P0, "Gain Three A", true, GAIN_THREE)
        .id();
    let second = scenario
        .add_spell_to_hand_from_oracle(P0, "Gain Three B", true, GAIN_THREE)
        .id();
    let third = scenario
        .add_spell_to_hand_from_oracle(P0, "Gain Three C", true, GAIN_THREE)
        .id();
    let on_their_turn = scenario
        .add_spell_to_hand_from_oracle(P0, "Gain Three D", true, GAIN_THREE)
        .id();
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Card"; 12]);
    }
    let mut runner = scenario.build();

    assert_eq!(
        gain_three_then_finish_turn(&mut runner, first),
        1,
        "the first qualifying end step draws a card"
    );

    // P1's turn: P0 gains no life, so P1's end step doesn't draw for P0. Stop at P0's
    // upkeep, before P0's own draw step.
    let hand_before = hand_size(&runner, P0);
    pass_until(&mut runner, |state| state.active_player == P0);
    assert_eq!(
        hand_size(&runner, P0),
        hand_before,
        "no life gained this turn, so no draw"
    );

    // Back on P0's turn, and the one after: each qualifying end step draws again.
    pass_to_main_of(&mut runner, P0);
    assert_eq!(
        gain_three_then_finish_turn(&mut runner, second),
        1,
        "the trigger fires again at a later qualifying end step"
    );
    pass_to_main_of(&mut runner, P0);
    assert_eq!(
        gain_three_then_finish_turn(&mut runner, third),
        1,
        "and again on the turn after that"
    );

    // "Each end step": P0 gains 3 life during P1's turn, so P1's end step draws for P0.
    pass_until(&mut runner, |state| {
        state.active_player == P1
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    let before = runner.life(P0);
    runner.cast(on_their_turn).resolve();
    assert_eq!(runner.life(P0), before + 3, "reach: the spell gains 3 life");
    let hand_before = hand_size(&runner, P0);
    pass_until(&mut runner, |state| state.active_player == P0);
    assert_eq!(
        hand_size(&runner, P0) - hand_before,
        1,
        "the opponent's end step draws too when The Gaffer's controller gained 3 life that turn"
    );
}
