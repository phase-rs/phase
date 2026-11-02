//! #9660: a "where X is <quantity>" clause defines X (CR 107.3); it does not
//! replace the count that X appears in. Every card here is SYNTHETIC — no
//! printed card combines "twice / half / N times X" with "where X is" in an
//! enters-with count or a draw count.
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TWICE: &str = "This creature enters with twice X +1/+1 counters on it, where X is the \
                     number of lands you control.";
const THREE_TIMES: &str = "This creature enters with three times X +1/+1 counters on it, where X \
                           is the number of lands you control.";
const HALF_UP: &str = "This creature enters with half X +1/+1 counters on it, rounded up, where X \
                       is the number of lands you control.";
const HALF_DOWN: &str = "This creature enters with half X +1/+1 counters on it, rounded down, \
                         where X is the number of lands you control.";
const BARE: &str = "This creature enters with X +1/+1 counters on it, where X is the number of \
                    lands you control.";
const NO_X: &str = "This creature enters with two +1/+1 counters on it, where X is the number of \
                    lands you control.";
const DRAW_TWICE: &str = "Draw twice X cards, where X is the number of lands you control.";
const DRAW_HALF_UP: &str =
    "Draw half X cards, rounded up, where X is the number of lands you control.";

/// Three lands, so X = 3 for every line.
fn scenario_with_three_lands() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for _ in 0..3 {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    scenario
}

fn add_free_creature(scenario: &mut GameScenario, name: &str, oracle: &str) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, name, 0, 1, oracle)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

fn cast_all(runner: &mut GameRunner, ids: &[ObjectId]) {
    for &id in ids {
        runner.cast(id).resolve();
        runner.advance_until_stack_empty();
    }
}

fn plus_counters(runner: &GameRunner, id: ObjectId) -> Option<u32> {
    let obj = &runner.state().objects[&id];
    assert_eq!(
        obj.zone,
        Zone::Battlefield,
        "reach guard: {} resolved",
        obj.name
    );
    obj.counters.get(&CounterType::Plus1Plus1).copied()
}

/// CR 107.3 + CR 107.3i + CR 614.1c: "twice X … where X is N" enters with
/// 2·N counters and "three times X" with 3·N; the bare-X sibling (N) is the
/// reach guard that the where-X quantity itself reads 3.
#[test]
fn enters_with_where_x_keeps_the_multiplier_around_x() {
    let mut scenario = scenario_with_three_lands();
    let twice = add_free_creature(&mut scenario, "Twice Witness", TWICE);
    let three = add_free_creature(&mut scenario, "Three Times Witness", THREE_TIMES);
    let bare = add_free_creature(&mut scenario, "Bare Witness", BARE);
    let mut runner = scenario.build();
    cast_all(&mut runner, &[twice, three, bare]);

    assert_eq!(plus_counters(&runner, bare), Some(3));
    assert_eq!(plus_counters(&runner, twice), Some(6));
    assert_eq!(plus_counters(&runner, three), Some(9));
}

/// CR 107.1a + CR 107.3 + CR 107.3i + CR 614.1c: "half X …, rounded up/down,
/// where X is 3" enters with 2 / 1 counters — the noun-separated rounding
/// postmodifier survives.
#[test]
fn enters_with_half_where_x_keeps_its_rounding() {
    let mut scenario = scenario_with_three_lands();
    let up = add_free_creature(&mut scenario, "Half Up Witness", HALF_UP);
    let down = add_free_creature(&mut scenario, "Half Down Witness", HALF_DOWN);
    let mut runner = scenario.build();
    cast_all(&mut runner, &[up, down]);

    assert_eq!(plus_counters(&runner, up), Some(2));
    assert_eq!(plus_counters(&runner, down), Some(1));
}

