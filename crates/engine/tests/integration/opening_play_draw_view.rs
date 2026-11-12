//! CR 103.1 + CR 103.1a + CR 805.3 + CR 904.6: `DerivedViews::viewer_play_draw` — the
//! engine-owned side-relative play/draw position the opening-hand screen renders, and the
//! CR 103.1 determination guard shared with `turn_order` / `viewer_turn_number`.

use engine::game::derived_views::{
    derive_views, ClientGameState, ClientGameStateRef, PlayDrawPosition,
};
use engine::game::engine::start_game_with_starting_player;
use engine::game::zones::create_object;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// 20 Forests in every seat's library, so the game reaches the opening-hand
/// mulligan decision with a full seven-card hand.
fn seed_forest_libraries(state: &mut GameState) {
    let seats: Vec<PlayerId> = state.players.iter().map(|player| player.id).collect();
    for (seat_index, player) in seats.into_iter().enumerate() {
        for i in 0..20 {
            let id = create_object(
                state,
                CardId(seat_index as u64 * 100 + i),
                player,
                "Forest".to_string(),
                Zone::Library,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
        }
    }
}

/// A two-player game started with `starter`, stopped at the opening-hand screen.
fn two_player_game_at_opening_hand(starter: PlayerId) -> GameState {
    let mut state = GameState::new_two_player(42);
    seed_forest_libraries(&mut state);
    let result = start_game_with_starting_player(&mut state, starter);
    assert!(
        matches!(result.waiting_for, WaitingFor::MulliganDecision { .. }),
        "the game must stop at the opening-hand mulligan decision, got {:?}",
        result.waiting_for
    );
    assert_eq!(state.players[0].hand.len(), 7);
    assert_eq!(state.players[1].hand.len(), 7);
    state
}

#[test]
fn viewer_play_draw_is_side_relative_to_the_starting_player() {
    let state = two_player_game_at_opening_hand(P1);
    assert_eq!(
        derive_views(&state, Some(P1)).viewer_play_draw,
        Some(PlayDrawPosition::OnThePlay),
        "CR 103.1: the starting player takes the first turn, so is on the play"
    );
    assert_eq!(
        derive_views(&state, Some(P0)).viewer_play_draw,
        Some(PlayDrawPosition::OnTheDraw),
        "CR 103.1: the other player is on the draw"
    );

    let state = two_player_game_at_opening_hand(P0);
    assert_eq!(
        derive_views(&state, Some(P0)).viewer_play_draw,
        Some(PlayDrawPosition::OnThePlay),
        "CR 103.1: the position follows the starter, not a fixed seat"
    );
    assert_eq!(
        derive_views(&state, Some(P1)).viewer_play_draw,
        Some(PlayDrawPosition::OnTheDraw),
        "CR 103.1: the position follows the starter, not a fixed seat"
    );
}

#[test]
fn viewer_play_draw_rides_the_client_envelope() {
    let state = two_player_game_at_opening_hand(P1);

    let json = serde_json::to_string(&ClientGameStateRef::wrap(&state, Some(P0)))
        .expect("serialize seated envelope");
    assert!(
        json.contains(r#""viewer_play_draw":"OnTheDraw""#),
        "the seated viewer's envelope must carry its play/draw position"
    );
    let round: ClientGameState = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(
        round.derived.viewer_play_draw,
        Some(PlayDrawPosition::OnTheDraw),
        "the position must survive the JSON round-trip"
    );

    let unseated =
        serde_json::to_string(&ClientGameStateRef::wrap(&state, None)).expect("serialize");
    assert!(
        !unseated.contains("viewer_play_draw"),
        "an unseated envelope has no play/draw position and omits the key"
    );
}

#[test]
fn viewer_play_draw_puts_a_two_headed_giant_team_on_the_play_together() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    start_game_with_starting_player(&mut state, P2);
    assert_eq!(state.current_starting_player, P2);
    assert_eq!(state.turn_number, 1);
    assert!(
        derive_views(&state, Some(P0)).turn_order.is_empty(),
        "a two-team game publishes no multiplayer turn order"
    );

    for seat in [P2, P3] {
        assert_eq!(
            derive_views(&state, Some(seat)).viewer_play_draw,
            Some(PlayDrawPosition::OnThePlay),
            "CR 103.1a + CR 805.3: every member of the starting team is on the play ({seat:?})"
        );
    }
    for seat in [P0, P1] {
        assert_eq!(
            derive_views(&state, Some(seat)).viewer_play_draw,
            Some(PlayDrawPosition::OnTheDraw),
            "CR 103.1a + CR 805.3: the other team is on the draw ({seat:?})"
        );
    }
}

#[test]
fn viewer_play_draw_follows_the_archenemy_not_the_requested_starter() {
    let mut state = GameState::new(FormatConfig::archenemy(), 4, 42);
    start_game_with_starting_player(&mut state, P2);
    assert_eq!(state.turn_number, 1);
    let archenemy = derive_views(&state, None)
        .archenemy
        .expect("Archenemy publishes its view")
        .archenemy;
    assert_ne!(
        archenemy, P2,
        "the requested starter must differ from the archenemy for this to be a multi-authority case"
    );
    assert_eq!(
        state.current_starting_player, archenemy,
        "CR 103.1b + CR 904.6: the archenemy takes the first turn"
    );

    assert_eq!(
        derive_views(&state, Some(archenemy)).viewer_play_draw,
        Some(PlayDrawPosition::OnThePlay),
        "CR 904.6: the archenemy is on the play"
    );
    for seat in [P0, P1, P2, P3]
        .into_iter()
        .filter(|&seat| seat != archenemy)
    {
        assert_eq!(
            derive_views(&state, Some(seat)).viewer_play_draw,
            Some(PlayDrawPosition::OnTheDraw),
            "CR 103.1b: every hero is on the draw, the requested starter included ({seat:?})"
        );
    }
}

#[test]
fn multiplayer_viewer_gets_a_turn_number_instead_of_play_draw() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
    start_game_with_starting_player(&mut state, P2);

    // CR 103.1: turn order begins with the starting player and proceeds clockwise.
    for (seat, turn) in [(P2, 1), (P3, 2), (P0, 3), (P1, 4)] {
        let views = derive_views(&state, Some(seat));
        assert_eq!(
            views.viewer_turn_number,
            Some(turn),
            "CR 103.1: {seat:?} takes turn {turn}"
        );
        assert_eq!(
            views.viewer_play_draw, None,
            "a game of more than two sides has no play/draw position ({seat:?})"
        );
    }
}

