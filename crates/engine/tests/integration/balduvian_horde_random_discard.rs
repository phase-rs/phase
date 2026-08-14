//! Balduvian Horde — random discard as an unless-COST.
//!
//! Oracle text (verbatim, Scryfall):
//!   "When this creature enters, sacrifice it unless you discard a card at
//!    random."
//!
//! CR 701.9b draws a hard line between a random discard and a player-selected
//! one, and the engine only ever implemented the EFFECT side of it. As a COST
//! the mode was dropped: the unless-payment path destructured `selection: _`
//! and raised `WardDiscardChoice`, so the payer got to pick which card to
//! pitch. That is not cosmetic — on this card it converts the printed cost into
//! a strictly cheaper one, letting you keep your best card and ditch a land.
//!
//! The fix routes the cost through `effects::discard::discard_at_random`, the
//! same authority (and the same seeded `state.rng`) the effect layer uses.
//!
//! These tests drive the REAL pipeline: the creature is built from its verbatim
//! Oracle text and cast, so the ETB trigger, the parse of the "at random" tail,
//! and the cost payment all have to work together.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const BALDUVIAN_HORDE: &str =
    "When this creature enters, sacrifice it unless you discard a card at random.";

/// P0 casts Balduvian Horde with `hand_size` other cards in hand. Returns the
/// runner, the Horde's id, and the ids of the staged hand cards.
fn cast_horde(hand_size: usize, seed: u64) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );

    let horde = scenario
        .add_creature_to_hand_from_oracle(P0, "Balduvian Horde", 5, 5, BALDUVIAN_HORDE)
        // Printed cost {2}{R}{R}.
        .with_mana_cost(ManaCost::Cost {
            generic: 2,
            shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        })
        .id();

    let hand: Vec<ObjectId> = (0..hand_size)
        .map(|i| scenario.add_card_to_hand(P0, &format!("Filler Card {i}")))
        .collect();

    let mut runner = scenario.build();
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.cast(horde).resolve();
    (runner, horde, hand)
}

fn discarded_count(runner: &GameRunner, hand: &[ObjectId]) -> usize {
    hand.iter()
        .filter(|id| runner.state().objects[id].zone == Zone::Graveyard)
        .count()
}

/// Drive the ETB trigger to its unless-payment prompt.
fn advance_to_unless_prompt(runner: &mut GameRunner) {
    for _ in 0..20 {
        if matches!(runner.state().waiting_for, WaitingFor::UnlessPayment { .. }) {
            return;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    panic!(
        "the ETB trigger never surfaced an unless-payment prompt: {:?}",
        runner.state().waiting_for
    );
}

/// CR 701.9b: paying the cost discards a card WITHOUT asking which one. This is
/// the discriminating assertion — before the fix the engine parked on
/// `WardDiscardChoice` here and let the payer select.
#[test]
fn balduvian_horde_random_discard_is_paid_without_a_prompt() {
    let (mut runner, horde, hand) = cast_horde(3, 42);
    advance_to_unless_prompt(&mut runner);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("paying the random discard must be accepted");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::WardDiscardChoice { .. }
        ),
        "a random discard must never ask the payer to choose, got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        discarded_count(&runner, &hand),
        1,
        "exactly one card must have been discarded by the game"
    );
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&horde].zone,
        Zone::Battlefield,
        "paying the cost keeps the Horde on the battlefield"
    );
}

/// CR 118.12a: declining makes the unless-effect happen — the Horde sacrifices
/// itself and the hand is untouched.
#[test]
fn balduvian_horde_declining_sacrifices_and_keeps_the_hand() {
    let (mut runner, horde, hand) = cast_horde(3, 42);
    advance_to_unless_prompt(&mut runner);

    runner
        .act(GameAction::PayUnlessCost { pay: false })
        .expect("declining must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&horde].zone,
        Zone::Graveyard,
        "declining the discard sacrifices the Horde"
    );
    assert_eq!(
        discarded_count(&runner, &hand),
        0,
        "a declined cost discards nothing"
    );
}

/// CR 118.3: an empty hand cannot pay a one-card discard, so the cost is
/// unpayable and the Horde is sacrificed even on `pay: true`.
#[test]
fn balduvian_horde_empty_hand_cannot_pay() {
    let (mut runner, horde, _hand) = cast_horde(0, 42);
    advance_to_unless_prompt(&mut runner);

    runner
        .act(GameAction::PayUnlessCost { pay: true })
        .expect("attempting an unpayable cost must be accepted");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&horde].zone,
        Zone::Graveyard,
        "an unpayable random discard still sacrifices the Horde"
    );
}
