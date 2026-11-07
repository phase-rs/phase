//! Reviving Vapors: "Reveal the top three cards of your library and put one of
//! them into your hand. You gain life equal to that card's mana value. Put all
//! other cards revealed this way into your graveyard."
//!
//! CR 608.2c: the three sentences are instructions of ONE resolution, followed
//! in written order. Two bug classes are pinned here:
//!
//! * the chosen card must actually reach its owner's hand and the life total
//!   must rise by THAT card's mana value (the Demonstrative "that card" is the
//!   settled kept card, never one of the cards moved to the graveyard);
//! * the remainder is recognized as its own instruction, but simultaneous
//!   graveyard entry requires owner ordering under CR 404.3, kept as an honest
//!   unsupported gap.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const REVIVING_VAPORS: &str = "Reveal the top three cards of your library and put one of them into your hand. You gain life equal to that card's mana value. Put all other cards revealed this way into your graveyard.";

const ZIMONES_EXPERIMENT: &str = "Look at the top five cards of your library. You may reveal up to two creature and/or land cards from among them, then put the rest on the bottom of your library in a random order. Put all land cards revealed this way onto the battlefield tapped and put all creature cards revealed this way into your hand.";

/// Move `id` to the top of its owner's library (mirrors the reveal fixtures'
/// `put_library_top`).
fn put_library_top(runner: &mut GameRunner, id: ObjectId) {
    let owner = runner.state().objects[&id].owner;
    let zone = runner.state().objects[&id].zone;
    let mut events = Vec::new();
    if zone != Zone::Library {
        engine::game::zones::remove_from_zone(runner.state_mut(), id, zone, owner);
        runner.state_mut().objects.get_mut(&id).unwrap().zone = Zone::Library;
        runner.state_mut().players[owner.0 as usize]
            .library
            .push_back(id);
    }
    engine::game::zones::move_to_library_position(runner.state_mut(), id, true, &mut events);
}

/// Arrange `ids` as the top of their owner's library, `ids[0]` on top.
fn arrange_library_top_first(runner: &mut GameRunner, ids: &[ObjectId]) {
    for &id in ids.iter().rev() {
        put_library_top(runner, id);
    }
    let owner = runner.state().objects[&ids[0]].owner;
    let top: Vec<ObjectId> = runner.state().players[owner.0 as usize]
        .library
        .iter()
        .take(ids.len())
        .copied()
        .collect();
    assert_eq!(
        top, ids,
        "library top must be staged in the requested order"
    );
}

fn add_carded_creature(scenario: &mut GameScenario, name: &str, mana_value: u32) -> ObjectId {
    scenario
        .add_creature(P0, name, 1, 1)
        .with_mana_cost(ManaCost::generic(mana_value))
        .id()
}

fn hand_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

fn graveyard_ids(runner: &GameRunner) -> Vec<ObjectId> {
    runner.state().players[P0.0 as usize]
        .graveyard
        .iter()
        .copied()
        .collect()
}

fn life(runner: &GameRunner) -> i32 {
    runner.state().players[P0.0 as usize].life
}

/// Cast Reviving Vapors over the staged library and answer its keep prompt with
/// `keep`.
fn cast_and_keep(
    scenario: GameScenario,
    library_top_first: &[ObjectId],
    keep: ObjectId,
) -> GameRunner {
    let mut runner = scenario.build();
    arrange_library_top_first(&mut runner, library_top_first);
    let spell = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Reviving Vapors")
        .expect("Reviving Vapors starts in hand");
    runner.cast(spell).resolve();
    let WaitingFor::DigChoice { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Reviving Vapors' reveal must surface a keep choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        cards.contains(&keep),
        "the staged card must be among the revealed cards: {cards:?}"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![keep] })
        .expect("the keep selection is legal");
    runner
}

fn add_reviving_vapors(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(P0, "Reviving Vapors", false, REVIVING_VAPORS)
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

/// Reach guard: the printed spell parses to a real, castable chain (a gap
/// would leave the cast prompt absent and this test vacuous).
#[test]
fn reviving_vapors_parses_to_a_castable_reveal_chain() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = add_reviving_vapors(&mut scenario);
    let runner = scenario.build();
    let object = &runner.state().objects[&spell];
    assert!(
        !object.abilities.is_empty(),
        "the printed Oracle text must produce an ability"
    );
    assert!(
        matches!(
            &*object.abilities[0].effect,
            engine::types::ability::Effect::Dig { reveal: true, .. }
        ),
        "the head instruction is the reveal-three Dig: {:?}",
        object.abilities[0].effect
    );
}