#[test]
fn no_play_draw_or_turn_order_before_the_starting_player_is_determined() {
    let mut two_player = GameState::new_two_player(42);
    two_player.current_starting_player = P1;
    assert_eq!(
        derive_views(&two_player, Some(P0)).viewer_play_draw,
        None,
        "CR 103.1: no position exists before the starting player is determined"
    );

    let mut pod = GameState::new(FormatConfig::free_for_all(), 4, 42);
    let undetermined = derive_views(&pod, Some(P0));
    assert!(
        undetermined.turn_order.is_empty(),
        "CR 103.1: no turn order exists before the starting player is determined"
    );
    assert_eq!(undetermined.viewer_turn_number, None);

    // The same states once a starting player is determined: the guard was the refusal.
    two_player.turn_number = 1;
    assert_eq!(
        derive_views(&two_player, Some(P0)).viewer_play_draw,
        Some(PlayDrawPosition::OnTheDraw)
    );
    pod.turn_number = 1;
    let determined = derive_views(&pod, Some(P0));
    assert!(!determined.turn_order.is_empty());
    assert!(determined.viewer_turn_number.is_some());
}

#[test]
fn viewer_play_draw_needs_a_seated_viewer() {
    let state = two_player_game_at_opening_hand(P1);
    assert!(
        derive_views(&state, Some(P0)).viewer_play_draw.is_some(),
        "a seated viewer of the same state has a position"
    );
    assert_eq!(
        derive_views(&state, None).viewer_play_draw,
        None,
        "a projection with no viewer has no position"
    );
    assert_eq!(
        derive_views(&state, Some(PlayerId(7))).viewer_play_draw,
        None,
        "a viewer the game does not seat has no position"
    );
}
