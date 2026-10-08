//! The Dandan free-reveal mulligan (CR 103.5 as modified by the Dandan rule): a
//! seat whose hand has fewer than two lands or fewer than two nonland cards may
//! reveal it and redraw, before its first regular mulligan, without taking a
//! mulligan.

use engine::ai_support::{
    build_decision_context_for_semantic_owner, candidate_actions, candidate_actions_exact,
    legal_actions, legal_actions_for_viewer, legal_actions_full, validated_candidate_actions,
};
use engine::database::card_db::CardDatabase;
use engine::game::deck_loading::{load_and_hydrate_decks, DeckPayload};
use engine::game::engine::{apply, start_game_with_starting_player};
use engine::game::scenario::{P0, P1};
use engine::game::zones::{add_to_zone, remove_from_zone};
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{
    GameState, MulliganDecisionPhase, MulliganDeclarationKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

fn dandan(db: &CardDatabase, starting: PlayerId) -> GameState {
    let mut state = GameState::new(FormatConfig::dandan(), 2, 7);
    load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(db));
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

fn pile(state: &GameState) -> Vec<ObjectId> {
    state.library_of(P0).iter().copied().collect()
}

fn is_land(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id]
        .card_types
        .core_types
        .contains(&CoreType::Land)
}

fn names(state: &GameState, ids: &[ObjectId]) -> Vec<String> {
    ids.iter()
        .map(|id| state.objects[id].name.clone())
        .collect()
}

/// The seat's own hand followed by the shared pile: the cards an arrangement may draw on.
fn pool(state: &GameState, seat: PlayerId) -> Vec<ObjectId> {
    hand(state, seat).into_iter().chain(pile(state)).collect()
}

/// Make `wanted` the seat's whole hand, swapping with the shared pile.
fn set_hand(state: &mut GameState, seat: PlayerId, wanted: &[ObjectId]) {
    for id in hand(state, seat) {
        if !wanted.contains(&id) {
            remove_from_zone(state, id, Zone::Hand, seat);
            add_to_zone(state, id, Zone::Library, P0);
            state.objects.get_mut(&id).unwrap().zone = Zone::Library;
        }
    }
    for &id in wanted {
        if !hand(state, seat).contains(&id) {
            remove_from_zone(state, id, Zone::Library, P0);
            add_to_zone(state, id, Zone::Hand, seat);
            let obj = state.objects.get_mut(&id).unwrap();
            obj.zone = Zone::Hand;
            obj.owner = seat;
        }
    }
    assert_eq!(hand(state, seat).len(), wanted.len());
}

/// Give `seat` a hand of exactly `lands` lands and `nonlands` nonland cards.
fn arrange_counts(state: &mut GameState, seat: PlayerId, lands: usize, nonlands: usize) {
    let pool = pool(state, seat);
    let mut wanted: Vec<ObjectId> = pool
        .iter()
        .copied()
        .filter(|&id| is_land(state, id))
        .take(lands)
        .collect();
    wanted.extend(
        pool.iter()
            .copied()
            .filter(|&id| !is_land(state, id))
            .take(nonlands),
    );
    assert_eq!(
        wanted.len(),
        lands + nonlands,
        "the pile can supply the hand"
    );
    set_hand(state, seat, &wanted);
}

/// Give `seat` a hand holding exactly the named real cards.
fn arrange_named(state: &mut GameState, seat: PlayerId, card_names: &[&str]) {
    let pool = pool(state, seat);
    let mut wanted: Vec<ObjectId> = Vec::new();
    for name in card_names {
        let id = pool
            .iter()
            .copied()
            .find(|id| state.objects[id].name == *name && !wanted.contains(id))
            .unwrap_or_else(|| panic!("{name} is in the pile or the hand"));
        wanted.push(id);
    }
    set_hand(state, seat, &wanted);
}

fn is_free_reveal(action: &GameAction) -> bool {
    matches!(
        action,
        GameAction::MulliganDecision {
            choice: MulliganChoice::FreeReveal
        }
    )
}

