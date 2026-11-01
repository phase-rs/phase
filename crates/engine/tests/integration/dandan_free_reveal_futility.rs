//! The Dandan free-reveal futility fact (CR 103.5 as modified by the Dandan
//! rule) is a function of the format axis and the registered pile, and the
//! per-seat count of free reveals taken rides the entry and the declaration.

use engine::ai_support::legal_actions_for_viewer;
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{
    load_and_hydrate_decks, resolve_deck_list, DeckList, DeckPayload, PlayerDeckList,
};
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::mulligan::free_reveal_futile_for;
use engine::game::scenario::{P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    GameState, MulliganDecisionEntry, MulliganDeclaration, MulliganDeclarationKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const ISLANDS: [&str; 7] = ["Island"; 7];
const FIVE_ISLANDS_TWO_OPTS: [&str; 7] = [
    "Island", "Island", "Island", "Island", "Island", "Opt", "Opt",
];
const TWO_OPTS: [(&str, usize); 2] = [("Island", 78), ("Opt", 2)];

fn payload(db: &CardDatabase, pile: &[(&str, usize)]) -> DeckPayload {
    let main_deck = pile
        .iter()
        .flat_map(|&(name, copies)| std::iter::repeat_n(name.to_string(), copies))
        .collect();
    resolve_deck_list(
        db,
        &DeckList {
            player: PlayerDeckList {
                main_deck,
                ..Default::default()
            },
            ..Default::default()
        },
    )
}

fn dealt(db: &CardDatabase, pile: &DeckPayload, seed: u64, starting: PlayerId) -> GameState {
    let mut state = GameState::new(FormatConfig::dandan(), 2, seed);
    load_and_hydrate_decks(&mut state, pile, Some(db));
    start_game_with_starting_player(&mut state, starting);
    state
}

fn other(seat: PlayerId) -> PlayerId {
    if seat == P0 {
        P1
    } else {
        P0
    }
}

fn hand(state: &GameState, seat: PlayerId) -> Vec<ObjectId> {
    state.players[seat.0 as usize]
        .hand
        .iter()
        .copied()
        .collect()
}

/// Make the named cards `seat`'s whole hand, swapping with the shared pile.
fn set_hand(state: &mut GameState, seat: PlayerId, names: &[&str]) {
    for id in hand(state, seat) {
        remove_from_zone(state, id, Zone::Hand, seat);
        add_to_zone(state, id, Zone::Library, P0);
        state.objects.get_mut(&id).unwrap().zone = Zone::Library;
    }
    for name in names {
        let id = state
            .library_of(P0)
            .iter()
            .copied()
            .find(|id| state.objects[id].name == *name)
            .unwrap_or_else(|| panic!("{name} is in the pile"));
        remove_from_zone(state, id, Zone::Library, P0);
        add_to_zone(state, id, Zone::Hand, seat);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.zone = Zone::Hand;
        obj.owner = seat;
    }
}

fn free_reveal() -> GameAction {
    GameAction::MulliganDecision {
        choice: MulliganChoice::FreeReveal,
    }
}

fn offered(state: &GameState, seat: PlayerId) -> bool {
    legal_actions_for_viewer(state, seat)
        .0
        .contains(&free_reveal())
}

fn act(state: &mut GameState, seat: PlayerId, choice: MulliganChoice) {
    let label = format!("{seat:?} {choice:?}");
    apply(state, seat, GameAction::MulliganDecision { choice })
        .unwrap_or_else(|e| panic!("{label}: {e:?}"));
}

fn accepts_free_reveal(state: &GameState, seat: PlayerId) -> bool {
    apply(&mut state.clone(), seat, free_reveal()).is_ok()
}

/// (pending seats, declared seats) of the open round.
fn round(state: &GameState) -> (Vec<PlayerId>, Vec<PlayerId>) {
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

/// Every orientation: (starting player, futile seat).
fn orientations() -> impl Iterator<Item = (PlayerId, PlayerId)> {
    [P0, P1]
        .into_iter()
        .flat_map(|starting| [P0, P1].map(|seat| (starting, seat)))
}

/// (open entries as `(seat, mulligan_count, free_reveals_taken)`).
fn entries(state: &GameState) -> Vec<(PlayerId, u8, u8)> {
    let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
        panic!("expected MulliganDecision, got {:?}", state.waiting_for);
    };
    pending
        .iter()
        .map(|e| (e.player, e.mulligan_count, e.free_reveals_taken))
        .collect()
}

fn is_land(state: &GameState, id: &ObjectId) -> bool {
    state.objects[id]
        .card_types
        .core_types
        .contains(&CoreType::Land)
}

/// Rejected rule: the library plus the seat's own hand cannot clear.
fn library_and_own_hand_model(state: &GameState, seat: PlayerId) -> bool {
    let cards: Vec<ObjectId> = state
        .library_of(seat)
        .iter()
        .copied()
        .chain(hand(state, seat))
        .collect();
    let lands = cards.iter().filter(|id| is_land(state, id)).count();
    lands < 2 || cards.len() - lands < 2
}

/// Rejected rule: the registered list minus the other seat's hand cannot clear.
fn list_minus_other_hand_model(state: &GameState, seat: PlayerId) -> bool {
    let pool = state.deck_pool_of(seat).expect("registered pile");
    let (mut lands, mut nonlands) = (0usize, 0usize);
    for entry in pool.current_main.iter() {
        let count = entry.count as usize;
        if entry.card.card_type.core_types.contains(&CoreType::Land) {
            lands += count;
        } else {
            nonlands += count;
        }
    }
    for id in hand(state, other(seat)) {
        if is_land(state, &id) {
            lands -= 1;
        } else {
            nonlands -= 1;
        }
    }
    lands < 2 || nonlands < 2
}

#[test]
fn the_fact_is_a_function_of_the_registered_pile() {
    let Some(db) = shared_card_db() else { return };
    for (pile, futile) in [
        (&[("Island", 80)][..], true),
        (&[("Island", 79), ("Opt", 1)][..], true),
        (&[("Island", 1), ("Opt", 79)][..], true),
        (&[("Island", 78), ("Opt", 2)][..], false),
        (&[("Island", 2), ("Opt", 78)][..], false),
    ] {
        let payload = payload(db, pile);
        for seed in 0..3 {
            let state = dealt(db, &payload, seed, P0);
            for seat in [P0, P1] {
                assert!(
                    state
                        .deck_pool_of(seat)
                        .is_some_and(|pool| !pool.current_main.is_empty()),
                    "reach: {pile:?} registers a pile"
                );
                assert_eq!(
                    free_reveal_futile_for(&state, seat),
                    futile,
                    "{pile:?} seed {seed} {seat:?}"
                );
                if futile {
                    assert!(offered(&state, seat), "reach: the reveal is offered");
                    assert!(accepts_free_reveal(&state, seat), "the reveal stays legal");
                }
            }
        }
    }
    let state = dealt(db, &DeckPayload::default(), 0, P0);
    assert!(!free_reveal_futile_for(&state, P0), "the default list");
}

#[test]
fn the_fact_does_not_move_with_any_other_zone() {
    let Some(db) = shared_card_db() else { return };
    let pile = payload(db, &TWO_OPTS);
    for (starting, seat) in orientations() {
        let mut facts = Vec::new();
        let mut zone_models = Vec::new();
        let mut list_models = Vec::new();
        for (kept_hand, keeps) in [
            (&ISLANDS, true),
            (&FIVE_ISLANDS_TWO_OPTS, true),
            (&ISLANDS, false),
            (&FIVE_ISLANDS_TWO_OPTS, false),
        ] {
            let mut state = dealt(db, &pile, 1, starting);
            set_hand(&mut state, seat, &ISLANDS);
            set_hand(&mut state, other(seat), kept_hand);
            if keeps {
                act(&mut state, other(seat), MulliganChoice::Keep);
                assert_eq!(
                    round(&state),
                    (vec![seat], vec![]),
                    "reach: the other seat kept"
                );
            } else {
                assert!(
                    round(&state).0.contains(&other(seat)),
                    "reach: the other seat is pending"
                );
            }
            assert!(offered(&state, seat), "reach: {seat:?} is offered");
            facts.push(free_reveal_futile_for(&state, seat));
            zone_models.push(library_and_own_hand_model(&state, seat));
            list_models.push(list_minus_other_hand_model(&state, seat));
        }
        assert_eq!(facts, [false; 4], "{starting:?} {seat:?}");
        for (name, model) in [
            ("library plus own hand", zone_models),
            ("list minus other hands", list_models),
        ] {
            assert!(
                model.contains(&true) && model.contains(&false),
                "reach: the rejected rule `{name}` flips on this set: {model:?}"
            );
        }
    }
}

#[test]
fn redacted_pool_less_and_other_format_states_are_not_futile() {
    let Some(db) = shared_card_db() else { return };
    let state = dealt(db, &payload(db, &[("Island", 80)]), 1, P0);
    assert!(
        free_reveal_futile_for(&state, P0),
        "reach: the populated state is futile"
    );
    for viewer in [P0, P1] {
        let projected = filter_state_for_viewer(&state, viewer);
        assert!(
            projected
                .deck_pool_of(P0)
                .is_some_and(|pool| pool.current_main.is_empty()),
            "reach: the projection empties the pile"
        );
        assert!(!free_reveal_futile_for(&projected, P0), "viewer {viewer:?}");
    }
    let mut pool_less = state.clone();
    pool_less.deck_pools.clear();
    assert!(!free_reveal_futile_for(&pool_less, P0));
    let mut standard = state.clone();
    standard.format_config = FormatConfig::standard();
    assert!(!free_reveal_futile_for(&standard, P0));
}

#[test]
fn the_free_reveal_count_is_per_seat_and_carried_by_regular_mulligans() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dealt(db, &payload(db, &[("Island", 80)]), 3, P0);
    assert_eq!(entries(&state), vec![(P0, 0, 0), (P1, 0, 0)]);

    act(&mut state, P0, MulliganChoice::FreeReveal);
    let WaitingFor::MulliganDecision { declared, .. } = &state.waiting_for else {
        panic!("expected MulliganDecision");
    };
    assert_eq!(declared.len(), 1, "reach: P0 declared");
    assert_eq!(
        declared[0].free_reveals_taken, 0,
        "the declaration carries the count before the reveal"
    );
    act(&mut state, P1, MulliganChoice::FreeReveal);
    assert_eq!(entries(&state), vec![(P0, 0, 1), (P1, 0, 1)]);

    act(&mut state, P0, MulliganChoice::FreeReveal);
    act(&mut state, P1, MulliganChoice::Mulligan);
    assert_eq!(entries(&state), vec![(P0, 0, 2), (P1, 1, 1)]);
}

