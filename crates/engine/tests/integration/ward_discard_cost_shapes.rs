//! Ward discard costs keep their count, random selection and card-type filter
//! (phase-rs/phase#9999).
//!
//! The ward cost reader used to collapse every "Ward—Discard ..." clause to a
//! one-chosen-card discard (or, for a comma-separated card-type list, to a
//! compound with free mana legs). These tests drive the real pipeline: each
//! warded permanent is built from its verbatim Oracle text, an opponent's spell
//! targets it, the ward trigger resolves, and the opponent pays or fails the
//! discard through `GameAction`s.
//!
//! Oracle text (verbatim, MTGJSON):
//!   - Alpharael, Stonechosen: "Ward—Discard a card at random."
//!   - Saruman of Many Colors: "Ward—Discard an enchantment, instant, or sorcery card."
//!   - Arna Kennerüd, Skycaptain: "Ward—Discard a card."
//!
//! CR references:
//!   - CR 702.21a: Ward — counter the targeting spell or ability unless that
//!     player pays the ward cost.
//!   - CR 701.9a + CR 701.9b: a discard is chosen by the discarding player
//!     unless the effect requires a random discard.
//!   - CR 118.3: a cost cannot be paid without the resources to pay it fully.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, CardSelectionMode, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ALPHARAEL: &str = "Ward—Discard a card at random.\nVoid — Whenever Alpharael attacks, if a nonland permanent left the battlefield this turn or a spell was warped this turn, defending player loses half their life, rounded up.";
const SARUMAN: &str = "Ward—Discard an enchantment, instant, or sorcery card.\nWhenever you cast your second spell each turn, each opponent mills two cards. When one or more cards are milled this way, exile target enchantment, instant, or sorcery card with equal or lesser mana value than that spell from an opponent's graveyard. Copy the exiled card. You may cast the copy without paying its mana cost.";
const ARNA: &str = "Flying, lifelink\nWard—Discard a card.\nWhenever a modified creature you control attacks, double the number of each kind of counter on it. Then for each nontoken permanent attached to it, create a token that's a copy of that permanent attached to that creature.";
// Synthetic (issue #9999): no printed card has a counted ward discard.
const TWO_CARD_WARD: &str = "Ward—Discard two cards.";

/// A warded creature under P0 that P1 has just targeted with a destroy spell,
/// with the ward trigger resolved so the game is waiting on P1's payment.
struct WardedBoard {
    runner: GameRunner,
    warded: ObjectId,
    destroy: ObjectId,
    p1_hand: Vec<ObjectId>,
    p0_hand: Vec<ObjectId>,
}

/// `stage` seeds P1's hand (first element of the result) and P0's hand
/// (second element); P1 is the active player holding priority.
fn target_warded_creature(
    name: &str,
    toughness: i32,
    keywords: &[&str],
    oracle: &str,
    stage: impl FnOnce(&mut GameScenario) -> (Vec<ObjectId>, Vec<ObjectId>),
) -> WardedBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warded = scenario
        .add_creature(P0, name, 3, toughness)
        .from_oracle_text_with_keywords(keywords, oracle)
        .id();
    let destroy = scenario
        .add_spell_to_hand_from_oracle(P1, "Destroy Spell", true, "Destroy target creature.")
        .id();
    let (p1_hand, p0_hand) = stage(&mut scenario);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }
    runner.cast(destroy).target_objects(&[warded]).commit();
    runner.advance_until_stack_empty();
    WardedBoard {
        runner,
        warded,
        destroy,
        p1_hand,
        p0_hand,
    }
}