fn offered(state: &GameState, seat: PlayerId) -> bool {
    legal_actions_for_viewer(state, seat)
        .0
        .iter()
        .any(is_free_reveal)
}

fn try_act(
    state: &mut GameState,
    seat: PlayerId,
    choice: MulliganChoice,
) -> Result<Vec<GameEvent>, String> {
    apply(state, seat, GameAction::MulliganDecision { choice })
        .map(|r| r.events)
        .map_err(|e| format!("{e:?}"))
}

fn act(state: &mut GameState, seat: PlayerId, choice: MulliganChoice) -> Vec<GameEvent> {
    let label = format!("{seat:?} {choice:?}");
    try_act(state, seat, choice).unwrap_or_else(|e| panic!("{label}: {e}"))
}

/// (pending seats, declared seats and their kind) of the open round.
fn round(state: &GameState) -> (Vec<PlayerId>, Vec<(PlayerId, MulliganDeclarationKind)>) {
    let WaitingFor::MulliganDecision {
        pending, declared, ..
    } = &state.waiting_for
    else {
        panic!("expected MulliganDecision, got {:?}", state.waiting_for);
    };
    (
        pending.iter().map(|e| e.player).collect(),
        declared.iter().map(|d| (d.player, d.kind)).collect(),
    )
}

fn entry(state: &GameState, seat: PlayerId) -> (u8, MulliganDecisionPhase) {
    let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
        panic!("expected MulliganDecision, got {:?}", state.waiting_for);
    };
    let e = pending
        .iter()
        .find(|e| e.player == seat)
        .unwrap_or_else(|| panic!("{seat:?} is pending"));
    (e.mulligan_count, e.phase)
}

fn moves(events: &[GameEvent]) -> Vec<(Option<Zone>, Zone)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::ZoneChanged { from, to, .. } => Some((*from, *to)),
            _ => None,
        })
        .collect()
}

fn reveals(events: &[GameEvent]) -> Vec<(PlayerId, Vec<ObjectId>, Vec<String>)> {
    events
        .iter()
        .filter_map(|e| match e {
            GameEvent::CardsRevealed {
                player,
                card_ids,
                card_names,
            } => Some((*player, card_ids.clone(), card_names.clone())),
            _ => None,
        })
        .collect()
}

/// No object is publicly revealed and no player holds remembered knowledge of any card.
fn assert_nothing_disclosed(state: &GameState) {
    assert!(state.public_revealed_cards.is_empty());
    for &id in state.objects.keys() {
        for seat in [P0, P1] {
            assert!(
                !state.viewer_knows_card_identity(seat, id),
                "{seat:?} must not know {id:?}"
            );
        }
    }
}

#[test]
fn v1_the_hand_predicate_is_fewer_than_two_lands_or_fewer_than_two_nonlands() {
    let Some(db) = shared_card_db() else { return };
    for (lands, nonlands, qualifies) in [
        (0, 7, true),
        (1, 6, true),
        (6, 1, true),
        (7, 0, true),
        (2, 5, false),
        (5, 2, false),
        (3, 4, false),
    ] {
        let mut state = dandan(db, P0);
        arrange_counts(&mut state, P0, lands, nonlands);
        assert_eq!(
            offered(&state, P0),
            qualifies,
            "({lands},{nonlands}) offered"
        );
        assert_eq!(
            offered(&state, P1),
            is_qualifying(&state, P1),
            "each seat's own hand decides its own offer"
        );

        let before = state.waiting_for.clone();
        let result = try_act(&mut state, P0, MulliganChoice::FreeReveal);
        assert_eq!(result.is_ok(), qualifies, "({lands},{nonlands}) accepted");
        if qualifies {
            assert_eq!(
                round(&state),
                (vec![P1], vec![(P0, MulliganDeclarationKind::FreeReveal)])
            );
        } else {
            assert_eq!(state.waiting_for, before, "a refusal changes nothing");
        }
    }
}

fn is_qualifying(state: &GameState, seat: PlayerId) -> bool {
    let h = hand(state, seat);
    let lands = h.iter().filter(|&&id| is_land(state, id)).count();
    lands < 2 || h.len() - lands < 2
}

