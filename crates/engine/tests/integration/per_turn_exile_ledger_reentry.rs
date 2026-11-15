//! CR 400.7 + CR 607.2a: the cards exiled with an object this turn belong to
//! that object. A permanent that leaves the battlefield and returns is a new
//! object, so "cards exiled with Maralen this turn" are only the cards the
//! returned Maralen's own trigger exiled.
//!
//! CR 400.7: a permission source's "once each turn" belongs to the object
//! too, so the returned object may use it again that turn.

use engine::game::casting::can_cast_object_now;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const MARALEN_FAE_ASCENDANT: &str = "Flying\nWhenever Maralen or another Elf or Faerie you control enters, exile the top two cards of target opponent's library.\nOnce each turn, you may cast a spell with mana value less than or equal to the number of Elves and Faeries you control from among cards exiled with Maralen this turn without paying its mana cost.";
const MULDROTHA: &str = "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard. (If a card has multiple permanent types, choose one as you play it.)";
const GHOSTLY_FLICKER: &str = "Exile two target artifacts, creatures, and/or lands you control, then return those cards to the battlefield under your control.";

struct MaralenBoard {
    runner: GameRunner,
    maralen: ObjectId,
    land: ObjectId,
    flickers: Vec<ObjectId>,
    /// P1's library, top first: each Maralen trigger exiles the next two.
    library_top_first: Vec<ObjectId>,
}

/// Maralen (verbatim) and a blank land on P0's battlefield, `flicker_count`
/// zero-cost Ghostly Flickers in P0's hand, and six zero-cost spells on top
/// of P1's library, so every exiled card's mana value is within Maralen's
/// Elf-and-Faerie count.
fn maralen_board(flicker_count: usize) -> MaralenBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let maralen = scenario
        .add_creature(P0, "Maralen, Fae Ascendant", 4, 5)
        .as_legendary()
        .with_subtypes(vec!["Elf", "Faerie", "Noble"])
        .from_oracle_text(MARALEN_FAE_ASCENDANT)
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let flickers = (0..flicker_count)
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
                .with_mana_cost(ManaCost::zero())
                .id()
        })
        .collect();
    let mut library_top_first: Vec<ObjectId> = (0..6)
        .map(|index| {
            scenario
                .add_spell_to_library_top(P1, &format!("Free Spell {index}"), false)
                .with_mana_cost(ManaCost::zero())
                .from_oracle_text("Draw a card.")
                .id()
        })
        .collect();
    library_top_first.reverse();
    // P0's library, so a cast pile spell's "Draw a card." has a card to draw.
    scenario.add_card_to_library_top(P0, "Spare Card");
    MaralenBoard {
        runner: scenario.build(),
        maralen,
        land,
        flickers,
        library_top_first,
    }
}

/// Casts `flicker` on Maralen and the land, then passes priority (choosing P1
/// for the re-entry trigger's target) until P0 holds priority on an empty stack.
fn flicker_maralen(board: &mut MaralenBoard, flicker: ObjectId) {
    board
        .runner
        .cast(flicker)
        .target_objects(&[board.maralen, board.land])
        .commit();
    for _ in 0..32 {
        let action = match &board.runner.state().waiting_for {
            WaitingFor::Priority { .. } if board.runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                GameAction::PassPriority
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(P1)),
                }
            }
            other => panic!("unexpected prompt: {other:?}"),
        };
        board
            .runner
            .act(action)
            .expect("the engine accepts the action");
    }
    panic!(
        "the flicker never finished resolving; waiting for {:?}",
        board.runner.state().waiting_for
    );
}

fn assert_in_exile(runner: &GameRunner, cards: &[ObjectId], what: &str) {
    for card in cards {
        assert_eq!(
            runner.state().objects[card].zone,
            Zone::Exile,
            "reach-guard: {what} {card:?} is in exile"
        );
    }
}

fn castable(runner: &GameRunner, card: ObjectId) -> bool {
    can_cast_object_now(runner.state(), P0, card)
}

/// CR 400.7 + CR 607.2a: Maralen is blinked twice in one main phase. The live
/// Maralen is the second returned object, so only the pile its own trigger
/// exiled is castable; the first returned Maralen's pile is not.
#[test]
fn blinked_maralen_casts_only_from_its_own_pile() {
    let mut board = maralen_board(2);
    let start_incarnation = board.runner.state().objects[&board.maralen].incarnation;
    let pile_one = board.library_top_first[0..2].to_vec();
    let pile_two = board.library_top_first[2..4].to_vec();

    let first = board.flickers[0];
    flicker_maralen(&mut board, first);
    assert_in_exile(&board.runner, &pile_one, "pile 1");
    let second = board.flickers[1];
    flicker_maralen(&mut board, second);
    assert_in_exile(&board.runner, &pile_two, "pile 2");
    assert_in_exile(&board.runner, &pile_one, "pile 1");
    let maralen = &board.runner.state().objects[&board.maralen];
    assert_eq!(
        maralen.zone,
        Zone::Battlefield,
        "reach-guard: Maralen is back"
    );
    assert_eq!(
        maralen.incarnation,
        start_incarnation + 4,
        "reach-guard: two blinks made Maralen a new object four times over"
    );

    for card in &pile_two {
        assert!(
            castable(&board.runner, *card),
            "CR 607.2a: the live Maralen's own pile is castable ({card:?})"
        );
    }
    for card in &pile_one {
        assert!(
            !castable(&board.runner, *card),
            "CR 400.7 + CR 607.2a: the departed Maralen's pile is not castable ({card:?})"
        );
    }
}

