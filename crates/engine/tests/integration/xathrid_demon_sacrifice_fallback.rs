//! Xathrid Demon — the "can't sacrifice" fallback reads the SACRIFICE'S result,
//! not the opponent life-loss clause that follows it.
//!
//! Oracle (Scryfall `cards/named?exact=Xathrid Demon`): "At the beginning of your
//! upkeep, sacrifice a creature other than Xathrid Demon, then each opponent
//! loses life equal to the sacrificed creature's power. If you can't sacrifice a
//! creature, tap Xathrid Demon and you lose 7 life."
//!
//! CR anchors:
//!   - CR 608.2c: instructions are followed in the order written; "can't
//!     sacrifice" refers to the sacrifice instruction, not to the life-loss
//!     clause between them (which moves nothing).
//!   - CR 608.2f: "each opponent loses life" is one action over several players.
//!   - CR 701.21a: to sacrifice a permanent, its controller moves it to the
//!     graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text (keywords included).
const XATHRID_DEMON: &str = "Flying, trample\n\
     At the beginning of your upkeep, sacrifice a creature other than Xathrid Demon, \
     then each opponent loses life equal to the sacrificed creature's power. If you \
     can't sacrifice a creature, tap Xathrid Demon and you lose 7 life.";

const P2: PlayerId = PlayerId(2);
const FODDER_POWER: i32 = 3;
const START_LIFE: i32 = 20;

/// Three players; P1 controls the Demon (and fodder when `with_fodder`). The turn
/// passes from P0's end step into P1's upkeep, which puts the Demon's trigger on
/// the stack. Returns the runner after that trigger resolved, the Demon, and the
/// fodder creature if any.
fn p1_upkeep(with_fodder: bool) -> (GameRunner, ObjectId, Option<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(3, 1707);
    scenario.at_phase(Phase::End);
    let demon = scenario
        .add_creature_from_oracle(P1, "Xathrid Demon", 7, 7, XATHRID_DEMON)
        .id();
    let fodder = with_fodder.then(|| {
        scenario
            .add_creature(P1, "Fodder Bear", FODDER_POWER, 3)
            .id()
    });
    for seat in 0..3 {
        // CR 504.1: P1's draw step must find a card.
        scenario.add_spell_to_library_top(PlayerId(seat), "Library Filler", false);
        scenario.add_spell_to_library_top(PlayerId(seat), "Library Filler", false);
    }
    let mut runner = scenario.build();
    for _ in 0..16 {
        if runner.state().phase == Phase::Upkeep {
            break;
        }
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        runner
            .act(GameAction::PassPriority)
            .expect("advance toward P1's upkeep");
    }
    assert_eq!(runner.state().phase, Phase::Upkeep, "reach guard");
    assert_eq!(runner.state().active_player, P1, "reach guard");
    assert!(
        !runner.state().stack.is_empty(),
        "reach guard: the upkeep trigger must be on the stack"
    );
    runner.advance_until_stack_empty();
    (runner, demon, fodder)
}

#[test]
fn sacrificing_a_creature_skips_the_fallback_and_drains_each_opponent() {
    let (runner, demon, fodder) = p1_upkeep(true);
    let fodder = fodder.expect("fodder staged");
    let start = START_LIFE;
    assert_eq!(
        runner.state().objects[&fodder].zone,
        Zone::Graveyard,
        "the other creature is sacrificed"
    );
    assert_eq!(
        runner.state().objects[&demon].zone,
        Zone::Battlefield,
        "the Demon is not the sacrifice"
    );
    assert!(
        !runner.state().objects[&demon].tapped,
        "CR 608.2c: a performed sacrifice means the fallback must not tap the Demon"
    );
    assert_eq!(
        runner.life(P1),
        start,
        "the controller must not lose 7 life after a successful sacrifice"
    );
    for opponent in [P0, P2] {
        assert_eq!(
            runner.life(opponent),
            start - FODDER_POWER,
            "each opponent loses life equal to the sacrificed creature's power"
        );
    }
}

#[test]
fn no_other_creature_taps_the_demon_and_costs_seven() {
    let (runner, demon, _) = p1_upkeep(false);
    assert!(
        runner.state().objects[&demon].tapped,
        "with nothing to sacrifice the Demon taps"
    );
    assert_eq!(
        runner.life(P1),
        START_LIFE - 7,
        "the controller loses 7 life"
    );
    for opponent in [P0, P2] {
        assert_eq!(runner.life(opponent), START_LIFE, "opponents lose nothing");
    }
}
