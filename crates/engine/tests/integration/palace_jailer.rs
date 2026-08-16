//! Palace Jailer — the ETB exile must last until an opponent becomes the monarch.
//!
//! The tests use the real card Oracle text and the cast/apply pipeline. The
//! delayed return is source-independent, persistent across cleanup, scoped to
//! any opponent, and guarded so a target that left exile is not moved again.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const PALACE_JAILER: &str = "When this creature enters, you become the monarch.\n\
When this creature enters, exile target creature an opponent controls until an opponent becomes the monarch.";

const OPPONENT_BECOMES_MONARCH: &str = "Target opponent becomes the monarch.";
const CONTROLLER_BECOMES_MONARCH: &str = "You become the monarch.";
const DESTROY_CREATURE: &str = "Destroy target creature.";
const RETURN_FROM_EXILE: &str = "Return target card from exile to the battlefield.";

struct Board {
    runner: GameRunner,
    jailer: ObjectId,
    target: ObjectId,
    opponent_crown: ObjectId,
    controller_crown: ObjectId,
    p1_crown: ObjectId,
    destroy: ObjectId,
    return_from_exile: ObjectId,
}

fn board(player_count: u8) -> Board {
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::PreCombatMain);
    for seat in 0..player_count {
        scenario.with_library_top(
            PlayerId(seat),
            &["Filler 1", "Filler 2", "Filler 3", "Filler 4"],
        );
    }

    let jailer = scenario
        .add_creature_to_hand_from_oracle(P0, "Palace Jailer", 2, 2, PALACE_JAILER)
        .id();
    let target = scenario.add_creature(P1, "Exiled Creature", 2, 2).id();
    let opponent_crown = scenario
        .add_spell_to_hand_from_oracle(P0, "Crown Opponent", true, OPPONENT_BECOMES_MONARCH)
        .id();
    let controller_crown = scenario
        .add_spell_to_hand_from_oracle(P0, "Crown Controller", true, CONTROLLER_BECOMES_MONARCH)
        .id();
    let p1_crown = scenario
        .add_spell_to_hand_from_oracle(P1, "Crown Yourself", true, CONTROLLER_BECOMES_MONARCH)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Jailer", true, DESTROY_CREATURE)
        .id();
    let return_from_exile = scenario
        .add_spell_to_hand_from_oracle(P1, "Return From Exile", true, RETURN_FROM_EXILE)
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(jailer).target_object(target).resolve();
    assert_eq!(
        outcome.zone_of(target),
        Zone::Exile,
        "reach-guard: Palace Jailer must actually exile the target"
    );
    assert_eq!(
        outcome.state().monarch,
        Some(P0),
        "reach-guard: the first ETB trigger must make Palace Jailer's controller monarch"
    );
    assert_eq!(
        outcome.state().delayed_triggers.len(),
        1,
        "reach-guard: the monarch-bounded return must be installed"
    );

    Board {
        runner,
        jailer,
        target,
        opponent_crown,
        controller_crown,
        p1_crown,
        destroy,
        return_from_exile,
    }
}

fn pass_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        match runner.state().waiting_for {
            WaitingFor::Priority { player: current } if current == player => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should succeed");
            }
            ref waiting => panic!("expected a priority window, got {waiting:?}"),
        }
    }
    panic!("priority did not reach {player:?}");
}

fn crown_opponent(runner: &mut GameRunner, spell: ObjectId, opponent: PlayerId) {
    pass_to(runner, P0);
    let outcome = runner.cast(spell).target_player(opponent).resolve();
    assert_eq!(
        outcome.state().monarch,
        Some(opponent),
        "reach-guard: the opponent crown spell must create a monarch-change event"
    );
}

/// CR 603.7 + CR 610.3 + CR 725.1: An opponent becoming the monarch returns
/// the exiled creature through the delayed-trigger path.
#[test]
fn opponent_becoming_monarch_returns_exiled_creature() {
    let Board {
        mut runner,
        target,
        opponent_crown,
        ..
    } = board(2);

    crown_opponent(&mut runner, opponent_crown, P1);

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "the target must return when an opponent becomes monarch"
    );
    assert_eq!(runner.state().objects[&target].controller, P1);
}

/// CR 725.1: The delayed condition is scoped to an opponent, so the controller
/// becoming monarch does not satisfy it.
#[test]
fn controller_becoming_monarch_does_not_return_exiled_creature() {
    let Board {
        mut runner,
        target,
        controller_crown,
        ..
    } = board(2);

    pass_to(&mut runner, P0);
    runner.cast(controller_crown).resolve();

    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Exile,
        "the target must remain exiled when only the controller becomes monarch"
    );
    assert_eq!(
        runner.state().delayed_triggers.len(),
        1,
        "the unmatched persistent trigger must remain installed"
    );
}

/// CR 603.7d + CR 400.7: Once created, the delayed trigger survives its source
/// moving to the graveyard and still returns the target on the matching event.
#[test]
fn palace_jailer_in_graveyard_does_not_change_the_delayed_return() {
    let Board {
        mut runner,
        jailer,
        target,
        opponent_crown,
        destroy,
        ..
    } = board(2);

    pass_to(&mut runner, P1);
    runner.cast(destroy).target_object(jailer).resolve();
    assert_eq!(runner.state().objects[&jailer].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&target].zone, Zone::Exile);

    crown_opponent(&mut runner, opponent_crown, P1);
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
}

/// CR 603.7b + CR 514.2: No "this turn" duration is printed, so the delayed
/// trigger remains through cleanup and fires on a later turn.
#[test]
fn monarch_bounded_return_survives_a_turn_boundary() {
    let Board {
        mut runner,
        target,
        p1_crown,
        ..
    } = board(2);

    runner.advance_to_phase(Phase::End);
    runner.auto_advance_to_main_phase();
    assert!(
        runner.state().turn_number > 2,
        "reach-guard: the scenario must cross a cleanup into a later turn"
    );
    assert_eq!(runner.state().objects[&target].zone, Zone::Exile);
    assert_eq!(runner.state().delayed_triggers.len(), 1);

    pass_to(&mut runner, P1);
    runner.cast(p1_crown).resolve();
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
}

/// CR 102.2 + CR 725.1: "an opponent" means any opponent, not only the
/// opponent whose creature was selected. P2 becomes monarch while P1's
/// creature is the exiled object.
#[test]
fn any_opponent_becoming_monarch_returns_the_selected_creature() {
    let Board {
        mut runner,
        target,
        opponent_crown,
        ..
    } = board(3);

    crown_opponent(&mut runner, opponent_crown, P2);
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);
}

/// CR 603.7c + CR 610.3: If the object leaves exile before the event, the
/// delayed return's expected-origin guard makes it a no-op.
#[test]
fn target_leaving_exile_before_monarch_change_is_not_moved_again() {
    let Board {
        mut runner,
        target,
        opponent_crown,
        return_from_exile,
        ..
    } = board(2);

    pass_to(&mut runner, P1);
    runner
        .cast(return_from_exile)
        .target_object(target)
        .resolve();
    assert_eq!(runner.state().objects[&target].zone, Zone::Battlefield);

    crown_opponent(&mut runner, opponent_crown, P1);
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Battlefield,
        "the delayed Exile -> Battlefield move must not re-exile or otherwise move a new object"
    );
}