#[test]
fn the_free_reveal_count_saturates_instead_of_overflowing() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dealt(db, &payload(db, &[("Island", 80)]), 3, P0);
    act(&mut state, P1, MulliganChoice::Keep);
    let WaitingFor::MulliganDecision { pending, .. } = &mut state.waiting_for else {
        panic!("expected MulliganDecision");
    };
    pending[0].free_reveals_taken = u8::MAX - 1;
    for _ in 0..3 {
        act(&mut state, P0, MulliganChoice::FreeReveal);
    }
    assert_eq!(entries(&state), vec![(P0, 0, u8::MAX)]);
}

#[test]
fn an_entry_and_a_declaration_without_the_count_load_as_zero() {
    let entry: MulliganDecisionEntry =
        serde_json::from_str(r#"{"player":0,"mulligan_count":1,"phase":{"type":"Declare"}}"#)
            .expect("an entry without the count loads");
    assert_eq!(entry.free_reveals_taken, 0);
    let declaration: MulliganDeclaration =
        serde_json::from_str(r#"{"player":1,"mulligan_count":2,"kind":{"type":"FreeReveal"}}"#)
            .expect("a declaration without the count loads");
    assert_eq!(declaration.free_reveals_taken, 0);
    assert_eq!(declaration.kind, MulliganDeclarationKind::FreeReveal);

    let entry = MulliganDecisionEntry {
        free_reveals_taken: 4,
        ..entry
    };
    let back: MulliganDecisionEntry =
        serde_json::from_str(&serde_json::to_string(&entry).unwrap()).unwrap();
    assert_eq!(back.free_reveals_taken, 4);
    let declaration = MulliganDeclaration {
        free_reveals_taken: 6,
        ..declaration
    };
    let back: MulliganDeclaration =
        serde_json::from_str(&serde_json::to_string(&declaration).unwrap()).unwrap();
    assert_eq!(back.free_reveals_taken, 6);
}
