//! Dandân wheels: a card that makes every player move zones and then draw is one
//! all-players move phase, one shuffle of the shared pile, and a draw dealt one
//! card at a time from the active player (CR 701.24a, CR 608.2c, CR 121.2c as
//! modified by the format's `DealOrder`). A format with separate libraries keeps
//! one wheel per player.

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, Outcome, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::EffectKind;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::format::{FormatConfig, ZoneScope};
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::dandan_filter_owner_axis::{dandan, scenario, stage, start};
use crate::support::shared_card_db;

fn draws(events: &[GameEvent]) -> Vec<(PlayerId, ObjectId)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardDrawn {
                player_id,
                object_id,
                ..
            } => Some((*player_id, *object_id)),
            _ => None,
        })
        .collect()
}

fn draw_order(events: &[GameEvent]) -> Vec<PlayerId> {
    draws(events).iter().map(|(seat, _)| *seat).collect()
}

fn shufflers(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::PlayerPerformedAction {
                player_id,
                action: PlayerActionKind::ShuffledLibrary,
                ..
            } => Some(*player_id),
            _ => None,
        })
        .collect()
}

fn index_of(events: &[GameEvent], pred: impl Fn(&GameEvent) -> bool) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| pred(event))
        .map(|(index, _)| index)
        .collect()
}

fn moves_to_library(events: &[GameEvent], from: Zone) -> Vec<usize> {
    index_of(events, |event| {
        matches!(
            event,
            GameEvent::ZoneChanged { from: Some(origin), to: Zone::Library, .. } if *origin == from
        )
    })
}

fn shuffle_positions(events: &[GameEvent]) -> Vec<usize> {
    index_of(events, |event| {
        matches!(
            event,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )
    })
}

fn draw_positions(events: &[GameEvent]) -> Vec<usize> {
    index_of(events, |event| matches!(event, GameEvent::CardDrawn { .. }))
}

fn holder(state: &GameState, id: ObjectId) -> Option<PlayerId> {
    state
        .players
        .iter()
        .find(|player| player.hand.contains(&id))
        .map(|player| player.id)
}

fn hand_len(state: &GameState, seat: PlayerId) -> usize {
    state.players[seat.0 as usize].hand.len()
}

fn alternating(first: PlayerId, second: PlayerId, per_seat: usize) -> Vec<PlayerId> {
    (0..per_seat * 2)
        .map(|k| if k % 2 == 0 { first } else { second })
        .collect()
}

/// A wheel spell in `actor`'s hand with `hands` extra hand cards per seat,
/// `graveyard` cards per seat in the graveyard, and `pile_len` pile cards.
fn wheel_game(
    db: &CardDatabase,
    format: FormatConfig,
    spell_name: &str,
    actor: PlayerId,
    hands: [usize; 2],
    graveyard: [usize; 2],
    pile_len: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let shared = format.format.shared_zones().library == ZoneScope::Shared;
    let mut sc = scenario(format);
    let mut moved_hand = Vec::new();
    let mut moved_graveyard = Vec::new();
    for seat in [P0, P1] {
        moved_hand.extend(stage(
            &mut sc,
            db,
            Zone::Hand,
            &vec![(seat, "Forest"); hands[seat.0 as usize]],
        ));
        moved_graveyard.extend(stage(
            &mut sc,
            db,
            Zone::Graveyard,
            &vec![(seat, "Mountain"); graveyard[seat.0 as usize]],
        ));
    }
    if shared {
        stage(&mut sc, db, Zone::Library, &vec![(P0, "Island"); pile_len]);
    } else {
        for seat in [P0, P1] {
            stage(
                &mut sc,
                db,
                Zone::Library,
                &vec![(seat, "Island"); pile_len],
            );
        }
    }
    let spell = sc.add_real_card(actor, spell_name, Zone::Hand, db);
    (start(sc, actor), spell, moved_hand, moved_graveyard)
}

// ---------------------------------------------------------------------------
// W1 / W1b: Day's Undoing over the shared pile
// ---------------------------------------------------------------------------

