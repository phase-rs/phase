//! The Borg Queen — `assimilate` of the card an exile-until loop found, then
//! "the rest" on the bottom, driven through the REAL cast pipeline.
//!
//! Oracle (verbatim, Scryfall-verified):
//!   "Artifact creatures you control get +2/+2.
//!    Whenever The Borg Queen enters or attacks, target opponent exiles cards
//!    from the top of their library until they exile a creature card.
//!    Assimilate that card. They put the rest on the bottom in a random order.
//!    (To assimilate a card, put it onto the battlefield under your control with
//!    a +1/+1 counter. It's a Borg artifact creature and loses all other
//!    creature types.)"
//!
//! Clause by clause:
//!   * CR 701.13a + CR 608.2c: the opponent exiles from the top of THEIR
//!     library one card at a time and stops on the first creature card.
//!   * "Assimilate that card": the found card — not a graveyard target — enters
//!     under OUR control (CR 110.2a) with a +1/+1 counter (CR 122.1) as a Borg
//!     artifact creature that keeps no other creature type (CR 205.1a/205.1b).
//!   * CR 401.4 + CR 608.2c: "the rest" is every other card that loop exiled;
//!     they go to the bottom of their owner's library in a random order. The
//!     found card is not among them.
//!
//! The graveyard form of `assimilate` (Borg Queen, Perfection Manifest) and its
//! full CR 205.1b table live in `borg_queen_assimilate.rs`.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// The Borg Queen's verbatim printed Oracle text, reminder text included, so the
/// real reminder-stripping path runs.
const THE_BORG_QUEEN: &str = "Artifact creatures you control get +2/+2.\n\
     Whenever The Borg Queen enters or attacks, target opponent exiles cards from the top \
     of their library until they exile a creature card. Assimilate that card. They put the \
     rest on the bottom in a random order. (To assimilate a card, put it onto the battlefield \
     under your control with a +1/+1 counter. It's a Borg artifact creature and loses all \
     other creature types.)";

/// The Borg Queen in P0's hand at zero cost; the cost is not under test.
fn the_borg_queen_in_hand(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_to_hand_from_oracle(P0, "The Borg Queen", 4, 4, THE_BORG_QUEEN)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// `RemoveAllSubtypes { Creature }` only strips subtypes listed in
/// `all_creature_types`, which `GameScenario` leaves empty; seed it so the
/// "loses all other creature types" assertion is not vacuous.
fn seed_creature_types(runner: &mut GameRunner) {
    runner.state_mut().all_creature_types = vec![
        "Human".to_string(),
        "Soldier".to_string(),
        "Borg".to_string(),
        "Noble".to_string(),
    ];
}

fn relayer(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
}

fn library(runner: &GameRunner, player: engine::types::player::PlayerId) -> Vec<ObjectId> {
    runner.state().library_of(player).iter().copied().collect()
}

/// The found creature is assimilated and every other exiled card goes to the
/// bottom of the opponent's library, under the card that was never exiled.
#[test]
fn the_borg_queen_assimilates_the_found_creature_and_bottoms_the_rest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Built bottom-first: each helper puts its card on top. Top-first order is
    // [miss_a, miss_b, found, untouched].
    let untouched = scenario
        .add_land_to_library_top(P1, "Untouched Forest")
        .id();
    let found = scenario
        .add_creature_to_library_top(P1, "Library Soldier", 2, 2)
        .with_subtypes(vec!["Human", "Soldier"])
        .id();
    let miss_b = scenario.add_land_to_library_top(P1, "Missed Island").id();
    let miss_a = scenario.add_land_to_library_top(P1, "Missed Plains").id();
    let queen = the_borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    let outcome = runner.cast(queen).target_players(&[P1]).resolve();

    // REACH-GUARD: the found card left the library and exile for the
    // battlefield. Every assertion below is meaningful only because this holds.
    outcome.assert_zone(&[found], Zone::Battlefield);

    // CR 110.2a + CR 108.3: under our control, still the opponent's card.
    let found_obj = &runner.state().objects[&found];
    assert_eq!(
        found_obj.controller, P0,
        "CR 110.2a: enters under our control"
    );
    assert_eq!(found_obj.owner, P1, "CR 108.3: ownership never changes");

    relayer(&mut runner);
    let found_obj = &runner.state().objects[&found];
    // CR 122.1: exactly one +1/+1 counter.
    assert_eq!(
        found_obj
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied()
            .unwrap_or(0),
        1,
        "CR 122.1: the assimilated card enters with one +1/+1 counter"
    );
    // CR 205.1a + CR 205.1b: Borg artifact creature, no other creature type.
    let subtypes = &found_obj.card_types.subtypes;
    assert!(
        subtypes.iter().any(|s| s == "Borg"),
        "gains Borg: {subtypes:?}"
    );
    assert!(
        !subtypes.iter().any(|s| s == "Human" || s == "Soldier"),
        "CR 205.1b: other creature types are replaced: {subtypes:?}"
    );
    assert!(found_obj
        .card_types
        .core_types
        .contains(&CoreType::Artifact));
    assert!(found_obj
        .card_types
        .core_types
        .contains(&CoreType::Creature));
    // CR 613.4c: printed 2/2 + counter (1,1) + the Queen's +2/+2 artifact lord.
    assert_eq!(
        (found_obj.power, found_obj.toughness),
        (Some(5), Some(5)),
        "printed 2/2 + (1,1) counter + (2,2) from the artifact-creature lord"
    );

    // CR 401.4 + CR 608.2c: "the rest" — both missed cards — are on the bottom,
    // beneath the card the loop never reached; nothing stays in exile.
    let lib = library(&runner, P1);
    assert_eq!(
        lib.len(),
        3,
        "the two missed cards return to the library: {lib:?}"
    );
    assert_eq!(
        lib[0], untouched,
        "the never-exiled card is now on top: {lib:?}"
    );
    let bottom: Vec<ObjectId> = lib[1..].to_vec();
    assert!(
        bottom.contains(&miss_a) && bottom.contains(&miss_b),
        "both missed cards are on the bottom: {lib:?}"
    );
    outcome.assert_zone(&[miss_a, miss_b], Zone::Library);
    assert!(
        runner.state().exile.is_empty(),
        "no card stays in exile: {:?}",
        runner.state().exile
    );
}

/// With no creature card in the library, the loop exiles everything, nothing
/// is assimilated, and "the rest" is the whole exiled pile.
#[test]
fn the_borg_queen_without_a_creature_bottoms_every_exiled_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land_b = scenario.add_land_to_library_top(P1, "Only Island").id();
    let land_a = scenario.add_land_to_library_top(P1, "Only Plains").id();
    let queen = the_borg_queen_in_hand(&mut scenario);
    let mut runner = scenario.build();
    seed_creature_types(&mut runner);

    let outcome = runner.cast(queen).target_players(&[P1]).resolve();

    // REACH-GUARD: the Queen resolved, so her enters trigger ran.
    outcome.assert_zone(&[queen], Zone::Battlefield);
    outcome.assert_zone(&[land_a, land_b], Zone::Library);
    assert_eq!(library(&runner, P1).len(), 2);
    assert!(
        runner.state().exile.is_empty(),
        "every exiled card returns to the library: {:?}",
        runner.state().exile
    );
    assert_eq!(
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner.state().objects[id].controller == P0)
            .count(),
        1,
        "nothing but the Queen entered under our control"
    );
}