#[test]
fn v1_lands_are_counted_by_card_type_not_by_subtype() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dandan(db, P0);
    // Mystic Sanctuary is a Land - Island, Dandan a creature: two lands, five nonlands.
    arrange_named(
        &mut state,
        P0,
        &[
            "Island",
            "Mystic Sanctuary",
            "Dandân",
            "Dandân",
            "Dandân",
            "Dandân",
            "Dandân",
        ],
    );
    assert!(!offered(&state, P0));
    assert!(try_act(&mut state, P0, MulliganChoice::FreeReveal).is_err());

    arrange_named(
        &mut state,
        P0,
        &[
            "Island", "Dandân", "Dandân", "Dandân", "Dandân", "Dandân", "Dandân",
        ],
    );
    assert!(offered(&state, P0), "reach: one land fewer qualifies");
    act(&mut state, P0, MulliganChoice::FreeReveal);
}

#[test]
fn v2_a_regular_mulligan_ends_the_free_reveal() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dandan(db, P0);
    act(&mut state, P1, MulliganChoice::Mulligan);
    act(&mut state, P0, MulliganChoice::Keep);
    assert_eq!(entry(&state, P1).0, 1, "reach: P1 took a regular mulligan");

    arrange_counts(&mut state, P1, 0, 7);
    assert!(!offered(&state, P1));
    assert!(try_act(&mut state, P1, MulliganChoice::FreeReveal).is_err());

    let mut twin = dandan(db, P0);
    arrange_counts(&mut twin, P1, 0, 7);
    assert_eq!(entry(&twin, P1).0, 0);
    assert!(offered(&twin, P1), "the same hand at count 0 is offered");
}

/// The free-reveal close for `declarer`, who declares first; the other seat keeps.
fn free_reveal_round(db: &CardDatabase, starting: PlayerId, declarer: PlayerId) {
    let keeper = other(declarer);
    let mut state = dandan(db, starting);
    arrange_counts(&mut state, declarer, 1, 6);
    let old_hand = hand(&state, declarer);
    let old_names = names(&state, &old_hand);
    let keeper_hand = hand(&state, keeper);
    let pile_before = pile(&state);
    assert_nothing_disclosed(&state);

    let events = act(&mut state, declarer, MulliganChoice::FreeReveal);

    assert!(moves(&events).is_empty() && reveals(&events).is_empty());
    assert_eq!(hand(&state, declarer), old_hand);
    assert_eq!(pile(&state), pile_before);
    assert_eq!(
        round(&state),
        (
            vec![keeper],
            vec![(declarer, MulliganDeclarationKind::FreeReveal)]
        )
    );

    let mut replay_pile = pile(&state);
    replay_pile.extend(&old_hand);
    let mut shuffled: im::Vector<ObjectId> = replay_pile.iter().copied().collect();
    let mut rng = state.rng.clone();
    engine::util::im_ext::shuffle_vector(&mut shuffled, &mut rng);
    let shuffled: Vec<ObjectId> = shuffled.into_iter().collect();

    let events = act(&mut state, keeper, MulliganChoice::Keep);

    assert_eq!(
        reveals(&events),
        vec![(declarer, Vec::new(), old_names)],
        "one reveal of the old hand by name, with no object ids"
    );
    let first_reveal = events
        .iter()
        .position(|e| matches!(e, GameEvent::CardsRevealed { .. }))
        .unwrap();
    let first_return = events
        .iter()
        .position(|e| {
            matches!(
                e,
                GameEvent::ZoneChanged {
                    from: Some(Zone::Hand),
                    to: Zone::Library,
                    ..
                }
            )
        })
        .unwrap();
    assert!(
        first_reveal < first_return,
        "the reveal precedes the return"
    );
    assert_eq!(
        moves(&events),
        [
            vec![(Some(Zone::Hand), Zone::Library); 7],
            vec![(Some(Zone::Library), Zone::Hand); 7],
        ]
        .concat()
    );
    assert_eq!(
        pile(&state),
        shuffled[7..].to_vec(),
        "one shuffle of the pile"
    );
    assert_ne!(
        pile(&state),
        replay_pile[7..].to_vec(),
        "reach: the shuffle moved the pile"
    );
    assert_eq!(hand(&state, declarer), shuffled[..7].to_vec());
    assert_eq!(hand(&state, keeper), keeper_hand);
    assert_eq!(pile(&state).len(), 80 - 14);
    assert_eq!(round(&state), (vec![declarer], vec![]));
    assert_eq!(entry(&state, declarer), (0, MulliganDecisionPhase::Declare));
    assert_nothing_disclosed(&state);

    act(&mut state, declarer, MulliganChoice::Keep);
    assert!(
        !matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }),
        "a free reveal owes no bottom: the game starts, got {:?}",
        state.waiting_for
    );
    assert_eq!((hand(&state, P0).len(), hand(&state, P1).len()), (7, 7));
}