fn assert_one_wheel_then_dealt_draw(
    outcome: &Outcome,
    first: PlayerId,
    second: PlayerId,
    moved: usize,
) {
    let events = outcome.events();
    let hand_moves = moves_to_library(events, Zone::Hand);
    let graveyard_moves = moves_to_library(events, Zone::Graveyard);
    assert_eq!(
        hand_moves.len() + graveyard_moves.len(),
        moved,
        "reach: every hand and graveyard card was found by a move"
    );
    let shuffles = shuffle_positions(events);
    let draw_events = draw_positions(events);
    assert_eq!(shuffles.len(), 1, "the pile is shuffled once");
    let last_move = hand_moves
        .iter()
        .chain(&graveyard_moves)
        .max()
        .copied()
        .expect("moves");
    assert!(
        last_move < shuffles[0],
        "every move precedes the shuffle: {events:#?}"
    );
    assert!(
        shuffles[0] < draw_events[0],
        "the shuffle precedes the first draw"
    );
    let order = draw_order(events);
    assert_eq!(
        order,
        alternating(first, second, 7),
        "dealt one card at a time"
    );
}

#[test]
fn w1_days_undoing_moves_every_seat_then_shuffles_once_then_deals_fourteen() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, hand, graveyard) =
        wheel_game(db, dandan(), "Day's Undoing", P0, [3, 2], [2, 2], 20);

    let outcome = runner.cast(spell).resolve();

    assert_one_wheel_then_dealt_draw(&outcome, P0, P1, hand.len() + graveyard.len());
    let state = outcome.state();
    for (k, (seat, object)) in draws(outcome.events()).iter().enumerate() {
        assert_eq!(holder(state, *object), Some(*seat), "draw {k} is held");
    }
    for card in &graveyard {
        assert!(
            !state.graveyard_of(P0).contains(card),
            "the shared graveyard was emptied"
        );
    }
    assert_eq!(hand_len(state, P0), 7);
    assert_eq!(hand_len(state, P1), 7);
}

#[test]
fn w1b_a_non_canonical_active_seat_is_dealt_first_and_shuffles_once() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, hand, graveyard) =
        wheel_game(db, dandan(), "Day's Undoing", P1, [3, 2], [2, 2], 20);

    let outcome = runner.cast(spell).resolve();

    assert_one_wheel_then_dealt_draw(&outcome, P1, P0, hand.len() + graveyard.len());
    assert_eq!(shufflers(outcome.events()), [P1], "the controller shuffles");
}

// ---------------------------------------------------------------------------
// W1(e): the end-the-turn tail runs after the dealt draw
// ---------------------------------------------------------------------------

#[test]
fn w1e_end_the_turn_follows_the_last_dealt_card() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, ..) = wheel_game(db, dandan(), "Day's Undoing", P0, [3, 2], [2, 2], 20);

    let outcome = runner.cast(spell).resolve();

    let events = outcome.events();
    let ends = index_of(events, |event| {
        matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::EndTheTurn,
                ..
            }
        )
    });
    assert_eq!(ends.len(), 1, "reach: the tail resolved");
    assert!(
        ends[0] > *draw_positions(events).last().expect("draws"),
        "CR 608.2c: the turn ends after the draw"
    );
}

// ---------------------------------------------------------------------------
// W2: separate libraries keep one wheel per player
// ---------------------------------------------------------------------------

#[test]
fn w2_separate_libraries_shuffle_and_draw_per_player() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, ..) = wheel_game(
        db,
        FormatConfig::standard(),
        "Day's Undoing",
        P0,
        [3, 2],
        [2, 2],
        10,
    );

    let outcome = runner.cast(spell).resolve();

    let events = outcome.events();
    assert_eq!(shufflers(events), [P0, P1], "one shuffle per library");
    let order = draw_order(events);
    let mut expected = vec![P0; 7];
    expected.extend([P1; 7]);
    assert_eq!(order, expected, "CR 121.2c with separate libraries");
    assert_eq!(hand_len(outcome.state(), P0), 7);
    assert_eq!(hand_len(outcome.state(), P1), 7);
}

