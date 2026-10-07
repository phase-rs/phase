//! Dandân in-game simultaneous draws: an instruction that makes several players
//! draw at once is settled seat by seat, then dealt one card at a time from the
//! shared pile, active player first (CR 121.2a, CR 121.2c as modified by the
//! format's `DealOrder`).
//!
//! A prompt between two empty-pile attempts needs a draw replacement; V5 is the
//! positive control that a prompt is reachable inside the dealer, and V7 covers a
//! board with none.

use std::collections::HashMap;

use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, Outcome, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::format::FormatConfig;
use engine::types::game_state::{DrawDealerStage, GameState, ReplacementChoiceKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::dandan_filter_owner_axis::{dandan, scenario, stage, start};
use crate::support::shared_card_db;

/// Recipient of every `CardDrawn` event, in event order.
fn draw_order(events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::CardDrawn { player_id, .. } => Some(*player_id),
            _ => None,
        })
        .collect()
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

fn table(entries: &[(PlayerId, i32)]) -> HashMap<PlayerId, i32> {
    entries.iter().copied().collect()
}

fn drew_this_turn(state: &GameState, seat: PlayerId) -> usize {
    state
        .player_actions_this_turn
        .iter()
        .filter(|entry| **entry == (seat, PlayerActionKind::Draw))
        .count()
}

/// Dandân game with Prosperity in `actor`'s hand over a pile of `pile_len`
/// Islands; returns the runner, the spell and the pile top first.
fn prosperity_game(
    db: &CardDatabase,
    actor: PlayerId,
    pile_len: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut sc = scenario(dandan());
    let pile = stage(&mut sc, db, Zone::Library, &vec![(P0, "Island"); pile_len]);
    let spell = sc.add_real_card(actor, "Prosperity", Zone::Hand, db);
    (start(sc, actor), spell, pile)
}

fn assert_each_player_drew_once(state: &GameState) {
    for seat in [P0, P1] {
        assert_eq!(
            drew_this_turn(state, seat),
            1,
            "{seat:?} drew one instruction"
        );
        assert!(
            state
                .player_actions_this_way
                .contains(&(seat, PlayerActionKind::Draw)),
            "{seat:?} is a drawer this way"
        );
    }
}

// ---------------------------------------------------------------------------
// V1 / V1b: the player_scope driver deals round-robin from the active player
// ---------------------------------------------------------------------------

#[test]
fn v1_prosperity_deals_one_card_at_a_time_active_player_first() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, pile) = prosperity_game(db, P0, 8);

    let outcome = runner.cast(spell).x(3).resolve();

    let state = outcome.state();
    assert_eq!(
        draw_order(outcome.events()),
        [P0, P1, P0, P1, P0, P1],
        "deal order"
    );
    for (index, card) in pile[..6].iter().enumerate() {
        let expected = if index % 2 == 0 { P0 } else { P1 };
        assert_eq!(
            holder(state, *card),
            Some(expected),
            "holder of card {index}"
        );
    }
    assert_eq!(hand_len(state, P0), 3, "reach: P0 drew X");
    assert_eq!(hand_len(state, P1), 3, "reach: P1 drew X");
    assert_eq!(
        state.last_effect_counts_by_player,
        table(&[(P0, 3), (P1, 3)]),
        "only the dealer publishes a per-seat Draw table"
    );
    assert!(
        state.active_draw_sequence().is_none(),
        "the dealer frame retired"
    );
    assert_each_player_drew_once(state);
}

#[test]
fn v1b_the_active_player_not_the_pile_holder_is_dealt_first() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, pile) = prosperity_game(db, P1, 8);

    let outcome = runner.cast(spell).x(3).resolve();

    let state = outcome.state();
    assert_eq!(
        draw_order(outcome.events()),
        [P1, P0, P1, P0, P1, P0],
        "deal order"
    );
    for index in [0, 2, 4] {
        assert_eq!(
            holder(state, pile[index]),
            Some(P1),
            "P1 holds card {index}"
        );
    }
    assert_eq!(
        state.last_effect_counts_by_player,
        table(&[(P0, 3), (P1, 3)])
    );
}

// ---------------------------------------------------------------------------
// V2: a format with separate libraries keeps the sequential fan-out
// ---------------------------------------------------------------------------