/// CR 107.1a + CR 107.3i + CR 614.1c: the equal-to count must retain the
/// recipient's explicit rounding before X is bound. These are synthetic shapes.
#[test]
fn enters_with_equal_to_half_where_x_keeps_its_rounding() {
    const UP: &str = "This creature enters with a number of +1/+1 counters on it, rounded up, \
                      equal to half X, where X is the number of lands you control.";
    const DOWN: &str = "This creature enters with a number of +1/+1 counters on it, rounded down, \
                        equal to half X, where X is the number of lands you control.";
    let mut scenario = scenario_with_three_lands();
    let up = add_free_creature(&mut scenario, "Equal To Half Up Witness", UP);
    let down = add_free_creature(&mut scenario, "Equal To Half Down Witness", DOWN);
    let bare = add_free_creature(&mut scenario, "Bare Witness", BARE);
    let mut runner = scenario.build();
    cast_all(&mut runner, &[up, down, bare]);

    for id in [up, down, bare] {
        assert!(
            !runner.state().objects[&id].has_unimplemented_mechanics(),
            "the counter clause must parse without a gap"
        );
    }
    assert_eq!(plus_counters(&runner, bare), Some(3));
    assert_eq!(plus_counters(&runner, up), Some(2));
    assert_eq!(plus_counters(&runner, down), Some(1));
}

/// CR 107.3 + CR 107.3i + CR 614.1c: a count that prints no X keeps its
/// printed value; the bare-X sibling in the same game proves the where-X
/// clause was read (3).
#[test]
fn enters_with_where_x_leaves_a_count_without_x() {
    let mut scenario = scenario_with_three_lands();
    let no_x = add_free_creature(&mut scenario, "No X Witness", NO_X);
    let bare = add_free_creature(&mut scenario, "Bare Witness", BARE);
    let mut runner = scenario.build();
    cast_all(&mut runner, &[no_x, bare]);

    assert_eq!(plus_counters(&runner, bare), Some(3));
    assert_eq!(plus_counters(&runner, no_x), Some(2));
}

/// CR 107.3 + CR 107.3i + CR 614.1c: "where X is your Ring-bearer's power"
/// names another permanent's stat, not the entering creature's, so "twice X"
/// must not bind it as this creature's own power (2 here, which would give 4
/// counters). The line is unsupported and places no counters; the readable
/// twin on an identical 2/2 body in the same game enters with 2·3 = 6.
#[test]
fn enters_with_where_x_foreign_possessive_places_no_counters() {
    const TWICE_RING_BEARER: &str = "This creature enters with twice X +1/+1 counters on it, \
                                     where X is your Ring-bearer's power.";
    let mut scenario = scenario_with_three_lands();
    let mut add_two_two = |name: &str, oracle: &str| {
        scenario
            .add_creature_to_hand_from_oracle(P0, name, 2, 2, oracle)
            .with_mana_cost(ManaCost::generic(0))
            .id()
    };
    let possessive = add_two_two("Ring-bearer Witness", TWICE_RING_BEARER);
    let twice = add_two_two("Twice Witness", TWICE);
    let mut runner = scenario.build();
    cast_all(&mut runner, &[possessive, twice]);

    // Reach guard: the possessive line was read as an enters-with clause and
    // landed as an honest unimplemented gap (not silently dropped), while the
    // readable twin parsed fully.
    assert!(
        runner.state().objects[&possessive].has_unimplemented_mechanics(),
        "the foreign-possessive clause must surface as an unimplemented gap"
    );
    assert!(
        !runner.state().objects[&twice].has_unimplemented_mechanics(),
        "the readable twin must parse without a gap"
    );
    assert_eq!(plus_counters(&runner, twice), Some(6));
    assert_eq!(plus_counters(&runner, possessive), None);
}

/// CR 121.1 + CR 107.3c + CR 107.3i + CR 107.1a: the draw count position
/// already binds where-X inside the count (#9830); lock it. Not
/// revert-failing for #9660.
#[test]
fn draw_where_x_keeps_the_arithmetic_around_x() {
    for (oracle, drawn) in [(DRAW_TWICE, 6), (DRAW_HALF_UP, 2)] {
        let mut scenario = scenario_with_three_lands();
        for _ in 0..10 {
            scenario.add_card_to_library_top(P0, "Library Card");
        }
        let spell = scenario
            .add_spell_to_hand_from_oracle(P0, "Draw Witness", false, oracle)
            .with_mana_cost(ManaCost::generic(0))
            .id();
        let mut runner = scenario.build();
        runner.cast(spell).resolve().assert_hand_drawn(P0, drawn);
    }
}
