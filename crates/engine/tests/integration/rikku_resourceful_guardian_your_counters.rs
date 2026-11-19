//! Issue #3861: Rikku, Resourceful Guardian — "Whenever you put one or more counters on a
//! creature, until end of turn, that creature can't be blocked by creatures your opponents
//! control." The trigger reads who put the counters (CR 603.2c names the event; "you" is
//! Rikku's controller), so an opponent putting a counter on their own creature doesn't
//! trigger it, while Rikku's controller putting one on any creature does.

use engine::game::combat::can_block_pair;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const RIKKU: &str = "Whenever you put one or more counters on a creature, until end of turn, that creature can't be blocked by creatures your opponents control.\nSteal — {1}, {T}: Move a counter from target creature an opponent controls onto target creature you control. Activate only as a sorcery.";
const BATTLEGROWTH: &str = "Put a +1/+1 counter on target creature.";

struct Board {
    runner: GameRunner,
    rikku: ObjectId,
    mine: ObjectId,
    theirs: ObjectId,
    my_growth: ObjectId,
    their_growth: ObjectId,
}

/// P0 controls Rikku and a Grizzly Bears; P1 controls a Grizzly Bears. Each player holds a
/// Battlegrowth.
fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let rikku = scenario
        .add_creature(P0, "Rikku, Resourceful Guardian", 2, 3)
        .from_oracle_text_with_keywords(&["Steal"], RIKKU)
        .id();
    let mine = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let theirs = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let my_growth = scenario
        .add_spell_to_hand_from_oracle(P0, "Battlegrowth", true, BATTLEGROWTH)
        .id();
    let their_growth = scenario
        .add_spell_to_hand_from_oracle(P1, "Battlegrowth", true, BATTLEGROWTH)
        .id();
    Board {
        runner: scenario.build(),
        rikku,
        mine,
        theirs,
        my_growth,
        their_growth,
    }
}

fn counters_on(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id].counters.values().copied().sum()
}

fn rikku_trigger_on_stack(state: &GameState, rikku: ObjectId) -> bool {
    state.stack.iter().any(|entry| entry.source_id == rikku)
}

/// Pass priority until `player` holds it with an empty stack.
fn pass_to_priority_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is legal");
    }
    panic!("{player:?} never got priority");
}

/// Rikku's controller puts a counter on their own creature: the trigger fires, and the
/// creature can't be blocked by the opponent's creature this turn (CR 509.1b).
#[test]
fn your_counter_on_your_creature_triggers_rikku() {
    let Board {
        mut runner,
        mine,
        theirs,
        my_growth,
        ..
    } = board();
    assert!(
        can_block_pair(runner.state(), theirs, mine),
        "reach: before the counter, P1's Bears can block P0's Bears"
    );

    runner.cast(my_growth).target_objects(&[mine]).commit();
    runner.advance_until_stack_empty();
    assert_eq!(
        counters_on(&runner, mine),
        1,
        "reach: P0's Bears got the counter"
    );
    assert!(
        !can_block_pair(runner.state(), theirs, mine),
        "Rikku triggered: until end of turn, P0's Bears can't be blocked by P1's creatures"
    );
}

/// "On a creature" isn't limited to your own: Rikku's controller putting a counter on the
/// opponent's creature also triggers it.
#[test]
fn your_counter_on_an_opponents_creature_triggers_rikku() {
    let Board {
        mut runner,
        mine,
        theirs,
        my_growth,
        ..
    } = board();

    runner.cast(my_growth).target_objects(&[theirs]).commit();
    runner.advance_until_stack_empty();
    assert_eq!(
        counters_on(&runner, theirs),
        1,
        "reach: P1's Bears got the counter"
    );
    assert!(
        !can_block_pair(runner.state(), mine, theirs),
        "Rikku triggered even though P1 controls the creature: P0's creatures can't block it"
    );
}

/// The reported case: the opponent puts a counter on their own creature. "You" didn't put
/// it, so Rikku doesn't trigger and the opponent's creature stays blockable.
#[test]
fn opponents_counter_does_not_trigger_rikku() {
    let Board {
        mut runner,
        rikku,
        mine,
        theirs,
        their_growth,
        ..
    } = board();

    pass_to_priority_of(&mut runner, P1);
    runner.cast(their_growth).target_objects(&[theirs]).commit();
    assert!(
        !rikku_trigger_on_stack(runner.state(), rikku),
        "reach: nothing of Rikku's is on the stack before the counter is put"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        counters_on(&runner, theirs),
        1,
        "reach: P1's Bears got the counter"
    );
    assert!(
        can_block_pair(runner.state(), mine, theirs),
        "P1 put the counter, so Rikku doesn't trigger: P1's Bears stays blockable"
    );
}