#[test]
fn v2_separate_libraries_keep_the_sequential_fan_out() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(FormatConfig::standard());
    for seat in [P0, P1] {
        stage(&mut sc, db, Zone::Library, &[(seat, "Island"); 4]);
    }
    let spell = sc.add_real_card(P0, "Prosperity", Zone::Hand, db);
    let mut runner = start(sc, P0);

    let outcome = runner.cast(spell).x(3).resolve();

    let state = outcome.state();
    assert_eq!(
        draw_order(outcome.events()),
        [P0, P0, P0, P1, P1, P1],
        "CR 121.2c order"
    );
    assert_eq!(hand_len(state, P0), 3, "reach: P0 drew X");
    assert_eq!(hand_len(state, P1), 3, "reach: P1 drew X");
    assert!(
        state.last_effect_counts_by_player.is_empty(),
        "the sequential fan-out publishes no Draw table"
    );
    assert_each_player_drew_once(state);
}

// ---------------------------------------------------------------------------
// V3: "any number of target players each draw" (Jace, Memory Adept)
// ---------------------------------------------------------------------------

fn jace_game(db: &CardDatabase, shared: bool, pile_len: usize) -> (GameRunner, ObjectId) {
    let mut sc = scenario(if shared {
        dandan()
    } else {
        FormatConfig::standard()
    });
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
    let jace = sc.add_real_card(P0, "Jace, Memory Adept", Zone::Battlefield, db);
    let mut runner = start(sc, P0);
    let object = runner.state_mut().objects.get_mut(&jace).expect("jace");
    object.loyalty = Some(7);
    object.counters.insert(CounterType::Loyalty, 7);
    (runner, jace)
}

fn minus_seven(runner: &GameRunner, jace: ObjectId) -> usize {
    runner.state().objects[&jace]
        .abilities
        .iter()
        .position(|ability| format!("{:?}", ability.cost).contains("-7"))
        .expect("the -7 loyalty ability")
}

#[test]
fn v3_target_players_each_draw_twenty_alternate_seats() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, jace) = jace_game(db, true, 44);
    let index = minus_seven(&runner, jace);

    let outcome = runner
        .activate(jace, index)
        .target_players(&[P0, P1])
        .resolve();

    let state = outcome.state();
    let order = draw_order(outcome.events());
    assert_eq!(order.len(), 40, "reach: both players drew twenty");
    assert!(
        order
            .iter()
            .enumerate()
            .all(|(slot, seat)| *seat == if slot % 2 == 0 { P0 } else { P1 }),
        "alternating deal, got {order:?}"
    );
    assert_eq!(
        state.last_effect_counts_by_player,
        table(&[(P0, 20), (P1, 20)])
    );
    assert_each_player_drew_once(state);
}

#[test]
fn v3_separate_libraries_keep_one_player_at_a_time() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, jace) = jace_game(db, false, 20);
    let index = minus_seven(&runner, jace);

    let outcome = runner
        .activate(jace, index)
        .target_players(&[P0, P1])
        .resolve();

    let order = draw_order(outcome.events());
    assert_eq!(order.len(), 40, "reach: both players drew twenty");
    assert!(order[..20].iter().all(|seat| *seat == P0));
    assert!(order[20..].iter().all(|seat| *seat == P1));
    assert!(outcome.state().last_effect_counts_by_player.is_empty());
}

// ---------------------------------------------------------------------------
// V4: CR 121.2a, every seat's instruction settles before any card is dealt
// ---------------------------------------------------------------------------

fn alms_game(db: &CardDatabase, shared: bool) -> (GameRunner, ObjectId) {
    let mut sc = scenario(if shared {
        dandan()
    } else {
        FormatConfig::standard()
    });
    if shared {
        stage(&mut sc, db, Zone::Library, &[(P0, "Island"); 8]);
    } else {
        for seat in [P0, P1] {
            stage(&mut sc, db, Zone::Library, &[(seat, "Island"); 4]);
        }
    }
    sc.add_real_card(P0, "Alms Collector", Zone::Battlefield, db);
    let spell = sc.add_real_card(P0, "Prosperity", Zone::Hand, db);
    (start(sc, P0), spell)
}

#[test]
fn v4_alms_collector_settles_the_opponents_instruction_before_the_deal() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell) = alms_game(db, true);

    let outcome = runner.cast(spell).x(2).resolve();

    let state = outcome.state();
    assert_eq!(
        draw_order(outcome.events()),
        [P0, P1, P0, P0],
        "the replaced instruction's draws precede the first dealt card"
    );
    assert_eq!(
        hand_len(state, P0),
        3,
        "P0's own instruction is not replaced"
    );
    assert_eq!(hand_len(state, P1), 1, "P1's instruction became one draw");
}

#[test]
fn v4_control_without_the_replacement_deals_alternately() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell, _) = prosperity_game(db, P0, 8);

    let outcome = runner.cast(spell).x(2).resolve();

    assert_eq!(draw_order(outcome.events()), [P0, P1, P0, P1]);
}

