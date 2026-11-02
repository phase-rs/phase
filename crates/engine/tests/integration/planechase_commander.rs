//! End-to-end composition probes for Planechase Commander: Commander
//! (CR 903: 100 singleton, command zone, 21 damage, 40 life, 2–6 seats)
//! composed with the communal planar deck option (CR 901.15a). Mirrors the
//! `commander_team_formats.rs` battery: construction, loading, planar
//! mechanics, and elimination through the real pipelines.

use engine::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use engine::game::derived_views::derive_views;
use engine::game::planechase::{active_plane, reveal_starting_plane};
use engine::game::sba::check_state_based_actions;
use engine::game::turns::should_skip_draw;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CommanderDamageEntry, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

fn card_entry(name: &str, count: u32) -> DeckEntry {
    DeckEntry {
        card: CardFace {
            name: name.to_string(),
            ..Default::default()
        },
        count,
    }
}

/// A 99-basic + 1-commander EDH seat submission.
fn edh_seat(commander_name: &str) -> PlayerDeckPayload {
    PlayerDeckPayload {
        main_deck: vec![card_entry("Plains", 99)],
        commander: vec![card_entry(commander_name, 1)],
        ..Default::default()
    }
}

fn plane_entry(name: &str) -> DeckEntry {
    let mut face = CardFace {
        name: name.to_string(),
        ..Default::default()
    };
    face.card_type.core_types = vec![CoreType::Plane];
    DeckEntry {
        card: face,
        count: 1,
    }
}

fn load_four_seat_game() -> GameState {
    let mut state = GameState::new(FormatConfig::planechase_commander(), 4, 7);
    let mut host_seat = edh_seat("General Zero");
    host_seat.planar_deck = vec![plane_entry("Chaotic Aether")];
    load_deck_into_state(
        &mut state,
        &DeckPayload {
            player: host_seat,
            opponent: edh_seat("General One"),
            ai_decks: vec![edh_seat("General Two"), edh_seat("General Three")],
            ai_difficulties: Vec::new(),
            booster_pack_pool: None,
        },
    );
    state
}

/// Commander governs life: 40 per seat, individual totals (no teams).
#[test]
fn planechase_commander_starts_at_forty_per_seat() {
    let state = GameState::new(FormatConfig::planechase_commander(), 4, 7);
    for seat in 0..4 {
        let life = state
            .players
            .iter()
            .find(|p| p.id == PlayerId(seat))
            .unwrap()
            .life;
        assert_eq!(life, 40, "seat {seat} starts at 40 (CR 903.7)");
    }
    assert_eq!(state.format_config.commander_damage_threshold, Some(21));
}

/// CR 903.6 + CR 901.15a: every seat's commander reaches the command zone
/// and the host's planar slot loads as the communal planar deck.
#[test]
fn planechase_commander_load_places_commanders_and_planar_deck() {
    let state = load_four_seat_game();

    for (seat, name) in [
        "General Zero",
        "General One",
        "General Two",
        "General Three",
    ]
    .into_iter()
    .enumerate()
    {
        let owner = PlayerId(seat as u8);
        let placed: Vec<String> = state
            .command_zone
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|o| o.is_commander && o.owner == owner)
            .map(|o| o.name.clone())
            .collect();
        assert_eq!(
            placed,
            vec![name.to_string()],
            "seat {seat} seats exactly its own commander"
        );
    }
    assert_eq!(
        state.planar_deck.len(),
        1,
        "the host's planar slot loads as the communal planar deck"
    );
}

/// CR 901.5 in a Commander game: the starting plane reveals from the
/// loaded communal deck.
#[test]
fn planechase_commander_reveals_starting_plane() {
    let mut state = load_four_seat_game();
    assert!(active_plane(&state).is_none());
    reveal_starting_plane(&mut state);
    assert!(
        active_plane(&state).is_some(),
        "starting plane revealed from the communal deck"
    );
}

/// The U4b regression: the derived planar view (which drives the client
/// panel and die legality) must populate for the Commander composition,
/// not just default Planechase.
#[test]
fn planechase_commander_derived_view_exposes_planar_state() {
    let state = load_four_seat_game();
    let views = derive_views(&state, Some(PlayerId(0)));
    let planechase = views
        .planechase
        .as_ref()
        .expect("planar view populated for PlanechaseCommander");
    assert_eq!(planechase.planar_deck_count, 1);
}

/// CR 903.10a on individual seats: 21 commander damage eliminates exactly
/// that player — no team cascade — and the game continues.
#[test]
fn planechase_commander_damage_eliminates_only_that_player() {
    let mut state = GameState::new(FormatConfig::planechase_commander(), 4, 7);
    state.commander_damage.push(CommanderDamageEntry {
        player: PlayerId(1),
        commander: ObjectId(999),
        damage: 21,
    });
    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(state.players[1].is_eliminated);
    for seat in [0, 2, 3] {
        assert!(!state.players[seat].is_eliminated);
    }
    assert!(
        !matches!(state.waiting_for, WaitingFor::GameOver { .. }),
        "three players remain — no winner yet"
    );
}

/// CR 103.8c multiplayer rule via the Commander ctor: the starting player
/// draws on turn 1 (no duel skip at 4 seats).
#[test]
fn planechase_commander_starting_player_draws_first_turn() {
    let mut state = GameState::new(FormatConfig::planechase_commander(), 4, 42);
    state.turn_number = 1;
    assert!(
        !should_skip_draw(&state),
        "CR 103.8c: 4-player Planechase Commander must not skip the starting player's first draw"
    );
}
