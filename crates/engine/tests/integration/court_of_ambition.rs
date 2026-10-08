//! Court of Ambition — the monarch-gated per-opponent punisher.
//!
//! Oracle text (verbatim, Scryfall `cmr` #114):
//!   "When this enchantment enters, you become the monarch.
//!    At the beginning of your upkeep, each opponent loses 3 life unless they
//!    discard a card. If you're the monarch, instead each opponent loses 6 life
//!    unless they discard two cards."
//!
//! Two axes meet on this card and neither works without the other:
//!
//!   1. **Per-opponent unless-costs** (CR 118.12a + CR 608.2f). The upkeep
//!      trigger fans out over `player_scope: Opponent`, and each iteration's
//!      unless-payer is that iteration's scoped opponent — so every opponent
//!      independently chooses to discard or to lose the life. A payer bound to
//!      the controller (or to only the first opponent) would let one decision
//!      speak for the table.
//!
//!   2. **The monarch "instead" swap** (CR 614.15 + CR 608.2c + CR 725.1). The
//!      rider replaces BOTH halves of the printed instruction — the life total
//!      AND the unless-cost. An additive rider would drain 3 *and* 6, and a
//!      rider that swapped only the life amount would still ask for one card.
//!
//! ROOT CAUSE this file pins: `parse_unless_they_discard_cost` hard-coded a
//! count of one and only accepted the singular article, while its `you`-payer
//! mirror had grown a numeric-count axis. "unless they discard two cards" fell
//! out of the grammar entirely, so the whole monarch branch lowered to
//! `Effect::Unimplemented { name: "Unsupported unless clause" }` — the monarch
//! half of the card silently did nothing. Both payer forms now share one
//! authority (`parse_unless_discard_cost_phrase`).
//!
//! These tests drive the REAL pipeline: the card is built from its verbatim
//! Oracle text and the upkeep trigger is fired by advancing the turn, so a
//! regression in parsing, fan-out, swap, or payment all surface here.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, QuantityExpr};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// Verbatim printed Oracle text — the fixture must never paraphrase, because
/// the clause boundary ("… a card. If you're the monarch, instead …") is
/// precisely what routes the second sentence to the "instead" branch builder.
const COURT_OF_AMBITION: &str = "When this enchantment enters, you become the monarch.\n\
     At the beginning of your upkeep, each opponent loses 3 life unless they discard a card. \
     If you're the monarch, instead each opponent loses 6 life unless they discard two cards.";

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// `player_count`-seat game with Court of Ambition already on P0's battlefield
/// and `opponent_hand` discardable cards in every opponent's hand. `monarch` seeds
/// the designation directly — a scenario-seeded permanent never fires its own
/// ETB, so the "you become the monarch" half is covered separately by
/// `court_of_ambition_etb_makes_its_controller_the_monarch`.
fn build_runner(player_count: u8, monarch: Option<PlayerId>, opponent_hand: usize) -> GameRunner {
    let mut scenario = GameScenario::new_n_player(player_count, 42);
    scenario.at_phase(Phase::Untap);
    scenario.add_enchantment_from_oracle(P0, "Court of Ambition", COURT_OF_AMBITION);

    for seat in 1..player_count {
        let pid = PlayerId(seat);
        let names: Vec<String> = (0..opponent_hand)
            .map(|i| format!("Filler Card {seat}-{i}"))
            .collect();
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        scenario.with_cards_in_hand(pid, &refs);
    }

    let mut runner = scenario.build();
    runner.state_mut().turn_number = 2;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().monarch = monarch;
    runner
}

/// Drive to P0's upkeep and let the trigger resolve until it pauses on the
/// first opponent's unless-payment prompt (CR 118.12a).
fn fire_upkeep(runner: &mut GameRunner) {
    runner.advance_to_upkeep();
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            return;
        }
        if runner.state().stack.is_empty() && runner.state().phase != Phase::Upkeep {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    panic!(
        "upkeep trigger never surfaced an unless-payment prompt: {:?}",
        runner.state().waiting_for
    );
}

/// Assert the pending prompt is an unless-discard of `expected_count` cards
/// owed by `expected_payer`. CR 118.12a: the payer is the scoped opponent of
/// this fan-out iteration, never the controller.
fn expect_discard_prompt(runner: &GameRunner, expected_payer: PlayerId, expected_count: i32) {
    match &runner.state().waiting_for {
        WaitingFor::UnlessPayment { player, cost, .. } => {
            assert_eq!(
                *player, expected_payer,
                "the scoped opponent pays their own unless-cost, not the controller"
            );
            match cost {
                AbilityCost::Discard { count, filter, .. } => {
                    assert_eq!(
                        *count,
                        QuantityExpr::Fixed {
                            value: expected_count
                        },
                        "unless-cost card count"
                    );
                    assert!(
                        filter.is_none(),
                        "any card may be discarded, got {filter:?}"
                    );
                }
                other => panic!("expected a Discard unless-cost, got {other:?}"),
            }
        }
        other => panic!("expected UnlessPayment prompt, got {other:?}"),
    }
}

/// Pay the pending unless-discard by submitting one card per re-prompt round
/// trip, until the discard loop is exhausted.
fn pay_discard(runner: &mut GameRunner) {
    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("choosing to pay the discard cost must be accepted");
    for _ in 0..8 {
        let card = match &runner.state().waiting_for {
            WaitingFor::WardDiscardChoice { cards, .. } => {
                *cards.first().expect("an eligible card to discard")
            }
            _ => return,
        };
        runner
            .act(GameAction::SelectCards { cards: vec![card] })
            .expect("discard selection must be accepted");
    }
    panic!("discard loop did not terminate");
}

