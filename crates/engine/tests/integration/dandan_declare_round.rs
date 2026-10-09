//! CR 103.5 on the Dandân shared pile: mulligans are declared, held until
//! every player has declared, then carried out together; the pregame deal
//! alternates seats starting with the starting player.

use engine::ai_support::candidate_actions;
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{load_and_hydrate_decks, DeckPayload};
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::scenario::{P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::events::{GameEvent, PlayerActionKind};
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, MulliganDecisionPhase, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn dandan(db: &CardDatabase, starting: PlayerId) -> (GameState, Vec<GameEvent>) {
    let mut state = GameState::new(FormatConfig::dandan(), 2, 7);
    load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(db));
    let result = start_game_with_starting_player(&mut state, starting);
    (state, result.events)
}

fn standard(starting: PlayerId) -> (GameState, Vec<GameEvent>) {
    let mut state = GameState::new_two_player(7);
    for seat in [P0, P1] {
        for i in 0..20 {
            create_object(
                &mut state,
                CardId(seat.0 as u64 * 100 + i),
                seat,
                format!("Card {i}"),
                Zone::Library,
            );
        }
    }
    let result = start_game_with_starting_player(&mut state, starting);
    (state, result.events)
}

fn hand(state: &GameState, seat: PlayerId) -> Vec<ObjectId> {
    state.players[seat.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

fn pile(state: &GameState) -> Vec<ObjectId> {
    state.library_of(P0).iter().copied().collect()
}

fn holder(state: &GameState, id: ObjectId) -> PlayerId {
    state
        .players
        .iter()
        .find(|p| p.hand.contains(&id))
        .expect("dealt card is in a hand")
        .id
}

/// The seat holding each card moved Library -> Hand, in event order.
fn deal_sequence(state: &GameState, events: &[GameEvent]) -> Vec<PlayerId> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Library),
                to: Zone::Hand,
                ..
            } => Some(holder(state, *object_id)),
            _ => None,
        })
        .collect()
}

fn act(state: &mut GameState, seat: PlayerId, choice: MulliganChoice) -> Vec<GameEvent> {
    let label = format!("{seat:?} {choice:?}");
    apply(state, seat, GameAction::MulliganDecision { choice })
        .unwrap_or_else(|e| panic!("{label}: {e:?}"))
        .events
}

fn alternating(first: PlayerId, second: PlayerId) -> Vec<PlayerId> {
    (0..7).flat_map(|_| [first, second]).collect()
}

fn other(seat: PlayerId) -> PlayerId {
    if seat == P0 {
        P1
    } else {
        P0
    }
}

fn declare_state(state: &GameState) -> (Vec<PlayerId>, Vec<PlayerId>) {
    let WaitingFor::MulliganDecision {
        pending, declared, ..
    } = &state.waiting_for
    else {
        panic!("expected MulliganDecision, got {:?}", state.waiting_for);
    };
    (
        pending.iter().map(|e| e.player).collect(),
        declared.iter().map(|d| d.player).collect(),
    )
}

#[test]
fn v3_opening_deal_alternates_starting_with_the_starting_player() {
    let Some(db) = shared_card_db() else { return };
    for starting in [P0, P1] {
        let (state, events) = dandan(db, starting);
        assert_eq!(
            deal_sequence(&state, &events),
            alternating(starting, other(starting)),
            "{starting:?} starts"
        );
        assert_eq!(hand(&state, P0).len(), 7);
        assert_eq!(hand(&state, P1).len(), 7);
        assert_eq!(pile(&state).len(), 80 - 14);
        for seat in [P0, P1] {
            for id in hand(&state, seat) {
                assert_eq!(state.objects[&id].owner, seat);
            }
        }

        let (state, events) = standard(starting);
        let expected: Vec<PlayerId> = [vec![starting; 7], vec![other(starting); 7]].concat();
        assert_eq!(
            deal_sequence(&state, &events),
            expected,
            "a per-player library deals seat by seat"
        );
    }
}