/// Positive control: one blink, and the returned Maralen's pile is castable.
#[test]
fn blinked_maralen_casts_from_its_reentry_pile() {
    let mut board = maralen_board(1);
    let pile = board.library_top_first[0..2].to_vec();

    let flicker = board.flickers[0];
    flicker_maralen(&mut board, flicker);
    assert_in_exile(&board.runner, &pile, "the re-entry pile");

    for card in &pile {
        assert!(
            castable(&board.runner, *card),
            "CR 607.2a: the returned Maralen's pile is castable ({card:?})"
        );
    }
}

/// CR 400.7: Maralen's "once each turn" belongs to the object. The returned
/// Maralen spends it, is blinked again, and the newest Maralen may still cast
/// from its own pile that turn.
#[test]
fn blinked_maralen_gets_its_own_once_each_turn_cast() {
    let mut board = maralen_board(2);
    let start_incarnation = board.runner.state().objects[&board.maralen].incarnation;
    let pile_one = board.library_top_first[0..2].to_vec();
    let pile_two = board.library_top_first[2..4].to_vec();

    let first = board.flickers[0];
    flicker_maralen(&mut board, first);
    assert_in_exile(&board.runner, &pile_one, "pile 1");
    board.runner.cast(pile_one[0]).resolve();
    assert_eq!(
        board.runner.state().objects[&pile_one[0]].zone,
        Zone::Graveyard,
        "reach-guard: pile 1's spell was cast and resolved"
    );
    assert!(
        board
            .runner
            .state()
            .exile_cast_permissions_used
            .contains(&board.maralen),
        "reach-guard: the cast spent Maralen's once each turn"
    );
    assert!(
        !castable(&board.runner, pile_one[1]),
        "reach-guard: the spent once blocks a second cast from the same Maralen"
    );

    let second = board.flickers[1];
    flicker_maralen(&mut board, second);
    assert_in_exile(&board.runner, &pile_two, "pile 2");
    let maralen = &board.runner.state().objects[&board.maralen];
    assert_eq!(
        maralen.zone,
        Zone::Battlefield,
        "reach-guard: Maralen is back"
    );
    assert_eq!(
        maralen.incarnation,
        start_incarnation + 4,
        "reach-guard: two blinks made Maralen a new object four times over"
    );

    assert!(
        castable(&board.runner, pile_two[0]),
        "CR 400.7: the newest Maralen has its own once each turn ({:?})",
        pile_two[0]
    );
}

/// CR 400.7: Muldrotha's per-type slots belong to the object (ruling
/// 2020-11-10: a new Muldrotha may play another card of a type already used
/// that turn). Muldrotha casts a creature from the graveyard, is blinked, and
/// the returned Muldrotha may cast another creature from the graveyard.
#[test]
fn blinked_muldrotha_gets_its_own_creature_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = scenario
        .add_creature_from_oracle(P0, "Muldrotha, the Gravetide", 6, 6, MULDROTHA)
        .as_legendary()
        .id();
    let land = scenario.add_land_from_oracle(P0, "Blank Land", "").id();
    let flicker = scenario
        .add_spell_to_hand_from_oracle(P0, "Ghostly Flicker", true, GHOSTLY_FLICKER)
        .with_mana_cost(ManaCost::zero())
        .id();
    let [first_bear, second_bear] = ["First Bear", "Second Bear"].map(|name| {
        scenario
            .add_creature_to_graveyard(P0, name, 2, 2)
            .with_mana_cost(ManaCost::zero())
            .id()
    });
    let mut runner = scenario.build();
    let start_incarnation = runner.state().objects[&muldrotha].incarnation;

    runner.cast(first_bear).resolve();
    assert_eq!(
        runner.state().objects[&first_bear].zone,
        Zone::Battlefield,
        "reach-guard: the first Bear was cast from the graveyard and resolved"
    );
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "reach-guard: the cast spent Muldrotha's creature slot"
    );
    assert!(
        !castable(&runner, second_bear),
        "reach-guard: the spent slot blocks a second creature from the same Muldrotha"
    );

    runner
        .cast(flicker)
        .target_objects(&[muldrotha, land])
        .resolve();
    let muldrotha_obj = &runner.state().objects[&muldrotha];
    assert_eq!(
        muldrotha_obj.zone,
        Zone::Battlefield,
        "reach-guard: Muldrotha is back"
    );
    assert_eq!(
        muldrotha_obj.incarnation,
        start_incarnation + 2,
        "reach-guard: the blink made Muldrotha a new object"
    );
    assert_eq!(
        runner.state().objects[&second_bear].zone,
        Zone::Graveyard,
        "reach-guard: the second Bear is still in the graveyard"
    );

    assert!(
        castable(&runner, second_bear),
        "CR 400.7: the returned Muldrotha has its own creature slot"
    );
}
