//! End-to-end composition probes for the two Commander team formats:
//! Two-Headed Giant Commander (CR 810 x CR 903) and Archenemy Commander
//! (CR 904.13). The format-type level (constructors, validation, family
//! predicates) composes; these tests prove the RUNTIME composition —
//! starting life splits, team-loss cascades from per-head commander damage
//! and poison — actually holds through the SBA/elimination pipeline.

use engine::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use engine::game::sba::check_state_based_actions;
use engine::game::turns::{advance_phase, start_next_turn};
use engine::types::card::CardFace;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CommanderDamageEntry, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

// 2HG seating: P0+P1 are one team, P2+P3 the other.
const TEAM_A: [PlayerId; 2] = [PlayerId(0), PlayerId(1)];
const TEAM_B: [PlayerId; 2] = [PlayerId(2), PlayerId(3)];
const ARCHENEMY: PlayerId = PlayerId(0);
const HEROES: [PlayerId; 3] = [PlayerId(1), PlayerId(2), PlayerId(3)];

fn life_of(state: &GameState, player: PlayerId) -> i32 {
    state.players.iter().find(|p| p.id == player).unwrap().life
}

fn team_life(state: &GameState, team: &[PlayerId]) -> i32 {
    team.iter().map(|p| life_of(state, *p)).sum()
}

fn game_winner(state: &GameState) -> Option<PlayerId> {
    match &state.waiting_for {
        WaitingFor::GameOver { winner } => *winner,
        _ => None,
    }
}

/// Played consensus (MTGO/paper 2HG Commander): each team's shared total
/// starts at 60, split 30/30 across the two heads.
#[test]
fn thgc_shared_life_starts_at_sixty_per_team() {
    let state = GameState::new(FormatConfig::two_headed_giant_commander(), 4, 7);
    for seat in 0..4 {
        assert_eq!(
            life_of(&state, PlayerId(seat)),
            30,
            "seat {seat} starts at half the shared 60"
        );
    }
    assert_eq!(team_life(&state, &TEAM_A), 60);
    assert_eq!(team_life(&state, &TEAM_B), 60);
}

/// CR 903.10a per-head commander damage meets CR 810.8a team loss: the dealt
/// head loses at 21 and its team loses with it.
#[test]
fn thgc_commander_damage_cascades_to_team_loss() {
    let mut state = GameState::new(FormatConfig::two_headed_giant_commander(), 4, 7);
    state.commander_damage.push(CommanderDamageEntry {
        player: PlayerId(0),
        commander: ObjectId(999),
        damage: 21,
    });
    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(state.players[0].is_eliminated, "dealt head is eliminated");
    assert!(
        state.players[1].is_eliminated,
        "teammate departs with the head (CR 810.8a)"
    );
    assert!(!state.players[2].is_eliminated);
    assert!(!state.players[3].is_eliminated);
    let winner = game_winner(&state).expect("team B wins the game");
    assert!(
        TEAM_B.contains(&winner),
        "winner {winner:?} is on the surviving team"
    );
}

/// CR 904.13b: the archenemy starts at 60; the heroes split their shared 60
/// (20/20/20 at four seats). The archenemy takes the first turn (CR 904.6).
#[test]
fn archc_starting_life_split_and_first_turn() {
    let state = GameState::new(FormatConfig::archenemy_commander(), 4, 7);
    assert_eq!(life_of(&state, ARCHENEMY), 60);
    for hero in HEROES {
        assert_eq!(life_of(&state, hero), 20, "{hero:?} holds a third of 60");
    }
    assert_eq!(team_life(&state, &HEROES), 60);
    assert_eq!(
        state.active_player, ARCHENEMY,
        "the archenemy takes the first turn"
    );
}

/// CR 903.10a-via-904.13b: a hero reaching 21 commander damage loses and the
/// heroes team loses with them — the archenemy wins.
#[test]
fn archc_hero_commander_damage_cascades_to_heroes_loss() {
    let mut state = GameState::new(FormatConfig::archenemy_commander(), 4, 7);
    state.commander_damage.push(CommanderDamageEntry {
        player: PlayerId(1),
        commander: ObjectId(999),
        damage: 21,
    });
    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    for hero in HEROES {
        assert!(
            state.players[hero.0 as usize].is_eliminated,
            "{hero:?} departs with the heroes team"
        );
    }
    assert!(!state.players[0].is_eliminated);
    assert_eq!(game_winner(&state), Some(ARCHENEMY));
}

/// CR 904.13c: poison is individual (10+ loses) even though life is shared —
/// but the loss still cascades to the heroes team via shared life.
#[test]
fn archc_hero_poison_cascades_to_heroes_loss() {
    let mut state = GameState::new(FormatConfig::archenemy_commander(), 4, 7);
    state.players[1].poison_counters = 10;
    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    for hero in HEROES {
        assert!(
            state.players[hero.0 as usize].is_eliminated,
            "{hero:?} departs with the heroes team"
        );
    }
    assert_eq!(game_winner(&state), Some(ARCHENEMY));
}