// ---------------------------------------------------------------------------
// W3: Wheel of Fortune (native split) and W4: Timetwister (the transformed chain)
// ---------------------------------------------------------------------------

#[test]
fn w3_wheel_of_fortune_discards_both_hands_then_deals() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, hand, _) =
        wheel_game(db, dandan(), "Wheel of Fortune", P0, [3, 2], [0, 0], 20);

    let outcome = runner.cast(spell).resolve();

    let events = outcome.events();
    let discards = index_of(events, |event| matches!(event, GameEvent::Discarded { .. }));
    assert_eq!(discards.len(), hand.len(), "reach: both hands discarded");
    assert!(
        *discards.last().unwrap() < draw_positions(events)[0],
        "every discard precedes the first draw"
    );
    let order = draw_order(events);
    assert_eq!(order, alternating(P0, P1, 7));
    assert_eq!(hand_len(outcome.state(), P0), 7);
    assert_eq!(hand_len(outcome.state(), P1), 7);
}

#[test]
fn w3_standard_deals_each_player_in_turn() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, hand, _) = wheel_game(
        db,
        FormatConfig::standard(),
        "Wheel of Fortune",
        P0,
        [3, 2],
        [0, 0],
        10,
    );

    let outcome = runner.cast(spell).resolve();

    let order: Vec<PlayerId> = draws(outcome.events())
        .iter()
        .map(|(seat, _)| *seat)
        .collect();
    let mut expected = vec![P0; 7];
    expected.extend([P1; 7]);
    assert_eq!(order, expected);
    assert_eq!(hand_len(outcome.state(), P0), 7, "reach: {}", hand.len());
}

#[test]
fn w4_timetwister_is_the_same_class_as_days_undoing() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, hand, graveyard) =
        wheel_game(db, dandan(), "Timetwister", P0, [3, 2], [2, 2], 20);

    let outcome = runner.cast(spell).resolve();

    assert_one_wheel_then_dealt_draw(&outcome, P0, P1, hand.len() + graveyard.len());
}

// ---------------------------------------------------------------------------
// W5 / W6: empty zones and deck-out
// ---------------------------------------------------------------------------

#[test]
fn w5_empty_hands_and_graveyard_still_shuffle_once_and_deal() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, ..) = wheel_game(db, dandan(), "Day's Undoing", P0, [0, 0], [0, 0], 14);

    let outcome = runner.cast(spell).resolve();

    let events = outcome.events();
    assert_eq!(shuffle_positions(events).len(), 1, "CR 701.24d");
    let order = draw_order(events);
    assert_eq!(order, alternating(P0, P1, 7));
}

#[test]
fn w6_a_pile_of_twelve_draws_the_game_and_thirteen_loses_only_the_second_seat() {
    let Some(db) = shared_card_db() else { return };
    // 4 hand + 2 graveyard + 6 pile cards = 12 dealt cards.
    let (mut runner, spell, ..) = wheel_game(db, dandan(), "Day's Undoing", P0, [2, 2], [1, 1], 6);
    let outcome = runner.cast(spell).resolve();
    let order = draw_order(outcome.events());
    assert_eq!(order, alternating(P0, P1, 6), "reach: dealt alternately");
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: None }
        ),
        "CR 104.4a: both seats attempt an empty pile: {:?}",
        outcome.final_waiting_for()
    );

    let (mut runner, spell, ..) = wheel_game(db, dandan(), "Day's Undoing", P0, [2, 2], [1, 1], 7);
    let outcome = runner.cast(spell).resolve();
    let order = draw_order(outcome.events());
    assert_eq!(
        order,
        alternating(P0, P1, 7)[..13],
        "reach: dealt alternately until the pile ran out"
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: Some(winner) } if *winner == P0
        ),
        "CR 121.4: only the seat that drew from an empty pile loses: {:?}",
        outcome.final_waiting_for()
    );
}
