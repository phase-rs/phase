//! CR 801: Limited range of influence — snapshot foundation and CR 801.4
//! targeting. Transcribed from the Comprehensive Rules' own worked examples,
//! including the two an earlier attempt (#6279) got wrong: a departure only
//! changing range at the next turn start (CR 801.2c), and an eliminated seat no
//! longer padding the distance between the players it separated.

use engine::game::range_of_influence::{object_in_range, player_in_range, refresh_for_turn};
use engine::game::zones::create_object;
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

fn table(players: u8, range: Option<u8>) -> GameState {
    let mut config = FormatConfig::standard();
    config.range_of_influence = range;
    GameState::new(config, players, 42)
}

#[test]
fn unlimited_range_reports_every_player_in_range() {
    // No shipped format sets the option, so this is the path every existing
    // game takes: no snapshot is stored and every gate passes.
    let state = table(5, None);
    assert!(state.range_of_influence.is_none());
    assert!(player_in_range(&state, PlayerId(0), PlayerId(3)));
}

#[test]
fn range_one_reaches_only_the_adjacent_seats() {
    // CR 801.2a's own example: "a range of influence of 1 means that only you
    // and the players seated directly next to you are within your range of
    // influence."
    let mut state = table(5, Some(1));
    refresh_for_turn(&mut state);

    assert!(player_in_range(&state, PlayerId(0), PlayerId(1)));
    assert!(
        player_in_range(&state, PlayerId(0), PlayerId(4)),
        "seats wrap: the last seat is adjacent to the first"
    );
    assert!(!player_in_range(&state, PlayerId(0), PlayerId(2)));
    assert!(!player_in_range(&state, PlayerId(0), PlayerId(3)));
}

#[test]
fn range_two_reaches_two_seats_in_each_direction() {
    // CR 801.2a: "you and the two players to your left and the two players to
    // your right".
    let mut state = table(7, Some(2));
    refresh_for_turn(&mut state);

    for other in [1, 2, 5, 6] {
        assert!(player_in_range(&state, PlayerId(0), PlayerId(other)));
    }
    for other in [3, 4] {
        assert!(!player_in_range(&state, PlayerId(0), PlayerId(other)));
    }
}

#[test]
fn a_player_is_always_within_their_own_range() {
    // CR 801.2b, which holds even at range 0.
    let mut state = table(4, Some(0));
    refresh_for_turn(&mut state);

    assert!(player_in_range(&state, PlayerId(0), PlayerId(0)));
    assert!(!player_in_range(&state, PlayerId(0), PlayerId(1)));
}

#[test]
fn a_departure_only_changes_range_at_the_next_turn_start() {
    // CR 801.2c, transcribed from the rule's own example: Alex and Carissa are
    // separated by Rob. When Rob leaves, Carissa enters Alex's range — but not
    // until the next turn begins. Recomputing live (what #6279 did) would move
    // her into range immediately and change legality mid-turn.
    let alex = PlayerId(0);
    let rob = PlayerId(1);
    let carissa = PlayerId(2);

    let mut state = table(3, Some(1));
    refresh_for_turn(&mut state);
    assert!(
        !player_in_range(&state, alex, carissa),
        "precondition: at range 1 with Rob between them, Carissa is out of Alex's range"
    );

    state.eliminated_players.push(rob);
    assert!(
        !player_in_range(&state, alex, carissa),
        "CR 801.2c: Rob leaving does NOT move Carissa into range mid-turn"
    );

    refresh_for_turn(&mut state);
    assert!(
        player_in_range(&state, alex, carissa),
        "CR 801.2c: she enters Alex's range at the start of the next turn"
    );
}

#[test]
fn an_eliminated_seat_stops_padding_the_distance_between_its_neighbours() {
    // The other half of CR 801.2c's example: distance is measured over the
    // seats still in the game. With two players gone from a 5-seat table, the
    // survivors are adjacent even though their seat indices are not.
    let mut state = table(5, Some(1));
    state.eliminated_players.push(PlayerId(1));
    state.eliminated_players.push(PlayerId(2));
    refresh_for_turn(&mut state);

    assert!(
        player_in_range(&state, PlayerId(0), PlayerId(3)),
        "seats 0 and 3 are adjacent once seats 1 and 2 have left"
    );
}

#[test]
fn object_range_follows_its_controller() {
    // CR 801.2d: an object is in range because its CONTROLLER is, not for any
    // property of its own.
    let mut state = table(5, Some(1));
    refresh_for_turn(&mut state);

    let near = create_object(
        &mut state,
        CardId(1),
        PlayerId(1),
        "Adjacent Bear".into(),
        Zone::Battlefield,
    );
    let far = create_object(
        &mut state,
        CardId(2),
        PlayerId(3),
        "Distant Bear".into(),
        Zone::Battlefield,
    );

    assert!(object_in_range(&state, PlayerId(0), near));
    assert!(!object_in_range(&state, PlayerId(0), far));
}

#[test]
fn a_nonexistent_object_is_out_of_range() {
    // Fail closed: a stale id must never widen what a player can reach.
    let mut state = table(5, Some(1));
    refresh_for_turn(&mut state);
    assert!(!object_in_range(&state, PlayerId(0), ObjectId(9999)));
}