/// CR 608.2c: the kept card reaches Hand and life rises by its mana value;
/// the graveyard remainder remains an explicit unsupported gap.
#[test]
fn reviving_vapors_puts_the_kept_card_in_hand_and_gains_its_mana_value() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_reviving_vapors(&mut scenario);
    let big = add_carded_creature(&mut scenario, "Hill Giant", 3);
    let small = add_carded_creature(&mut scenario, "Grizzly Bears", 2);
    let tiny = add_carded_creature(&mut scenario, "Savannah Lions", 1);

    let runner = cast_and_keep(scenario, &[big, small, tiny], big);

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the spell must have finished resolving, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the spell must have left the stack: {:?}",
        runner.state().stack
    );

    assert!(
        hand_ids(&runner).contains(&big),
        "the chosen card must be in its owner's hand: {:?}",
        hand_ids(&runner)
    );
    assert_eq!(
        runner.state().objects[&big].zone,
        Zone::Hand,
        "the chosen card's zone"
    );
    for (id, label) in [
        (small, "second revealed card"),
        (tiny, "third revealed card"),
    ] {
        assert!(
            !hand_ids(&runner).contains(&id),
            "the {label} must not be put into the hand"
        );
        // CR 404.3: moving multiple cards into the same graveyard simultaneously
        // requires an owner-ordering choice that the engine does not provide for
        // this mass remainder move. Rather than delivering in arbitrary engine
        // order, the remainder is an honest unsupported gap; the unselected
        // cards remain in the library.
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Library,
            "the {label} remains in library because the graveyard remainder is an honest gap"
        );
    }
    assert!(
        !graveyard_ids(&runner).contains(&big),
        "the kept card must not be swept into the graveyard: {:?}",
        graveyard_ids(&runner)
    );
    assert_eq!(
        life(&runner),
        23,
        "life equal to the kept card's mana value (3), not 0 and not the remainder's"
    );
}

/// CR 608.2c: the neighbouring class that shares the lowered shapes —
/// Zimone's Experiment routes the cards the player REVEALED by type. Its
/// remainder and its selection have the same types, so an implementation that
/// publishes the unselected pile instead of the selected one hands back the
/// wrong cards (or none at all when nothing was selected).
#[test]
fn zimones_experiment_routes_the_selected_cards_by_type() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Zimone's Experiment", false, ZIMONES_EXPERIMENT)
        .with_mana_cost(ManaCost::generic(0));
    // One untouched card beneath the looked-at five. It must finish ABOVE the
    // remainder, which is what separates "the remainder was put on the bottom"
    // from "the remainder never moved": a zone-only check cannot tell them
    // apart.
    let deeper = scenario.add_land_to_library_top(P0, "Island").id();
    // Four lands and one creature, so the selection and the remainder both hold
    // creature and/or land cards.
    let lands: Vec<ObjectId> = (0..4)
        .map(|_| scenario.add_land_to_library_top(P0, "Forest").id())
        .collect();
    let creature = add_carded_creature(&mut scenario, "Hill Giant", 3);

    let mut runner = scenario.build();
    put_library_top(&mut runner, creature);

    let spell = runner.state().players[P0.0 as usize]
        .hand
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Zimone's Experiment")
        .expect("Zimone's Experiment starts in hand");
    runner.cast(spell).resolve();
    let WaitingFor::DigChoice { cards, .. } = &runner.state().waiting_for else {
        panic!(
            "Zimone's Experiment must surface a reveal choice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        cards.contains(&creature) && lands.iter().all(|land| cards.contains(land)),
        "all five looked-at cards must be offered: {cards:?}"
    );
    let kept_land = lands[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![creature, kept_land],
        })
        .expect("revealing a creature and a land is legal");

    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the spell must have finished resolving, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "the spell must have left the stack: {:?}",
        runner.state().stack
    );
    assert_eq!(
        runner.state().objects[&creature].zone,
        Zone::Hand,
        "the revealed creature goes to its owner's hand"
    );
    assert_eq!(
        runner.state().objects[&kept_land].zone,
        Zone::Battlefield,
        "the revealed land goes to the battlefield"
    );
    assert!(
        runner.state().objects[&kept_land].tapped,
        "the revealed land enters tapped"
    );
    for &land in &lands[1..] {
        assert_eq!(
            runner.state().objects[&land].zone,
            Zone::Library,
            "a land that was not revealed stays in the library"
        );
    }

    // CR 401.4 + CR 608.2c: "put the rest on the bottom of your library" moves
    // the unselected lands to the bottom. Assert their POSITION, not just their
    // zone: each unselected land must have only unselected lands below it, and
    // the untouched deeper card must sit above all of them.
    let library: Vec<ObjectId> = runner.state().players[P0.0 as usize]
        .library
        .iter()
        .copied()
        .collect();
    let deeper_position = library
        .iter()
        .position(|&id| id == deeper)
        .unwrap_or_else(|| panic!("the untouched deeper card stays in the library: {library:?}"));
    for &land in &lands[1..] {
        let position = library
            .iter()
            .position(|&id| id == land)
            .unwrap_or_else(|| panic!("the unselected land stays in the library: {library:?}"));
        assert!(
            deeper_position < position,
            "the untouched deeper card must stay above the remainder: {library:?}"
        );
        assert!(
            library[position..].iter().all(|id| lands[1..].contains(id)),
            "each unselected land must sit at the bottom, below every non-remainder card: {library:?}"
        );
    }
}

/// CR 608.2c: with a short library the reveal keeps exactly the one card, and
/// the SINGLE remainder card must not become the "that card" the life gain
/// reads.
#[test]
fn reviving_vapors_life_gain_ignores_a_single_remainder_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_reviving_vapors(&mut scenario);
    let big = add_carded_creature(&mut scenario, "Hill Giant", 3);
    let small = add_carded_creature(&mut scenario, "Grizzly Bears", 2);

    let runner = cast_and_keep(scenario, &[big, small], big);

    assert!(hand_ids(&runner).contains(&big), "kept card in hand");
    assert_eq!(
        runner.state().objects[&small].zone,
        Zone::Library,
        "the unselected card remains in the library because graveyard remainder is an honest gap"
    );
    assert_eq!(
        life(&runner),
        23,
        "life must be the kept card's mana value, not the unselected card's"
    );
}
