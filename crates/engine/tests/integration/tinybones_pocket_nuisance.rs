//! Regression for Tinybones, Pocket Nuisance (FRA #237):
//! "Whenever a player discards one or more cards, Tinybones deals 1 damage
//! to each opponent."
//!
//! Before this fix, `try_parse_discard_trigger` had no combinator arm for the
//! "a player discards" actor combined with the batched "one or more cards"
//! quantity wording, so the whole trigger condition fell through to
//! `TriggerMode::Unknown` and the damage ability never fired at all — this is
//! the plural/batched sibling of the already-supported singular "a player
//! discards a card" trigger (see `trigger_opponent_discards_a_card` /
//! `parse_discard_subject` in `oracle_trigger.rs`).
//!
//! CR 603.2c: "An ability triggers only once each time its trigger event
//! occurs. However, it can trigger repeatedly if one event contains multiple
//! occurrences." A single cleanup-discard action that discards two cards at
//! once is one qualifying event, so Tinybones must deal exactly 1 damage —
//! not 2 — for that event. A fix that plumbed the new actor phrasing through
//! as plain `TriggerMode::Discarded` (instead of `TriggerMode::DiscardedAll`
//! with `batched: true`) would still make the card "supported", but would
//! fire once per discarded card instead of once per event; the two-card test
//! below is what catches that mistake.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TINYBONES: &str = "When Tinybones enters, each opponent discards a card.\nWhenever a player discards one or more cards, Tinybones deals 1 damage to each opponent.";

/// Seeds Tinybones directly onto the battlefield (bypassing its own ETB, per
/// `add_creature_from_oracle`'s `create_object` placement — no zone-change
/// event is emitted) and parks the active player mid-cleanup with `hand_size`
/// cards, exactly at the point a `DiscardToHandSize` choice is outstanding.
/// Mirrors `cleanup_discard_trigger_pipeline.rs`'s `setup_cleanup_discard`,
/// the existing analogous fixture for another `TriggerMode::DiscardedAll`
/// discard-punisher (Magmakin Artillerist).
fn setup(hand_size: usize) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Cleanup);

    scenario.add_creature_from_oracle(P0, "Tinybones, Pocket Nuisance", 2, 1, TINYBONES);

    let cards = (0..hand_size)
        .map(|index| scenario.add_card_to_hand(P0, &format!("Hand Card {index}")))
        .collect::<Vec<_>>();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::Cleanup;
        state.waiting_for = WaitingFor::DiscardToHandSize {
            player: P0,
            count: hand_size - 7,
            cards: cards.clone(),
        };
    }
    (runner, cards)
}

fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to resolve Tinybones's damage trigger");
    }
}

#[test]
fn tinybones_a_player_discards_one_card_deals_one_damage_once() {
    let (mut runner, cards) = setup(8);

    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("submit single-card cleanup discard");

    assert_eq!(runner.state().objects[&cards[0]].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the singular discard event must place exactly one Tinybones trigger"
    );

    resolve_stack(&mut runner);
    assert_eq!(
        runner.state().players[1].life,
        19,
        "a single discarded card must deal exactly 1 damage via Tinybones"
    );
}

#[test]
fn tinybones_batched_two_card_discard_deals_damage_exactly_once() {
    let (mut runner, cards) = setup(9);

    runner
        .act(GameAction::SelectCards {
            cards: cards[..2].to_vec(),
        })
        .expect("submit two-card cleanup discard");

    assert_eq!(
        runner.state().objects[&cards[0]].zone,
        Zone::Graveyard,
        "reach guard: both cards must have actually been discarded"
    );
    assert_eq!(
        runner.state().objects[&cards[1]].zone,
        Zone::Graveyard,
        "reach guard: both cards must have actually been discarded"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "CR 603.2c: one discard event that discards two cards must place \
         exactly one Tinybones trigger, not two"
    );

    resolve_stack(&mut runner);
    assert_eq!(
        runner.state().players[1].life,
        19,
        "CR 603.2c: a batched two-card discard event must deal 1 damage \
         total via Tinybones, not 1 damage per discarded card"
    );
}