/// The archenemy losing (21 commander damage) ends the game with a hero win —
/// no cascade the other way.
#[test]
fn archc_archenemy_commander_damage_ends_with_hero_win() {
    let mut state = GameState::new(FormatConfig::archenemy_commander(), 4, 7);
    state.commander_damage.push(CommanderDamageEntry {
        player: ARCHENEMY,
        commander: ObjectId(999),
        damage: 21,
    });
    let mut events = Vec::new();
    check_state_based_actions(&mut state, &mut events);

    assert!(state.players[0].is_eliminated);
    let winner = game_winner(&state).expect("a hero wins the game");
    assert!(
        HEROES.contains(&winner),
        "winner {winner:?} is a surviving hero"
    );
}

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

fn commanders_of(state: &GameState, owner: PlayerId) -> Vec<String> {
    state
        .command_zone
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.is_commander && o.owner == owner)
        .map(|o| o.name.clone())
        .collect()
}

/// CR 903.6: every 2HG Commander head seats its own decklist commander —
/// four seats, four commanders, each owned by its seat.
#[test]
fn thgc_load_places_all_four_commanders() {
    let mut state = GameState::new(FormatConfig::two_headed_giant_commander(), 4, 7);
    let payload = DeckPayload {
        player: edh_seat("General A0"),
        opponent: edh_seat("General A1"),
        ai_decks: vec![edh_seat("General B0"), edh_seat("General B1")],
        ai_difficulties: Vec::new(),
        booster_pack_pool: None,
    };
    load_deck_into_state(&mut state, &payload);

    for (seat, name) in ["General A0", "General A1", "General B0", "General B1"]
        .into_iter()
        .enumerate()
    {
        let placed = commanders_of(&state, PlayerId(seat as u8));
        assert_eq!(
            placed,
            vec![name.to_string()],
            "seat {seat} seats exactly its own commander"
        );
    }
}

/// CR 903.6 + CR 904.3/904.13d: every Archenemy Commander seat (archenemy
/// included) seats its commander, and the archenemy seat's scheme-deck slot
/// loads as the shared scheme deck.
#[test]
fn archc_load_places_commanders_and_scheme_deck() {
    let mut state = GameState::new(FormatConfig::archenemy_commander(), 4, 7);
    let mut archenemy_seat = edh_seat("Villain");
    archenemy_seat.scheme_deck = vec![card_entry("Scheme of Ruin", 10)];
    let payload = DeckPayload {
        player: archenemy_seat,
        opponent: edh_seat("Hero One"),
        ai_decks: vec![edh_seat("Hero Two"), edh_seat("Hero Three")],
        ai_difficulties: Vec::new(),
        booster_pack_pool: None,
    };
    load_deck_into_state(&mut state, &payload);

    for (seat, name) in ["Villain", "Hero One", "Hero Two", "Hero Three"]
        .into_iter()
        .enumerate()
    {
        let placed = commanders_of(&state, PlayerId(seat as u8));
        assert_eq!(
            placed,
            vec![name.to_string()],
            "seat {seat} seats exactly its own commander"
        );
    }
    assert_eq!(
        state.scheme_deck.len(),
        10,
        "the archenemy's scheme slot loads as the shared scheme deck"
    );
}

/// CR 810.4: the two heads share each team turn, so consecutive turns
/// alternate TEAMS (P0+P1, then P2+P3), never heads within a team.
#[test]
fn thgc_turns_alternate_between_teams() {
    let mut state = GameState::new(FormatConfig::two_headed_giant_commander(), 4, 7);
    let team_of = |p: PlayerId| TEAM_A.contains(&p) as u8;

    let mut actives = vec![state.active_player];
    let mut events = Vec::new();
    for _ in 0..4 {
        start_next_turn(&mut state, &mut events);
        actives.push(state.active_player);
    }
    for pair in actives.windows(2) {
        assert_ne!(
            team_of(pair[0]),
            team_of(pair[1]),
            "consecutive turns alternate teams, got {actives:?}"
        );
    }
}

/// CR 904.9 in a Commander game: the archenemy's precombat main sets the top
/// scheme of the loaded scheme deck in motion.
#[test]
fn archc_precombat_main_sets_scheme_in_motion() {
    let mut state = GameState::new(FormatConfig::archenemy_commander(), 4, 7);
    let mut archenemy_seat = edh_seat("Villain");
    archenemy_seat.scheme_deck = vec![card_entry("Scheme of Ruin", 10)];
    load_deck_into_state(
        &mut state,
        &DeckPayload {
            player: archenemy_seat,
            opponent: edh_seat("Hero One"),
            ai_decks: vec![edh_seat("Hero Two"), edh_seat("Hero Three")],
            ai_difficulties: Vec::new(),
            booster_pack_pool: None,
        },
    );
    assert_eq!(state.scheme_deck.len(), 10);

    state.active_player = ARCHENEMY;
    state.phase = Phase::Draw;
    let mut events = Vec::new();
    advance_phase(&mut state, &mut events);
    assert_eq!(state.phase, Phase::PreCombatMain);

    assert_eq!(
        state.scheme_deck.len(),
        9,
        "precombat main sets one scheme in motion"
    );
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::SchemeSetInMotion {
                player_id,
                ..
            } if *player_id == ARCHENEMY
        )),
        "SchemeSetInMotion emitted for the archenemy, got {events:?}"
    );
}