#[test]
fn v4a_a_declared_mulligan_is_recorded_not_carried_out() {
    let Some(db) = shared_card_db() else { return };
    let (mut state, _) = dandan(db, P0);
    let (hand_before, pile_before) = (hand(&state, P1), pile(&state));

    let events = act(&mut state, P1, MulliganChoice::Mulligan);

    assert!(!events
        .iter()
        .any(|e| matches!(e, GameEvent::ZoneChanged { .. })));
    assert_eq!(hand(&state, P1), hand_before);
    assert_eq!(pile(&state), pile_before);
    assert_eq!(declare_state(&state), (vec![P0], vec![P1]));
    assert_eq!(state.waiting_for.acting_players(), vec![P0]);
    assert!(apply(
        &mut state,
        P1,
        GameAction::MulliganDecision {
            choice: MulliganChoice::Keep
        }
    )
    .is_err());

    let actors: Vec<Option<PlayerId>> = candidate_actions(&state)
        .iter()
        .map(|c| c.metadata.actor)
        .collect();
    assert!(
        actors.contains(&Some(P0)),
        "reach: the pending seat has candidates"
    );
    assert!(
        !actors.contains(&Some(P1)),
        "a declared seat is not driven again"
    );

    let (mut state, _) = standard(P0);
    let hand_before = hand(&state, P1);
    act(&mut state, P1, MulliganChoice::Mulligan);
    assert_ne!(
        hand(&state, P1),
        hand_before,
        "a per-player library redraws at once"
    );
    let WaitingFor::MulliganDecision {
        pending, declared, ..
    } = &state.waiting_for
    else {
        panic!("expected MulliganDecision");
    };
    assert!(declared.is_empty());
    assert_eq!(
        pending
            .iter()
            .find(|e| e.player == P1)
            .map(|e| e.mulligan_count),
        Some(1)
    );
}

#[test]
fn v4b_the_round_closes_when_the_last_player_keeps() {
    let Some(db) = shared_card_db() else { return };
    let (mut state, _) = dandan(db, P0);
    let (p0_hand, p1_hand, pile_before) = (hand(&state, P0), hand(&state, P1), pile(&state));
    act(&mut state, P1, MulliganChoice::Mulligan);

    let events = act(&mut state, P0, MulliganChoice::Keep);

    assert_eq!(hand(&state, P0), p0_hand);
    let new_hand = hand(&state, P1);
    assert_eq!(new_hand.len(), 7);
    assert_ne!(new_hand, p1_hand, "reach: the redraw changed the hand");
    assert_eq!(pile(&state).len(), pile_before.len());
    assert!(p1_hand
        .iter()
        .all(|id| pile(&state).contains(id) || new_hand.contains(id)));
    assert!(!events.iter().any(|e| matches!(
        e,
        GameEvent::PlayerPerformedAction {
            action: PlayerActionKind::ShuffledLibrary,
            ..
        }
    )));
    assert_eq!(declare_state(&state), (vec![P1], vec![]));

    act(&mut state, P1, MulliganChoice::Keep);
    let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
        panic!("expected P1 to owe a bottom, got {:?}", state.waiting_for);
    };
    assert!(matches!(
        pending[0].phase,
        MulliganDecisionPhase::BottomCards { count: 1, .. }
    ));
    let bottomed = hand(&state, P1)[0];
    apply(
        &mut state,
        P1,
        GameAction::SelectCards {
            cards: vec![bottomed],
        },
    )
    .expect("bottom one card");
    assert!(!matches!(
        state.waiting_for,
        WaitingFor::MulliganDecision { .. }
    ));
    assert_eq!(pile(&state).last(), Some(&bottomed));
    assert_eq!((hand(&state, P0).len(), hand(&state, P1).len()), (7, 6));
}