#[test]
fn v4_separate_libraries_reference_reading() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell) = alms_game(db, false);

    let outcome = runner.cast(spell).x(2).resolve();

    assert_eq!(
        hand_len(outcome.state(), P0),
        3,
        "P0: two plus Alms Collector's draw"
    );
    assert_eq!(
        hand_len(outcome.state(), P1),
        1,
        "P1: the replacement's draw"
    );
}

// ---------------------------------------------------------------------------
// V5: a draw replacement prompt inside the dealer, across seat rotation
// ---------------------------------------------------------------------------

#[test]
fn v5_a_replacement_prompt_parks_the_dealer_and_keeps_every_seats_count() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    let pile = stage(&mut sc, db, Zone::Library, &[(P0, "Island"); 8]);
    sc.add_real_card(P0, "Obstinate Familiar", Zone::Battlefield, db);
    let spell = sc.add_real_card(P0, "Prosperity", Zone::Hand, db);
    let mut runner = start(sc, P0);

    let mut events = runner.cast(spell).x(3).resolve().events().to_vec();

    // Optional replacement: index 0 accepts the skip, 1 declines it.
    let mut answers = [1, 0, 1].into_iter();
    let mut prompts = 0;
    while let WaitingFor::ReplacementChoice { player, .. } = runner.state().waiting_for.clone() {
        assert_eq!(player, P0, "only P0's draws can be replaced");
        if prompts == 0 {
            let frame = runner
                .state()
                .active_draw_sequence()
                .expect("a parked dealer frame");
            assert_eq!(frame.player, P0, "the held unit belongs to P0");
            assert!(frame.dealer.is_some(), "the parked frame is a dealer frame");
        }
        prompts += 1;
        let index = answers.next().expect("exactly three prompts");
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("choice accepted")
                .events,
        );
    }

    let state = runner.state();
    assert_eq!(prompts, 3, "one prompt per P0 unit");
    assert_eq!(holder(state, pile[0]), Some(P0));
    assert_eq!(holder(state, pile[3]), Some(P0));
    for index in [1, 2, 4] {
        assert_eq!(
            holder(state, pile[index]),
            Some(P1),
            "P1 holds card {index}"
        );
    }
    assert_eq!(
        state.last_effect_counts_by_player,
        table(&[(P0, 2), (P1, 3)]),
        "P0's delivered count survived two seat rotations and three pauses"
    );
    assert!(state.active_draw_sequence().is_none(), "the frame retired");
    assert_eq!(draw_order(&events).len(), 5);
}

// ---------------------------------------------------------------------------
// V6 / V7: deck-out under the dealer
// ---------------------------------------------------------------------------

fn deck_out(pile_len: usize) -> (GameRunner, Outcome) {
    let Some(db) = shared_card_db() else {
        unreachable!("callers return early without the card export")
    };
    let (mut runner, spell, _) = prosperity_game(db, P0, pile_len);
    let outcome = runner.cast(spell).x(3).resolve();
    (runner, outcome)
}

#[test]
fn v6a_an_empty_pile_draws_the_game() {
    if shared_card_db().is_none() {
        return;
    }
    let (_, outcome) = deck_out(0);
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::GameOver { winner: None }
    ));
}

#[test]
fn v6b_a_pile_short_by_one_loses_only_the_second_seat() {
    if shared_card_db().is_none() {
        return;
    }
    let (_, outcome) = deck_out(5);
    assert_eq!(hand_len(outcome.state(), P0), 3, "reach: P0 drew X");
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::GameOver { winner: Some(winner) } if *winner == P0
    ));
}

#[test]
fn v6c_a_pile_short_by_two_makes_both_seats_attempt_and_draws_the_game() {
    if shared_card_db().is_none() {
        return;
    }
    let (_, outcome) = deck_out(4);
    assert_eq!(
        draw_order(outcome.events()),
        [P0, P1, P0, P1],
        "reach: dealt alternately"
    );
    assert!(
        matches!(
            outcome.final_waiting_for(),
            WaitingFor::GameOver { winner: None }
        ),
        "both seats attempted an empty draw before either loss: {:?}",
        outcome.final_waiting_for()
    );
}

#[test]
fn v7_no_prompt_opens_between_the_two_empty_draws() {
    if shared_card_db().is_none() {
        return;
    }
    let (runner, outcome) = deck_out(4);
    assert!(
        !matches!(
            outcome.final_waiting_for(),
            WaitingFor::ReplacementChoice { .. }
        ),
        "no draw replacement is on the board"
    );
    assert!(runner.state().active_draw_sequence().is_none());
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::GameOver { winner: None }
    ));
}

// ---------------------------------------------------------------------------
// V11: the unscoped tail runs once, after the last card is dealt
// ---------------------------------------------------------------------------