/// Reach guard: the ward trigger asked P1 to pay a discard cost. Returns the
/// cost for axis assertions.
fn assert_discard_payment_prompt(runner: &GameRunner) -> AbilityCost {
    let WaitingFor::UnlessPayment { player, cost, .. } = &runner.state().waiting_for else {
        panic!(
            "Ward must prompt the targeting opponent to pay a discard cost, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1, "the targeting spell's controller pays Ward");
    assert!(
        matches!(cost, AbilityCost::Discard { .. }),
        "Ward's cost must be a discard, got {cost:?}"
    );
    cost.clone()
}

fn pay(runner: &mut GameRunner) {
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("the opponent chooses to pay Ward");
}

fn zone_of(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// CR 702.21a + CR 118.3: an unpaid ward counters the targeting spell. The stack
/// has emptied, the destroy spell went to the graveyard without resolving, and
/// the warded creature is untouched.
fn assert_ward_countered_the_spell(board: &WardedBoard) {
    assert!(
        board.runner.state().stack.is_empty(),
        "the stack must have emptied after the ward payment, got {:?}",
        board.runner.state().stack
    );
    assert_eq!(
        zone_of(&board.runner, board.destroy),
        Zone::Graveyard,
        "the countered destroy spell goes to the graveyard"
    );
    assert_eq!(
        zone_of(&board.runner, board.warded),
        Zone::Battlefield,
        "the unpaid ward counters the destroy spell"
    );
}

/// CR 118.3 + CR 702.21a: Saruman's ward needs an enchantment, instant, or
/// sorcery card. A payer holding only a creature card cannot pay, so the
/// targeting spell is countered and nothing is discarded. The instant in the
/// warded permanent's controller's hand is not the payer's ("that player").
#[test]
fn saruman_ward_cannot_be_paid_with_a_creature_card() {
    let mut board = target_warded_creature(
        "Saruman of Many Colors",
        5,
        &["Mill", "Ward"],
        SARUMAN,
        |s| {
            let creature = s.add_creature_to_hand(P1, "Grizzly Bears", 2, 2).id();
            let controllers_instant = s.add_spell_to_hand(P0, "Shock", true).id();
            (vec![creature], vec![controllers_instant])
        },
    );

    let cost = assert_discard_payment_prompt(&board.runner);
    assert!(
        matches!(
            cost,
            AbilityCost::Discard {
                filter: Some(_),
                selection: CardSelectionMode::Chosen,
                ..
            }
        ),
        "Saruman's ward is one filtered, chosen discard, got {cost:?}"
    );

    pay(&mut board.runner);
    board.runner.advance_until_stack_empty();

    assert!(
        !matches!(
            board.runner.state().waiting_for,
            WaitingFor::WardDiscardChoice { .. }
        ),
        "a creature card must not be offered for a card-type-filtered discard, got {:?}",
        board.runner.state().waiting_for
    );
    assert_ward_countered_the_spell(&board);
    assert_eq!(zone_of(&board.runner, board.p1_hand[0]), Zone::Hand);
    assert_eq!(zone_of(&board.runner, board.p0_hand[0]), Zone::Hand);
}

/// CR 701.9a + CR 205.2a: with an instant and a creature card in hand, only the
/// instant is offered, and discarding it pays Ward so the spell resolves.
#[test]
fn saruman_ward_offers_only_cards_of_the_listed_types() {
    let mut board = target_warded_creature(
        "Saruman of Many Colors",
        5,
        &["Mill", "Ward"],
        SARUMAN,
        |s| {
            let creature = s.add_creature_to_hand(P1, "Grizzly Bears", 2, 2).id();
            let instant = s.add_spell_to_hand(P1, "Shock", true).id();
            (vec![creature, instant], vec![])
        },
    );
    let (creature, instant) = (board.p1_hand[0], board.p1_hand[1]);

    assert_discard_payment_prompt(&board.runner);
    pay(&mut board.runner);

    let WaitingFor::WardDiscardChoice { cards, .. } = &board.runner.state().waiting_for else {
        panic!(
            "a matching card in hand must raise the discard choice, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(cards, &vec![instant], "only the instant is eligible");

    board
        .runner
        .act(GameAction::SelectCards {
            cards: vec![instant],
        })
        .expect("discarding the instant pays Ward");
    board.runner.advance_until_stack_empty();

    assert_eq!(zone_of(&board.runner, instant), Zone::Graveyard);
    assert_eq!(zone_of(&board.runner, creature), Zone::Hand);
    assert_eq!(
        zone_of(&board.runner, board.warded),
        Zone::Graveyard,
        "the paid ward lets the destroy spell resolve"
    );
}

/// CR 701.9b: Alpharael's ward discard is random, so the payer is never asked
/// to choose; exactly one of the two cards in hand is discarded.
#[test]
fn alpharael_ward_discards_a_random_card_without_a_choice() {
    let mut board = target_warded_creature(
        "Alpharael, Stonechosen",
        3,
        &["Void", "Ward"],
        ALPHARAEL,
        |s| {
            let a = s.add_card_to_hand(P1, "Card A");
            let b = s.add_card_to_hand(P1, "Card B");
            (vec![a, b], vec![])
        },
    );
    let (a, b) = (board.p1_hand[0], board.p1_hand[1]);

    let cost = assert_discard_payment_prompt(&board.runner);
    assert!(
        matches!(
            cost,
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                selection: CardSelectionMode::Random,
                ..
            }
        ),
        "Alpharael's ward is one random discard, got {cost:?}"
    );
    let hand_before = board.runner.state().players[P1.0 as usize].hand.len();

    pay(&mut board.runner);

    assert!(
        !matches!(
            board.runner.state().waiting_for,
            WaitingFor::WardDiscardChoice { .. }
        ),
        "a random discard must never ask the payer to choose, got {:?}",
        board.runner.state().waiting_for
    );
    board.runner.advance_until_stack_empty();

    let hand_after = board.runner.state().players[P1.0 as usize].hand.len();
    assert_eq!(hand_after, hand_before - 1, "exactly one card is discarded");
    let in_graveyard = [a, b]
        .into_iter()
        .filter(|id| zone_of(&board.runner, *id) == Zone::Graveyard)
        .count();
    assert_eq!(
        in_graveyard, 1,
        "exactly one of the two cards was discarded"
    );
    assert_eq!(
        zone_of(&board.runner, board.warded),
        Zone::Graveyard,
        "the paid ward lets the destroy spell resolve"
    );
}

/// CR 118.3: with nothing to discard, the random discard cannot be paid and the
/// targeting spell is countered.
#[test]
fn alpharael_ward_with_an_empty_hand_counters_the_spell() {
    let mut board = target_warded_creature(
        "Alpharael, Stonechosen",
        3,
        &["Void", "Ward"],
        ALPHARAEL,
        |_| (vec![], vec![]),
    );

    assert_discard_payment_prompt(&board.runner);
    pay(&mut board.runner);
    board.runner.advance_until_stack_empty();

    assert_ward_countered_the_spell(&board);
}

/// CR 702.21a + CR 701.9a: a counted ward discard needs and takes that many
/// cards, one prompt per card.
#[test]
fn counted_ward_discard_takes_every_card() {
    let mut board = target_warded_creature("Two-Card Warder", 2, &["Ward"], TWO_CARD_WARD, |s| {
        let a = s.add_card_to_hand(P1, "Card A");
        let b = s.add_card_to_hand(P1, "Card B");
        (vec![a, b], vec![])
    });
    let (a, b) = (board.p1_hand[0], board.p1_hand[1]);

    assert_discard_payment_prompt(&board.runner);
    pay(&mut board.runner);

    let WaitingFor::WardDiscardChoice { remaining, .. } = &board.runner.state().waiting_for else {
        panic!(
            "expected a discard choice, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*remaining, 2, "both cards must be discarded");

    board
        .runner
        .act(GameAction::SelectCards { cards: vec![a] })
        .expect("first discard");
    let WaitingFor::WardDiscardChoice { remaining, .. } = &board.runner.state().waiting_for else {
        panic!(
            "expected a second discard choice, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*remaining, 1);
    board
        .runner
        .act(GameAction::SelectCards { cards: vec![b] })
        .expect("second discard");
    board.runner.advance_until_stack_empty();

    assert_eq!(zone_of(&board.runner, a), Zone::Graveyard);
    assert_eq!(zone_of(&board.runner, b), Zone::Graveyard);
    assert_eq!(zone_of(&board.runner, board.warded), Zone::Graveyard);
}

/// CR 118.3: one card in hand cannot pay a two-card ward discard.
#[test]
fn counted_ward_discard_with_too_few_cards_counters_the_spell() {
    let mut board = target_warded_creature("Two-Card Warder", 2, &["Ward"], TWO_CARD_WARD, |s| {
        (vec![s.add_card_to_hand(P1, "Card A")], vec![])
    });

    assert_discard_payment_prompt(&board.runner);
    pay(&mut board.runner);
    board.runner.advance_until_stack_empty();

    assert_ward_countered_the_spell(&board);
    assert_eq!(zone_of(&board.runner, board.p1_hand[0]), Zone::Hand);
}

/// Preservation: the plain "Ward—Discard a card." (13 printed cards) still
/// offers every card in hand for one chosen discard.
#[test]
fn plain_ward_discard_still_offers_every_hand_card() {
    let mut board = target_warded_creature(
        "Arna Kennerüd, Skycaptain",
        4,
        &["Double", "Flying", "Lifelink", "Ward"],
        ARNA,
        |s| {
            let a = s.add_card_to_hand(P1, "Card A");
            let b = s.add_card_to_hand(P1, "Card B");
            (vec![a, b], vec![])
        },
    );
    let (a, b) = (board.p1_hand[0], board.p1_hand[1]);

    assert_discard_payment_prompt(&board.runner);
    pay(&mut board.runner);

    let WaitingFor::WardDiscardChoice {
        cards, remaining, ..
    } = &board.runner.state().waiting_for
    else {
        panic!(
            "expected a discard choice, got {:?}",
            board.runner.state().waiting_for
        );
    };
    assert_eq!(*remaining, 1);
    let mut offered = cards.clone();
    offered.sort();
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(offered, expected, "every hand card may pay a plain discard");
}