#[test]
fn v4c_both_redraw_together_from_one_shuffled_pile_active_player_first() {
    let Some(db) = shared_card_db() else { return };
    for starting in [P0, P1] {
        let (mut state, _) = dandan(db, starting);
        let (first, second) = (other(starting), starting);
        let (old_first, old_second) = (hand(&state, first), hand(&state, second));
        act(&mut state, first, MulliganChoice::Mulligan);

        // The close returns the active player's hand first, then shuffles the pile once.
        let mut replay_pile: Vec<ObjectId> = pile(&state);
        replay_pile.extend(old_second.iter().chain(&old_first));
        let mut shuffled: im::Vector<ObjectId> = replay_pile.iter().copied().collect();
        let mut rng = state.rng.clone();
        engine::util::im_ext::shuffle_vector(&mut shuffled, &mut rng);
        let shuffled: Vec<ObjectId> = shuffled.into_iter().collect();

        let events = act(&mut state, second, MulliganChoice::Mulligan);

        let moves: Vec<(Option<Zone>, Zone)> = events
            .iter()
            .filter_map(|e| match e {
                GameEvent::ZoneChanged { from, to, .. } => Some((*from, *to)),
                _ => None,
            })
            .collect();
        let expected: Vec<(Option<Zone>, Zone)> = [
            vec![(Some(Zone::Hand), Zone::Library); 14],
            vec![(Some(Zone::Library), Zone::Hand); 14],
        ]
        .concat();
        assert_eq!(
            moves, expected,
            "{starting:?}: every hand returns before the deal"
        );
        assert_eq!(
            deal_sequence(&state, &events),
            alternating(starting, other(starting))
        );
        assert_eq!(
            pile(&state),
            shuffled[14..].to_vec(),
            "{starting:?}: one shuffle of the returned hands"
        );
        assert_ne!(
            pile(&state),
            replay_pile[14..].to_vec(),
            "reach: the shuffle moved the pile"
        );
        assert!(!events.iter().any(|e| matches!(
            e,
            GameEvent::PlayerPerformedAction {
                action: PlayerActionKind::ShuffledLibrary,
                ..
            }
        )));
        assert_eq!(hand(&state, P0).len(), 7);
        assert_eq!(hand(&state, P1).len(), 7);
        assert_ne!(
            hand(&state, first),
            old_first,
            "reach: the redraw changed the hand"
        );
        assert!(declare_state(&state).1.is_empty());
    }
}

#[test]
fn v4d_the_round_waits_for_a_keeper_to_bottom_first() {
    let Some(db) = shared_card_db() else { return };
    let (mut state, _) = dandan(db, P0);
    act(&mut state, P0, MulliganChoice::Mulligan);
    act(&mut state, P1, MulliganChoice::Mulligan);
    assert_eq!(
        declare_state(&state),
        (vec![P0, P1], vec![]),
        "reach: round one closed"
    );

    act(&mut state, P1, MulliganChoice::Mulligan);
    let p1_hand = hand(&state, P1);
    act(&mut state, P0, MulliganChoice::Keep);
    assert_eq!(declare_state(&state), (vec![P0], vec![P1]));
    assert_eq!(
        hand(&state, P1),
        p1_hand,
        "no redraw while a bottom is owed"
    );

    let bottomed = hand(&state, P0)[0];
    apply(
        &mut state,
        P0,
        GameAction::SelectCards {
            cards: vec![bottomed],
        },
    )
    .expect("bottom one card");
    assert_ne!(hand(&state, P1), p1_hand, "the redraw followed the bottom");
    assert!(pile(&state).contains(&bottomed));
    assert!(state.players.iter().all(|p| !p.hand.contains(&bottomed)));
}

#[test]
fn v4e_serum_powder_stays_an_immediate_action_under_the_round() {
    let Some(db) = shared_card_db() else { return };
    let (mut state, _) = dandan(db, P0);
    let powder = create_object(
        &mut state,
        CardId(9000),
        P1,
        "Serum Powder".to_string(),
        Zone::Hand,
    );
    let old_hand = hand(&state, P1);
    assert_eq!(old_hand.len(), 8, "reach: the Powder is in hand");

    apply(
        &mut state,
        P1,
        GameAction::MulliganDecision {
            choice: MulliganChoice::UseSerumPowder { object_id: powder },
        },
    )
    .expect("Powder accepted");

    assert_eq!(state.objects[&powder].zone, Zone::Exile);
    let new_hand = hand(&state, P1);
    assert_eq!(new_hand.len(), 8);
    assert!(new_hand.iter().all(|id| !old_hand.contains(id)));
    assert_eq!(declare_state(&state), (vec![P0, P1], vec![]));

    act(&mut state, P1, MulliganChoice::Mulligan);
    assert_eq!(declare_state(&state), (vec![P0], vec![P1]));
    assert!(apply(
        &mut state,
        P1,
        GameAction::MulliganDecision {
            choice: MulliganChoice::UseSerumPowder { object_id: powder },
        },
    )
    .is_err());
}