/// Answer every open replacement prompt, declining optional skips.
fn answer_every_prompt(runner: &mut GameRunner) -> Vec<GameEvent> {
    let mut events = Vec::new();
    while let WaitingFor::ReplacementChoice { kind, .. } = runner.state().waiting_for.clone() {
        let index = usize::from(kind == ReplacementChoiceKind::OptionalBranch);
        events.extend(
            runner
                .act(GameAction::ChooseReplacement { index })
                .expect("choice accepted")
                .events,
        );
    }
    events
}

fn tail_game(db: &CardDatabase, with_familiar: bool) -> (GameRunner, ObjectId) {
    let mut sc = scenario(dandan());
    stage(&mut sc, db, Zone::Library, &vec![(P0, "Island"); 10]);
    sc.add_real_card(P0, "Alms Collector", Zone::Battlefield, db);
    if with_familiar {
        sc.add_real_card(P0, "Obstinate Familiar", Zone::Battlefield, db);
    }
    let spell = sc
        .add_spell_to_hand_from_oracle(
            P0,
            "Hostile Tail",
            false,
            "Each player draws two cards. You gain 7 life.",
        )
        .id();
    (start(sc, P0), spell)
}

fn life_gains(events: &[GameEvent]) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, GameEvent::LifeChanged { player_id, amount, .. } if *player_id == P0 && *amount == 7))
        .map(|(index, _)| index)
        .collect()
}

#[test]
fn v11_a_prompt_inside_a_nested_instruction_keeps_the_tail_after_the_deal() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell) = tail_game(db, true);

    let mut events = runner.cast(spell).resolve().events().to_vec();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "reach: a replacement prompt parked the dealer"
    );
    events.extend(answer_every_prompt(&mut runner));

    let gains = life_gains(&events);
    assert_eq!(gains.len(), 1, "the tail ran exactly once: {gains:?}");
    let last_draw = events
        .iter()
        .rposition(|event| matches!(event, GameEvent::CardDrawn { .. }))
        .expect("cards were drawn");
    assert!(
        last_draw < gains[0],
        "every card is dealt before the tail runs"
    );
    assert_eq!(draw_order(&events).len(), 4, "P0: 2 + Alms draw, P1: 1");
}

#[test]
fn v11_without_a_prompt_the_tail_runs_after_both_hands_fill() {
    let Some(db) = shared_card_db() else { return };
    let (mut runner, spell) = tail_game(db, false);

    let outcome = runner.cast(spell).resolve();

    let gains = life_gains(outcome.events());
    assert_eq!(gains.len(), 1, "the tail ran exactly once");
    let last_draw = outcome
        .events()
        .iter()
        .rposition(|event| matches!(event, GameEvent::CardDrawn { .. }))
        .expect("cards were drawn");
    assert!(last_draw < gains[0]);
    assert_eq!(
        draw_order(outcome.events()).len(),
        4,
        "reach: the dealer ran"
    );
}

// ---------------------------------------------------------------------------
// V12: a prompt while the dealer is still settling (CR 121.2a)
// ---------------------------------------------------------------------------

#[test]
fn v12_a_prompt_while_settling_parks_the_dealer_before_any_card_is_dealt() {
    let Some(db) = shared_card_db() else { return };
    let mut sc = scenario(dandan());
    stage(&mut sc, db, Zone::Library, &[(P0, "Island"); 8]);
    sc.add_real_card(P0, "Alms Collector", Zone::Battlefield, db);
    sc.add_real_card(P1, "Quantum Riddler", Zone::Battlefield, db);
    let spell = sc.add_real_card(P0, "Prosperity", Zone::Hand, db);
    let mut runner = start(sc, P0);

    let mut events = runner.cast(spell).x(2).resolve().events().to_vec();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { player, .. } if player == P1
        ),
        "reach: P1's instruction parked a replacement choice"
    );
    let frame = runner.state().active_draw_sequence().expect("parked frame");
    assert!(
        matches!(
            frame.dealer.as_ref().map(|dealer| &dealer.stage),
            Some(DrawDealerStage::Settling { next: 1 })
        ),
        "the dealer is parked settling P1's instruction: {:?}",
        frame.dealer
    );
    assert!(
        draw_order(&events).is_empty(),
        "no card is dealt while settling"
    );

    events.extend(
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("choice accepted")
            .events,
    );
    events.extend(answer_every_prompt(&mut runner));

    let state = runner.state();
    assert!(state.active_draw_sequence().is_none(), "the frame retired");
    assert_eq!(
        hand_len(state, P0),
        3,
        "P0's own instruction is not replaced"
    );
    assert_eq!(hand_len(state, P1), 1, "P1's instruction became one draw");
    assert_eq!(draw_order(&events).len(), 4);
}
