//! phase-rs/phase#7651: a Vibranium token's mana must not pay for a
//! nonartifact spell.
//!
//! T'Challa prints the token's mana ability only as reminder text, so the
//! ability (and its spend restriction) comes from the token catalog at
//! creation time. These tests drive that whole chain through the production
//! pipeline: cast the creator, resolve its trigger, then try to spend the
//! token's mana, both via auto-tap and from the pool.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TCHALLA_ORACLE: &str = "Whenever T'Challa enters or attacks, create a tapped Vibranium token. (It's an artifact with indestructible and \"{T}: Add {C}. This mana can't be spent to cast a nonartifact spell.\")\nWhenever you cast an artifact spell with mana value 4 or greater, put two +1/+1 counters on T'Challa.";

struct Board {
    runner: GameRunner,
    token: ObjectId,
    creature: ObjectId,
    artifact: ObjectId,
}

/// Cast T'Challa, resolve its trigger, and untap the Vibranium it creates, the
/// only mana source on the board. The two {1} spells in hand differ only in
/// core type, so the artifact half of each test is the reach guard.
fn board() -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let tchalla = scenario
        .add_creature_to_hand_from_oracle(P0, "T'Challa, the Black Panther", 2, 2, TCHALLA_ORACLE)
        .id();
    let creature = scenario
        .add_creature_to_hand(P0, "Nonartifact Probe", 2, 2)
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let artifact = scenario
        .add_artifact_to_hand_from_oracle(P0, "Artifact Probe", "")
        .with_mana_cost(ManaCost::generic(1))
        .id();
    let mut runner = scenario.build();

    runner.cast(tchalla).resolve();
    runner.advance_until_stack_empty();
    let tokens: Vec<_> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].name == "Vibranium")
        .collect();
    assert_eq!(tokens.len(), 1, "T'Challa must create a Vibranium token");
    let token = tokens[0];
    runner.state_mut().objects.get_mut(&token).unwrap().tapped = false;
    Board {
        runner,
        token,
        creature,
        artifact,
    }
}

/// The nonartifact spell is not offered, and its cast fails at payment
/// (CR 601.2h), leaving it in hand.
fn assert_creature_unpayable(board: &mut Board) {
    assert!(!can_cast_object_now(
        board.runner.state(),
        P0,
        board.creature
    ));
    let Err(error) = board.runner.cast(board.creature).try_resolve() else {
        panic!("Vibranium mana must not pay for a nonartifact spell");
    };
    assert!(
        error.to_string().contains("Cannot pay mana cost"),
        "rejected for an unrelated reason: {error}"
    );
    assert_eq!(
        board.runner.state().objects[&board.creature].zone,
        Zone::Hand
    );
}

/// CR 106.6 + CR 111.3: auto-tap won't tap Vibranium for a nonartifact spell,
/// but will for an artifact spell of the same cost.
#[test]
fn vibranium_auto_tap_pays_for_artifact_spells_only() {
    let mut board = board();

    assert_creature_unpayable(&mut board);
    assert!(!board.runner.state().objects[&board.token].tapped);

    assert!(can_cast_object_now(
        board.runner.state(),
        P0,
        board.artifact
    ));
    let outcome = board.runner.cast(board.artifact).resolve();
    outcome.assert_zone(&[board.artifact], Zone::Battlefield);
    assert!(outcome.state().objects[&board.token].tapped);
}

/// CR 106.6 + CR 111.3: the {C} Vibranium adds to the pool carries the
/// restriction, so floating mana can't pay for a nonartifact spell but can pay
/// for an artifact spell of the same cost.
#[test]
fn vibranium_floating_mana_pays_for_artifact_spells_only() {
    let mut board = board();

    board.runner.activate(board.token, 0).resolve();
    assert!(board.runner.state().objects[&board.token].tapped);
    let pool = &board.runner.state().players[0].mana_pool;
    assert_eq!(pool.total(), 1, "{pool:?}");
    assert_eq!(pool.count_color(ManaType::Colorless), 1, "{pool:?}");

    assert_creature_unpayable(&mut board);
    assert_eq!(board.runner.state().players[0].mana_pool.total(), 1);

    assert!(can_cast_object_now(
        board.runner.state(),
        P0,
        board.artifact
    ));
    let outcome = board.runner.cast(board.artifact).resolve();
    outcome.assert_zone(&[board.artifact], Zone::Battlefield);
    assert_eq!(outcome.state().players[0].mana_pool.total(), 0);
}
