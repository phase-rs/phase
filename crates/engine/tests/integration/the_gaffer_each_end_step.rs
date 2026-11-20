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
const SWORDS_TO_PLOWSHARES: &str =
    "Exile target creature. Its controller gains life equal to its power.";
const HEALING_SALVE: &str = "Choose one —\n• Target player gains 3 life.\n• Prevent the next 3 damage that would be dealt to any target this turn.";

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

/// What The Gaffer did at the end step of the current turn.
#[derive(Debug, PartialEq)]
struct EndStep {
    triggered: bool,
    drawn: usize,
}

/// Pass to the current turn's end step and record whether The Gaffer's trigger was waiting
/// on the stack as the step began (CR 500.6), then finish the turn and count the cards P0
/// drew in between. P0's hand doesn't change before the end step.
fn finish_turn(runner: &mut GameRunner, gaffer: ObjectId) -> EndStep {
    let turn_player = runner.state().active_player;
    let hand_before = hand_size(runner, P0);
    pass_until(runner, |state| state.phase == Phase::End);
    assert_eq!(
        hand_size(runner, P0),
        hand_before,
        "nothing draws before the end step"
    );
    let triggered = runner
        .state()
        .stack
        .iter()
        .any(|entry| entry.source_id == gaffer);
    pass_until(runner, |state| state.active_player != turn_player);
    EndStep {
        triggered,
        drawn: hand_size(runner, P0) - hand_before,
    }
}

/// Cast Healing Salve in its "Target player gains 3 life" mode on P0.
fn gain_three(runner: &mut GameRunner, salve: ObjectId) {
    let before = runner.life(P0);
    runner.cast(salve).modes(&[0]).target_player(P0).resolve();
    assert_eq!(
        runner.life(P0),
        before + 3,
        "reach: Healing Salve gains 3 life"
    );
}

const DREW: EndStep = EndStep {
    triggered: true,
    drawn: 1,
};
const NOTHING: EndStep = EndStep {
    triggered: false,
    drawn: 0,
};

#[test]
fn the_gaffer_draws_at_every_qualifying_end_step_not_only_the_first() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gaffer = scenario
        .add_creature_from_oracle(P0, "The Gaffer", 2, 3, THE_GAFFER)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let salves: Vec<ObjectId> = (0..4)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Healing Salve", true, HEALING_SALVE)
                .id()
        })
        .collect();
    let swords = scenario
        .add_spell_to_hand_from_oracle(P0, "Swords to Plowshares", true, SWORDS_TO_PLOWSHARES)
        .id();
    for player in [P0, P1] {
        scenario.with_library_top(player, &["Card"; 12]);
    }
    let mut runner = scenario.build();

    gain_three(&mut runner, salves[0]);
    assert_eq!(
        finish_turn(&mut runner, gaffer),
        DREW,
        "the first qualifying end step draws a card"
    );

    // P1's turn: P0 gains no life, so P1's end step doesn't trigger for P0.
    assert_eq!(
        finish_turn(&mut runner, gaffer),
        NOTHING,
        "no life gained this turn"
    );

    pass_to_main_of(&mut runner, P0);
    gain_three(&mut runner, salves[1]);
    assert_eq!(
        finish_turn(&mut runner, gaffer),
        DREW,
        "the trigger fires again at a later qualifying end step"
    );

    // CR 603.4: 2 life is short of 3, so the trigger doesn't fire. Swords to Plowshares on
    // P0's own 2-power Grizzly Bears gains P0 exactly 2.
    pass_to_main_of(&mut runner, P0);
    let before = runner.life(P0);
    runner.cast(swords).target_objects(&[bears]).resolve();
    assert_eq!(runner.life(P0), before + 2, "reach: Swords gains 2 life");
    assert_eq!(
        finish_turn(&mut runner, gaffer),
        NOTHING,
        "2 life gained is below the threshold"
    );

    // "Each end step": P0 gains 3 life during P1's turn, so P1's end step draws for P0.
    pass_until(&mut runner, |state| {
        state.active_player == P1
            && state.phase == Phase::PreCombatMain
            && matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
    });
    gain_three(&mut runner, salves[2]);
    assert_eq!(
        finish_turn(&mut runner, gaffer),
        DREW,
        "the opponent's end step draws too when The Gaffer's controller gained 3 life that turn"
    );

    pass_to_main_of(&mut runner, P0);
    gain_three(&mut runner, salves[3]);
    assert_eq!(
        finish_turn(&mut runner, gaffer),
        DREW,
        "and again on P0's next turn"
    );
}
