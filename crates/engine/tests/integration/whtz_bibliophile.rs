//! CR 103.2a + CR 903.5a: Whtz, the Bibliophile — "Rulebreaker — A deck with
//! this commander has no maximum deck size. {3}, {T}: You draw a card and gain
//! 1 life. This ability costs {3} less to activate if you had 200 or more
//! cards in your starting deck."
//!
//! These rows drive the parsed verbatim Oracle through the production
//! activation pipeline with the `starting_deck_size` latch set the way
//! `load_deck_into_state` writes it (see the `starting_deck_size_latches_*`
//! unit rows in `deck_loading.rs`). V10/V11 pin the discount boundary;
//! V14 pins controller scoping and the latch's immunity to mill.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const WHTZ_ORACLE: &str = "Rulebreaker — A deck with this commander has no maximum deck size.\n{3}, {T}: You draw a card and gain 1 life. This ability costs {3} less to activate if you had 200 or more cards in your starting deck.";

fn pool(scenario: &mut GameScenario, player: PlayerId, n: usize) {
    scenario.with_mana_pool(
        player,
        (0..n)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect(),
    );
}

/// Stage Whtz on P0's battlefield with a `library_cards`-card library, unsick,
/// and the given starting-deck latches. Returns the runner and Whtz's object
/// id.
fn stage(
    p0_latch: u32,
    p1_latch: u32,
    pool_n: usize,
    library_cards: usize,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let whtz = scenario
        .add_creature_from_oracle(P0, "Whtz, the Bibliophile", 1, 3, WHTZ_ORACLE)
        .id();
    for i in 0..library_cards {
        scenario.add_card_to_library_top(P0, &format!("Draw Me {i}"));
    }
    pool(&mut scenario, P0, pool_n);
    let mut runner = scenario.build();
    // Summoning sickness would block the {T} cost; the latch is what this
    // file varies, not the untap step.
    runner
        .state_mut()
        .objects
        .get_mut(&whtz)
        .expect("whtz on the battlefield")
        .has_summoning_sickness = false;
    runner.state_mut().players[P0.0 as usize].starting_deck_size = p0_latch;
    runner.state_mut().players[P1.0 as usize].starting_deck_size = p1_latch;
    (runner, whtz)
}

/// V10: a 200-card starting deck discounts the {3} to {0} — the activation
/// succeeds with an empty pool, drawing a card and gaining 1 life.
#[test]
fn whtz_activates_for_free_with_200_card_starting_deck() {
    let (mut runner, whtz) = stage(200, 60, 0, 1);

    let outcome = runner.activate(whtz, 0).pay_with(&[whtz]).resolve();
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, 1);
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "a fully paid activation must reach priority with an empty stack"
    );
    assert!(
        runner.state().objects.get(&whtz).expect("whtz").tapped,
        "the {{T}} cost must be paid even when the mana discount is full"
    );
}

/// V11: a 199-card starting deck keeps the full {3} — {2} in the pool cannot
/// pay it (the V10 row guards the boundary: 200 succeeds, 199 fails).
#[test]
fn whtz_needs_full_price_with_199_card_starting_deck() {
    let (mut runner, whtz) = stage(199, 60, 2, 1);
    let life_before = runner.state().players[P0.0 as usize].life;
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    // Announce-time rejection surfaces as graceful `Err` via `act` (the
    // sibling convention in `activation_cost_reduction_election.rs`:
    // `an_illegal_activation_never_reaches_the_election`). The fluent
    // `activate(..).try_resolve()` unwraps the announce step
    // (`scenario.rs`: "ActivateAbility must be accepted by the engine"), so an
    // unaffordable activation panics there instead of returning `Err`.
    let result = runner.act(GameAction::ActivateAbility {
        source_id: whtz,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        result.is_err(),
        "2 mana must not pay a {{3}} activation the 199-card deck does not discount"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].life,
        life_before,
        "a failed activation must not gain life"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "a failed activation must not draw"
    );
}

/// V14: the discount is controller-scoped and latched — P1's 60-card deck is
/// irrelevant to P0's activation, and milling P0 to 50 cards does not move
/// P0's 200 latch, so the discount persists.
#[test]
fn whtz_discount_is_controller_scoped_and_survives_mill() {
    // P0's 200 latch discounts P0's activation even though P1's latch is 60
    // (controller scope) and P0's live library holds only 51 cards (the
    // latch, not the live library, gates the discount).
    let (mut runner, whtz) = stage(200, 60, 0, 51);
    assert_eq!(
        runner.state().players[P0.0 as usize].library.len(),
        51,
        "the live library is far below the latch"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].starting_deck_size,
        200,
        "the latch holds the submitted 200"
    );

    let outcome = runner.activate(whtz, 0).pay_with(&[whtz]).resolve();
    outcome.assert_hand_drawn(P0, 1);
    outcome.assert_life_delta(P0, 1);
}