#[test]
fn v3_a_free_reveal_is_held_then_revealed_shuffled_and_redealt_at_the_close() {
    let Some(db) = shared_card_db() else { return };
    // Non-active, non-canonical declarer; then the canonical seat (the pile's holder).
    free_reveal_round(db, P0, P1);
    free_reveal_round(db, P0, P0);
    free_reveal_round(db, P1, P1);
}

#[test]
fn v3_a_regular_close_discloses_nothing_either() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dandan(db, P0);
    act(&mut state, P1, MulliganChoice::Mulligan);
    let events = act(&mut state, P0, MulliganChoice::Keep);
    assert!(reveals(&events).is_empty());
    assert_nothing_disclosed(&state);
}

#[test]
fn v4_free_reveal_and_regular_mulligans_close_together_with_their_own_counts() {
    let Some(db) = shared_card_db() else { return };
    for starting in [P0, P1] {
        let mut state = dandan(db, starting);
        arrange_counts(&mut state, P1, 1, 6);
        let p1_names = names(&state, &hand(&state, P1));
        act(&mut state, P1, MulliganChoice::FreeReveal);

        let events = act(&mut state, P0, MulliganChoice::Mulligan);

        assert_eq!(reveals(&events), vec![(P1, Vec::new(), p1_names)]);
        assert_eq!(
            moves(&events),
            [
                vec![(Some(Zone::Hand), Zone::Library); 14],
                vec![(Some(Zone::Library), Zone::Hand); 14],
            ]
            .concat(),
            "{starting:?}: every hand returns before the deal"
        );
        assert_eq!(entry(&state, P0).0, 1, "the regular declarer counts");
        assert_eq!(entry(&state, P1).0, 0, "the free reveal does not");
        assert_eq!(round(&state).1, vec![]);

        act(&mut state, P1, MulliganChoice::Keep);
        assert_eq!(round(&state).0, vec![P0], "P1 keeps owing nothing");
        act(&mut state, P0, MulliganChoice::Keep);
        let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for else {
            panic!("P0 owes a bottom, got {:?}", state.waiting_for);
        };
        assert_eq!(pending.len(), 1);
        assert!(matches!(
            pending[0].phase,
            MulliganDecisionPhase::BottomCards { count: 1, .. }
        ));
        assert_eq!(pending[0].player, P0);
    }
}

#[test]
fn v5_the_format_axis_gates_the_free_reveal() {
    let Some(db) = shared_card_db() else { return };
    let mut dandan_state = dandan(db, P0);
    arrange_counts(&mut dandan_state, P0, 0, 7);
    assert!(offered(&dandan_state, P0), "reach: Dandan offers it");

    let mut standard_state = dandan_state.clone();
    standard_state.format_config = FormatConfig::standard();
    assert!(!offered(&standard_state, P0));
    assert!(try_act(&mut standard_state, P0, MulliganChoice::FreeReveal).is_err());
    assert!(try_act(&mut dandan_state, P0, MulliganChoice::FreeReveal).is_ok());
}