/// CR 118.12a: declining the discard makes the effect happen — the opponent
/// loses 3 life and keeps their hand.
#[test]
fn court_of_ambition_non_monarch_opponent_declining_loses_three() {
    let mut runner = build_runner(2, None, 3);
    fire_upkeep(&mut runner);
    expect_discard_prompt(&runner, P1, 1);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(life(&runner, P1), 17, "declining costs the opponent 3 life");
    assert_eq!(
        hand_size(&runner, P1),
        3,
        "a declined cost discards nothing"
    );
    assert_eq!(life(&runner, P0), 20, "the controller is never the subject");
}

/// CR 118.12a: paying the discard prevents the life loss entirely.
#[test]
fn court_of_ambition_non_monarch_opponent_paying_discards_one_and_keeps_life() {
    let mut runner = build_runner(2, None, 3);
    fire_upkeep(&mut runner);
    expect_discard_prompt(&runner, P1, 1);

    pay_discard(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1),
        20,
        "a paid unless-cost prevents the loss"
    );
    assert_eq!(hand_size(&runner, P1), 2, "exactly one card was discarded");
}

/// CR 614.15 + CR 725.1: while the controller is the monarch, the rider
/// REPLACES the printed instruction — the demand becomes two cards, and
/// declining costs 6 life, not 3 and not 9 (which is what an additive rider
/// would produce).
#[test]
fn court_of_ambition_monarch_branch_demands_two_cards_and_drains_six() {
    let mut runner = build_runner(2, Some(P0), 3);
    fire_upkeep(&mut runner);
    expect_discard_prompt(&runner, P1, 2);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1),
        14,
        "the monarch branch REPLACES the 3-life branch: 6 total, not 3 and not 9"
    );
    assert_eq!(
        hand_size(&runner, P1),
        3,
        "a declined cost discards nothing"
    );
}

/// CR 109.5 + CR 725.1: "you're the monarch" on this ability means its
/// controller, not merely that any player is monarch.
#[test]
fn court_of_ambition_opponent_monarch_uses_non_monarch_branch() {
    let mut runner = build_runner(2, Some(P1), 3);
    fire_upkeep(&mut runner);
    expect_discard_prompt(&runner, P1, 1);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1),
        17,
        "only the controller's monarch status applies"
    );
    assert_eq!(
        hand_size(&runner, P1),
        3,
        "a declined cost discards nothing"
    );
}

/// CR 614.15 + CR 701.9: paying the monarch branch costs exactly two cards.
#[test]
fn court_of_ambition_monarch_branch_paid_with_two_discards_keeps_life() {
    let mut runner = build_runner(2, Some(P0), 3);
    fire_upkeep(&mut runner);
    expect_discard_prompt(&runner, P1, 2);

    pay_discard(&mut runner);
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1),
        20,
        "a paid unless-cost prevents the loss"
    );
    assert_eq!(
        hand_size(&runner, P1),
        1,
        "exactly two cards were discarded"
    );
}

/// CR 118.3 + CR 118.12a: "A player can't pay a cost without having the
/// necessary resources to pay it fully" — an opponent who cannot produce the
/// full count cannot partially pay, so the effect happens. With the monarch
/// branch demanding two cards and only one in hand, the life loss happens and
/// the card stays put.
#[test]
fn court_of_ambition_monarch_branch_is_unpayable_with_one_card() {
    let mut runner = build_runner(2, Some(P0), 1);
    fire_upkeep(&mut runner);
    expect_discard_prompt(&runner, P1, 2);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting an unpayable cost must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        life(&runner, P1),
        14,
        "an unpayable cost lets the loss happen"
    );
    assert_eq!(
        hand_size(&runner, P1),
        1,
        "the lone card must not be taken as a partial payment"
    );
}

/// CR 608.2f + CR 101.4: every opponent is polled independently, in APNAP
/// order, and each decision binds only to that opponent. This is the assertion
/// that a controller-bound (or first-opponent-bound) payer cannot satisfy:
/// P1 pays and keeps their life, P2 declines and loses it.
#[test]
fn court_of_ambition_polls_each_opponent_independently() {
    let mut runner = build_runner(3, None, 3);
    fire_upkeep(&mut runner);

    // First iteration: P1, in APNAP order after the active player P0.
    expect_discard_prompt(&runner, P1, 1);
    pay_discard(&mut runner);

    // The fan-out continuation must now surface P2's own prompt.
    for _ in 0..10 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    expect_discard_prompt(&runner, P2, 1);
    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(life(&runner, P1), 20, "P1 paid, so P1 loses no life");
    assert_eq!(hand_size(&runner, P1), 2, "P1 discarded exactly one card");
    assert_eq!(life(&runner, P2), 17, "P2 declined, so P2 loses 3 life");
    assert_eq!(hand_size(&runner, P2), 3, "P2 discarded nothing");
    assert_eq!(life(&runner, P0), 20, "the controller is never the subject");
}

/// CR 725.1: the ETB half. Cast from hand so the enters trigger actually fires
/// (a scenario-seeded permanent does not), and confirm the controller takes the
/// monarch designation.
#[test]
fn court_of_ambition_etb_makes_its_controller_the_monarch() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let court: ObjectId = scenario
        .add_spell_to_hand_from_oracle(P0, "Court of Ambition", false, COURT_OF_AMBITION)
        .as_enchantment()
        // Printed cost {2}{B}{B}.
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Black, ManaCostShard::Black],
        })
        .id();
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    assert!(
        runner.state().monarch.is_none(),
        "fixture reach-guard: nobody is the monarch before the cast"
    );

    runner.cast(court).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&court].zone,
        Zone::Battlefield,
        "the enchantment must have resolved onto the battlefield"
    );
    assert_eq!(
        runner.state().monarch,
        Some(P0),
        "the enters trigger makes its controller the monarch"
    );
}
