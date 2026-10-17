//! Issue #4231 — Final Fortune: "Take an extra turn after this one. At the
//! beginning of that turn's end step, you lose the game."
//!
//! CR 500.7 + CR 603.7a: "that turn" is the extra turn the first sentence
//! grants, so the delayed loss must wait for that turn's end step. Before the
//! fix it fired at the end step of the turn Final Fortune was cast in, so the
//! caster lost before the extra turn was ever taken.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const FINAL_FORTUNE: &str = "Take an extra turn after this one. At the beginning of that turn's end step, you lose the game.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

/// Walk the real priority loop until `stop` holds or the game ends. Uses
/// `PassPriority` rather than `advance_to_phase`, which re-runs
/// beginning-of-step triggers when priority is already open.
fn pass_priority_until(runner: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..200 {
        if stop(runner) || matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }) {
            return;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                runner.declare_attackers(&[]).expect("declare no attackers");
            }
            other => panic!("unexpected prompt while advancing: {other:?}"),
        }
    }
    panic!("stop condition never reached");
}

fn in_turn_after_its_end_step(runner: &GameRunner, turn: u32) -> bool {
    runner.state().turn_number > turn
}

fn assert_still_in_game(runner: &GameRunner, when: &str) {
    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::GameOver { .. })
            && !state.players[0].is_eliminated,
        "Final Fortune's caster must still be in the game {when}; waiting_for = {:?}",
        state.waiting_for
    );
}

fn assert_caster_lost(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        state.players[0].is_eliminated,
        "the caster must lose at the extra turn's end step; waiting_for = {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.waiting_for,
        WaitingFor::GameOver { winner: Some(P1) },
        "the opponent wins once the delayed loss resolves"
    );
}

/// Cast Final Fortune on `caster`'s behalf while `active` holds the turn.
fn cast_final_fortune(active: PlayerId) -> (GameRunner, u32) {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Lib A", "Lib B", "Lib C"]);
    scenario.with_library_top(P1, &["Opp A", "Opp B", "Opp C"]);
    let final_fortune = scenario
        .add_spell_to_hand_from_oracle(P0, "Final Fortune", true, FINAL_FORTUNE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(2, ManaType::Red));

    let mut runner = scenario.build();
    if active != P0 {
        let state = runner.state_mut();
        state.active_player = active;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    let cast_turn = runner.state().turn_number;
    runner.cast(final_fortune).resolve();

    // Reach-guards: the spell granted exactly one extra turn and scheduled one
    // delayed trigger, so every later assertion is about that trigger's timing.
    assert_eq!(
        runner.state().extra_turns.len(),
        1,
        "Final Fortune must grant one extra turn"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "Final Fortune must schedule its delayed loss"
    );
    (runner, cast_turn)
}

#[test]
fn final_fortune_cast_on_your_turn_loses_at_the_extra_turns_end_step_not_this_one() {
    let (mut runner, cast_turn) = cast_final_fortune(P0);

    // Through this turn's end step and cleanup, into the extra turn.
    pass_priority_until(&mut runner, |r| in_turn_after_its_end_step(r, cast_turn));
    assert_still_in_game(&runner, "after the turn Final Fortune was cast in");
    assert_eq!(
        runner.state().active_player,
        P0,
        "the extra turn is the caster's"
    );
    assert_eq!(runner.state().turn_number, cast_turn + 1);

    // Into the extra turn's end step, then let the delayed loss resolve.
    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
    assert_caster_lost(&runner);
}

#[test]
fn final_fortune_cast_on_an_opponents_turn_loses_at_the_extra_turns_end_step() {
    // Instant speed on the opponent's turn: "this one" is P1's turn, and the
    // extra turn is P0's. The opponent's end step must not fire the loss.
    let (mut runner, cast_turn) = cast_final_fortune(P1);

    pass_priority_until(&mut runner, |r| in_turn_after_its_end_step(r, cast_turn));
    assert_still_in_game(&runner, "after the opponent's turn it was cast in");
    assert_eq!(
        runner.state().active_player,
        P0,
        "the extra turn is the caster's"
    );

    pass_priority_until(&mut runner, |r| r.state().turn_number > cast_turn + 1);
    assert_caster_lost(&runner);
}