#[test]
fn v6_a_free_reveal_that_redraws_a_qualifying_hand_may_repeat() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dandan(db, P0);
    arrange_counts(&mut state, P1, 0, 7);
    act(&mut state, P1, MulliganChoice::FreeReveal);
    act(&mut state, P0, MulliganChoice::Keep);
    assert_eq!(entry(&state, P1), (0, MulliganDecisionPhase::Declare));

    arrange_counts(&mut state, P1, 7, 0);
    assert!(offered(&state, P1));
    let events = act(&mut state, P1, MulliganChoice::FreeReveal);
    assert_eq!(reveals(&events).len(), 1);
    assert_eq!(entry(&state, P1), (0, MulliganDecisionPhase::Declare));
    assert_eq!(round(&state), (vec![P1], vec![]));
}

fn viewer_sees_free_reveal(state: &GameState, viewer: PlayerId) -> bool {
    legal_actions_for_viewer(state, viewer)
        .0
        .contains(&GameAction::MulliganDecision {
            choice: MulliganChoice::FreeReveal,
        })
}

#[test]
fn v7_a_viewer_is_offered_the_free_reveal_only_for_their_own_hand() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dandan(db, P0);
    arrange_counts(&mut state, P0, 0, 7);
    arrange_counts(&mut state, P1, 3, 4);
    let keep = GameAction::MulliganDecision {
        choice: MulliganChoice::Keep,
    };
    assert!(legal_actions_for_viewer(&state, P0).0.contains(&keep));
    assert!(legal_actions_for_viewer(&state, P1).0.contains(&keep));

    assert!(viewer_sees_free_reveal(&state, P0));
    assert!(!viewer_sees_free_reveal(&state, P1));
    assert!(try_act(&mut state.clone(), P1, MulliganChoice::FreeReveal).is_err());

    arrange_counts(&mut state, P1, 1, 6);
    assert!(viewer_sees_free_reveal(&state, P0) && viewer_sees_free_reveal(&state, P1));
    arrange_counts(&mut state, P0, 3, 4);
    arrange_counts(&mut state, P1, 3, 4);
    assert!(!viewer_sees_free_reveal(&state, P0) && !viewer_sees_free_reveal(&state, P1));
}

#[test]
fn v8_the_free_reveal_is_only_in_the_lists_scoped_to_its_own_seat() {
    let Some(db) = shared_card_db() else { return };
    let mut state = dandan(db, P0);
    arrange_counts(&mut state, P0, 0, 7);
    arrange_counts(&mut state, P1, 3, 4);
    let keep = GameAction::MulliganDecision {
        choice: MulliganChoice::Keep,
    };
    let unscoped: [(&str, Vec<GameAction>); 5] = [
        ("legal_actions_full", legal_actions_full(&state).0),
        ("legal_actions", legal_actions(&state)),
        (
            "candidate_actions",
            candidate_actions(&state)
                .into_iter()
                .map(|c| c.action)
                .collect(),
        ),
        (
            "candidate_actions_exact",
            candidate_actions_exact(&state)
                .into_iter()
                .map(|c| c.action)
                .collect(),
        ),
        (
            "validated_candidate_actions",
            validated_candidate_actions(&state)
                .into_iter()
                .map(|c| c.action)
                .collect(),
        ),
    ];
    for (name, actions) in &unscoped {
        assert!(
            actions.contains(&keep),
            "reach: {name} enumerates the prompt"
        );
        assert!(
            !actions.iter().any(is_free_reveal),
            "{name} must not carry a seat-specific FreeReveal"
        );
    }

    let ai_issued = |seat| {
        build_decision_context_for_semantic_owner(&state, seat)
            .candidates
            .into_iter()
            .map(|c| c.action)
            .collect::<Vec<_>>()
    };
    for seat in [P0, P1] {
        assert!(ai_issued(seat).contains(&keep), "reach: Keep for {seat:?}");
    }
    assert!(ai_issued(P0).iter().any(is_free_reveal));
    assert!(!ai_issued(P1).iter().any(is_free_reveal));
    assert!(offered(&state, P0) && !offered(&state, P1));
}
