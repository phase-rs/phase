//! CR 103.1 + CR 103.1a + CR 800.4h: the game-one starting-player prompt
//! (`WaitingFor::ChooseStartingPlayer`) and its answer
//! (`GameAction::ChooseStartingPlayer`).
//!
//! The handler rows seed the prompt on a turn-0 state and drive the production
//! `apply` boundary from there: the answer starts the game through
//! `start_game_with_starting_player`, and a departure during the prompt is
//! reconciled by the elimination sweep. The `start_game` rows enter through the
//! production emission (`engine::game::engine::start_game`), whose d20 contest
//! decides who chooses.

use std::collections::HashSet;

use engine::ai_support::legal_actions;
use engine::game::apply;
use engine::game::deck_loading::{
    create_object_from_card_face, load_and_hydrate_decks, load_deck_into_state, DeckEntry,
    DeckPayload, PlayerDeckPayload,
};
use engine::game::derived_views::{derive_views, PlayDrawPosition};
use engine::game::engine::{start_game, start_game_winner_plays_first, EngineError};
use engine::game::match_flow::handle_game_over_transition;
use engine::game::zones::create_object;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::custom_format::CustomFormatDef;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::match_config::{DeckCardCount, MatchPhase, MatchType};
use engine::types::phase::TurnDirection;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

/// 20 Forests in every seat's library, so the started game reaches the
/// opening-hand mulligan decision.
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

/// A turn-0 game with stocked libraries, paused at the CR 103.1 prompt.
fn seeded_prompt(
    format: FormatConfig,
    player_count: u8,
    chooser: PlayerId,
    candidates: Vec<PlayerId>,
) -> GameState {
    let mut state = GameState::new(format, player_count, 42);
    seed_forest_libraries(&mut state);
    state.waiting_for = WaitingFor::ChooseStartingPlayer {
        player: chooser,
        candidates,
    };
    state
}

fn every_seat(state: &GameState) -> Vec<PlayerId> {
    state.seat_order.clone()
}

fn choose(state: &mut GameState, actor: PlayerId, chosen: PlayerId) -> Result<(), EngineError> {
    apply(
        state,
        actor,
        GameAction::ChooseStartingPlayer { player: chosen },
    )
    .map(|_| ())
}

fn concede(state: &mut GameState, player: PlayerId) {
    apply(state, player, GameAction::Concede { player_id: player })
        .expect("CR 104.3a: a player may concede during the prompt");
}

fn assert_reached_opening_hands(state: &GameState) {
    assert_eq!(state.turn_number, 1, "the answer must start the game");
    assert!(
        matches!(state.waiting_for, WaitingFor::MulliganDecision { .. }),
        "the started game must reach the opening-hand mulligan decision, got {:?}",
        state.waiting_for
    );
}

#[test]
fn chooser_names_the_opponent_as_starting_player() {
    let mut state = seeded_prompt(FormatConfig::standard(), 2, P0, vec![P0, P1]);

    let result = apply(
        &mut state,
        P0,
        GameAction::ChooseStartingPlayer { player: P1 },
    )
    .expect("CR 103.1: the chooser may name the other player");

    assert_reached_opening_hands(&state);
    assert!(result.events.contains(&GameEvent::GameStarted));
    assert!(result.events.contains(&GameEvent::TurnStarted {
        player_id: P1,
        turn_number: 1,
    }));
    assert_eq!(state.current_starting_player, P1);
    assert_eq!(state.active_player, P1);
    assert_eq!(state.seat_order[0], P1);
    assert_eq!(
        state.next_game_chooser,
        Some(P0),
        "CR 103.1: the chooser, not the starter, is latched for a drawn game"
    );
}

#[test]
fn chooser_names_themselves() {
    let mut state = seeded_prompt(FormatConfig::standard(), 2, P0, vec![P0, P1]);

    choose(&mut state, P0, P0).expect("CR 103.1: the chooser may take the first turn");

    assert_reached_opening_hands(&state);
    assert_eq!(state.current_starting_player, P0);
    assert_eq!(state.next_game_chooser, Some(P0));
}

#[test]
fn non_chooser_answer_is_rejected() {
    let mut state = seeded_prompt(FormatConfig::standard(), 2, P0, vec![P0, P1]);
    let prompt = state.waiting_for.clone();

    let result = choose(&mut state, P1, P1);

    assert!(
        matches!(result, Err(EngineError::WrongPlayer)),
        "CR 103.1: only the chooser answers, got {result:?}"
    );
    assert_eq!(state.waiting_for, prompt);
    assert_eq!(
        state.turn_number, 0,
        "the rejected answer must not start the game"
    );
    assert_eq!(state.next_game_chooser, None);
}

#[test]
fn non_candidate_answer_is_rejected() {
    // P1 is alive and seated, so only `candidates` membership can reject it.
    let mut state = seeded_prompt(FormatConfig::free_for_all(), 3, P0, vec![P0, P2]);
    let prompt = state.waiting_for.clone();

    let result = choose(&mut state, P0, P1);

    assert!(
        matches!(result, Err(EngineError::InvalidAction(_))),
        "a seat outside the candidates is not a legal answer, got {result:?}"
    );
    assert_eq!(state.waiting_for, prompt);
    assert_eq!(
        state.turn_number, 0,
        "the rejected answer must not start the game"
    );

    choose(&mut state, P0, P2).expect("a listed candidate is accepted in the same layout");
    assert_eq!(state.current_starting_player, P2);
}

#[test]
fn four_player_chooser_names_any_seat() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
    seed_forest_libraries(&mut state);
    state.waiting_for = WaitingFor::ChooseStartingPlayer {
        player: P1,
        candidates: every_seat(&state),
    };

    choose(&mut state, P1, P3).expect("CR 103.1: any candidate seat may be named");

    assert_reached_opening_hands(&state);
    assert_eq!(state.seat_order, vec![P3, P0, P1, P2]);
    assert_eq!(state.current_starting_player, P3);
    assert_eq!(state.next_game_chooser, Some(P1));
}

#[test]
fn two_headed_giant_choice_starts_that_team() {
    let mut state = seeded_prompt(
        FormatConfig::two_headed_giant(),
        4,
        P0,
        vec![P0, P1, P2, P3],
    );

    choose(&mut state, P0, P2).expect("CR 103.1a: the chooser names the starting team's seat");

    assert_eq!(state.current_starting_player, P2);
    assert_eq!(
        derive_views(&state, Some(P3)).viewer_play_draw,
        Some(PlayDrawPosition::OnThePlay),
        "CR 103.1a: the chosen seat's teammate is on the starting team"
    );
    assert_eq!(
        derive_views(&state, Some(P0)).viewer_play_draw,
        Some(PlayDrawPosition::OnTheDraw),
        "CR 103.1a: the chooser's team is on the draw"
    );
}

#[test]
fn departed_chooser_passes_the_choice() {
    for (direction, next_chooser) in [(TurnDirection::Normal, P1), (TurnDirection::Reversed, P2)] {
        let mut state = seeded_prompt(FormatConfig::free_for_all(), 3, P0, vec![P0, P1, P2]);
        state.turn_direction = direction;

        concede(&mut state, P0);

        assert_eq!(
            state.waiting_for,
            WaitingFor::ChooseStartingPlayer {
                player: next_chooser,
                candidates: vec![P1, P2],
            },
            "CR 800.4h: the prompt survives and passes to the next player in turn order \
             ({direction:?})"
        );

        choose(&mut state, next_chooser, P2).expect("the new chooser answers the prompt");
        assert_eq!(state.current_starting_player, P2);
        assert_eq!(state.next_game_chooser, Some(next_chooser));
    }
}

#[test]
fn departed_candidate_leaves_the_choice() {
    let mut state = seeded_prompt(FormatConfig::free_for_all(), 3, P0, vec![P0, P1, P2]);

    concede(&mut state, P2);

    assert!(
        state.seat_order.contains(&P2),
        "the departed seat stays in seat order, so only the prune removes it"
    );
    assert_eq!(
        state.waiting_for,
        WaitingFor::ChooseStartingPlayer {
            player: P0,
            candidates: vec![P0, P1],
        },
        "CR 800.4a: a departed seat is no longer a candidate"
    );
    let result = choose(&mut state, P0, P2);
    assert!(
        matches!(result, Err(EngineError::InvalidAction(_))),
        "a departed seat cannot take the first turn, got {result:?}"
    );

    choose(&mut state, P0, P1).expect("a remaining candidate is accepted");
    assert_eq!(state.current_starting_player, P1);
}

#[test]
fn drawn_game_keeps_the_game_one_chooser() {
    let mut state = seeded_prompt(FormatConfig::standard(), 2, P0, vec![P0, P1]);
    state.match_config.match_type = MatchType::Bo3;

    choose(&mut state, P0, P1).expect("CR 103.1: the chooser names the other player");
    assert_eq!(state.current_starting_player, P1);

    // The game is drawn: the match-flow transition `draw_keeps_existing_chooser`
    // drives, entered from the game-one state the answer produced.
    state.waiting_for = WaitingFor::GameOver { winner: None };
    handle_game_over_transition(&mut state);

    assert_eq!(state.match_phase, MatchPhase::BetweenGames);
    assert_eq!(state.match_score.draws, 1);
    assert!(
        matches!(state.waiting_for, WaitingFor::BetweenGamesSideboard { .. }),
        "the drawn-game branch must run, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.next_game_chooser,
        Some(P0),
        "CR 103.1: after a draw the player who made the choice chooses again"
    );
}

#[test]
fn ai_legal_actions_cover_every_candidate() {
    let expected = |seats: &[PlayerId]| -> HashSet<String> {
        seats
            .iter()
            .map(|seat| format!("{:?}", GameAction::ChooseStartingPlayer { player: *seat }))
            .collect()
    };
    let actual = |state: &GameState| -> HashSet<String> {
        legal_actions(state)
            .iter()
            .map(|action| format!("{action:?}"))
            .collect()
    };

    let mut state = seeded_prompt(FormatConfig::free_for_all(), 3, P0, vec![P0, P1, P2]);
    assert_eq!(legal_actions(&state).len(), 3, "one answer per candidate");
    assert_eq!(actual(&state), expected(&[P0, P1, P2]));

    concede(&mut state, P2);
    assert_eq!(actual(&state), expected(&[P0, P1]));
}

/// Gemstone Caverns: "If this card is in your opening hand and you're not the
/// starting player, you may begin the game with Gemstone Caverns on the
/// battlefield with a luck counter on it. If you do, exile a card from your
/// hand."
#[test]
fn chooser_on_the_draw_is_offered_gemstone_caverns() {
    let Some(db) = shared_card_db() else {
        return;
    };
    let gemstone = db
        .get_face_by_name("Gemstone Caverns")
        .expect("Gemstone Caverns must be in the card database");
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest must be in the card database");

    for (chosen, chooser_is_offered) in [(P1, true), (P0, false)] {
        // Seven-card libraries, so each library becomes its owner's opening hand.
        let mut state = GameState::new_two_player(42);
        create_object_from_card_face(&mut state, gemstone, P0);
        for _ in 0..6 {
            create_object_from_card_face(&mut state, forest, P0);
        }
        for _ in 0..7 {
            create_object_from_card_face(&mut state, forest, P1);
        }
        state.waiting_for = WaitingFor::ChooseStartingPlayer {
            player: P0,
            candidates: vec![P0, P1],
        };

        choose(&mut state, P0, chosen).expect("CR 103.1: the chooser names the starter");
        while let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for {
            let player = pending.first().expect("a pending keeper").player;
            apply(
                &mut state,
                player,
                GameAction::MulliganDecision {
                    choice: MulliganChoice::Keep,
                },
            )
            .expect("keeping an opening hand succeeds");
        }

        if chooser_is_offered {
            assert!(
                matches!(
                    state.waiting_for,
                    WaitingFor::OptionalEffectChoice { player: P0, .. }
                ),
                "CR 103.6a: the chooser on the draw is offered Gemstone Caverns, got {:?}",
                state.waiting_for
            );
        } else {
            assert!(
                matches!(state.waiting_for, WaitingFor::Priority { .. }),
                "the chooser who starts is not offered Gemstone Caverns, got {:?}",
                state.waiting_for
            );
        }
    }
}

// --- Rows reached through `start_game` ------------------------------------

/// The contest winner in a start batch, if the batch ran the contest.
fn contest_winner(events: &[GameEvent]) -> Option<PlayerId> {
    events.iter().find_map(|event| match event {
        GameEvent::StartingPlayerContest { winner, .. } => Some(*winner),
        _ => None,
    })
}

/// The first seed whose `start_game` contest winner satisfies `wanted`.
fn seed_whose_winner(
    new_state: impl Fn(u64) -> GameState,
    wanted: impl Fn(PlayerId) -> bool,
) -> u64 {
    (0..256u64)
        .find(|&seed| {
            let mut state = new_state(seed);
            contest_winner(&start_game(&mut state).events).is_some_and(&wanted)
        })
        .expect("a seed with the wanted contest winner within 256 seeds")
}

/// The chooser the `start_game` prompt names.
fn prompted_chooser(state: &GameState) -> PlayerId {
    let WaitingFor::ChooseStartingPlayer { player, .. } = state.waiting_for else {
        panic!(
            "the start rests at the CR 103.1 choice, got {:?}",
            state.waiting_for
        );
    };
    player
}

/// The other seat of a two-player game.
fn other(player: PlayerId) -> PlayerId {
    PlayerId(1 - player.0)
}

/// Keeps every pending opening hand.
fn keep_all_hands(state: &mut GameState) {
    while let WaitingFor::MulliganDecision { pending, .. } = &state.waiting_for {
        let player = pending.first().expect("a pending keeper").player;
        apply(
            state,
            player,
            GameAction::MulliganDecision {
                choice: MulliganChoice::Keep,
            },
        )
        .expect("keeping an opening hand succeeds");
    }
}

/// CR 103.1: `start_game` ends at the prompt, and the roll winner chooses.
#[test]
fn start_game_asks_the_roll_winner_who_plays_first() {
    let seed = seed_whose_winner(GameState::new_two_player, |winner| winner == P1);
    let mut state = GameState::new_two_player(seed);

    let result = start_game(&mut state);

    assert!(
        matches!(
            result.events.as_slice(),
            [GameEvent::StartingPlayerContest { winner: P1, .. }]
        ),
        "the start batch is the contest alone, won by P1: {:?}",
        result.events
    );
    assert_eq!(
        state.waiting_for,
        WaitingFor::ChooseStartingPlayer {
            player: P1,
            candidates: vec![P0, P1],
        },
        "CR 103.1: the roll winner chooses who takes the first turn"
    );
    assert_eq!(state.turn_number, 0, "no turn has begun before the choice");
}

/// CR 103.1: no play/draw position and no turn order exist before the choice.
#[test]
fn no_position_is_published_before_the_choice() {
    let mut duel = GameState::new_two_player(42);
    start_game(&mut duel);
    for seat in [P0, P1] {
        assert_eq!(derive_views(&duel, Some(seat)).viewer_play_draw, None);
    }
    let chooser = prompted_chooser(&duel);
    choose(&mut duel, chooser, chooser).expect("the chooser elects to play first");
    for seat in [P0, P1] {
        assert!(
            derive_views(&duel, Some(seat)).viewer_play_draw.is_some(),
            "after the answer the position is published"
        );
    }

    let mut table = GameState::new(FormatConfig::free_for_all(), 4, 42);
    start_game(&mut table);
    let views = derive_views(&table, Some(P0));
    assert!(views.turn_order.is_empty());
    assert_eq!(views.viewer_turn_number, None);
    let chooser = prompted_chooser(&table);
    choose(&mut table, chooser, chooser).expect("the chooser elects to play first");
    let views = derive_views(&table, Some(P0));
    assert!(
        !views.turn_order.is_empty(),
        "after the answer the turn order is published"
    );
    assert!(views.viewer_turn_number.is_some());
}

/// CR 103.1: the roll winner may put the opponent on the play; positions
/// follow the starter, and the chooser is latched for a drawn game.
#[test]
fn roll_winner_who_chooses_to_draw_is_on_the_draw() {
    let mut state = GameState::new_two_player(42);
    seed_forest_libraries(&mut state);
    let start = start_game(&mut state);
    let winner = prompted_chooser(&state);
    let starter = other(winner);

    let answer = apply(
        &mut state,
        winner,
        GameAction::ChooseStartingPlayer { player: starter },
    )
    .expect("CR 103.1: the chooser may name the other player");

    assert_reached_opening_hands(&state);
    assert_eq!(state.current_starting_player, starter);
    assert_eq!(state.active_player, starter);
    assert_eq!(state.seat_order[0], starter);
    assert_eq!(
        derive_views(&state, Some(winner)).viewer_play_draw,
        Some(PlayDrawPosition::OnTheDraw)
    );
    assert_eq!(
        derive_views(&state, Some(starter)).viewer_play_draw,
        Some(PlayDrawPosition::OnThePlay)
    );
    assert_eq!(state.next_game_chooser, Some(winner));
    assert_eq!(contest_winner(&start.events), Some(winner));
    assert_eq!(
        contest_winner(&answer.events),
        None,
        "the answer's batch runs no contest"
    );
}

/// CR 103.1: at a real prompt only the chooser answers, and only with a
/// candidate.
#[test]
fn real_prompt_refuses_the_wrong_actor_and_a_non_candidate() {
    let mut state = GameState::new_two_player(42);
    start_game(&mut state);
    let prompt = state.waiting_for.clone();
    let chooser = prompted_chooser(&state);

    let wrong_actor = choose(&mut state, other(chooser), chooser);
    assert!(
        matches!(wrong_actor, Err(EngineError::WrongPlayer)),
        "only the chooser answers, got {wrong_actor:?}"
    );
    assert_eq!(state.waiting_for, prompt);
    assert_eq!(state.turn_number, 0);

    let non_candidate = choose(&mut state, chooser, PlayerId(5));
    assert!(
        matches!(non_candidate, Err(EngineError::InvalidAction(_))),
        "a seat outside the candidates is refused, got {non_candidate:?}"
    );
    assert_eq!(state.waiting_for, prompt);
    assert_eq!(state.turn_number, 0);

    choose(&mut state, chooser, chooser).expect("the chooser's valid answer is accepted");
    assert_eq!(state.turn_number, 1);
}

/// CR 103.1: in a free-for-all game every seat is a candidate in seat order,
/// and the chooser may name any of them.
#[test]
fn free_for_all_chooser_may_name_any_seat() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 4, 42);
    seed_forest_libraries(&mut state);
    let start = start_game(&mut state);
    let winner = contest_winner(&start.events).expect("the contest ran");
    let WaitingFor::ChooseStartingPlayer { player, candidates } = state.waiting_for.clone() else {
        panic!("the start rests at the CR 103.1 choice");
    };
    assert_eq!(candidates, vec![P0, P1, P2, P3]);
    assert_eq!(player, winner);
    let named = [P1, P2, P3]
        .into_iter()
        .find(|&seat| seat != winner)
        .expect("a seat that is neither the chooser nor seat 0");

    choose(&mut state, winner, named).expect("CR 103.1: any candidate seat may be named");

    assert_reached_opening_hands(&state);
    assert_eq!(state.seat_order[0], named);
    assert_eq!(state.current_starting_player, named);
}

/// CR 103.1a + CR 805.3: under shared team turns the candidates and the
/// chooser are side representatives, so the choice names a starting team.
#[test]
fn two_headed_giant_choice_is_between_team_representatives() {
    let new_state = |seed| GameState::new(FormatConfig::two_headed_giant(), 4, seed);
    let seed = seed_whose_winner(new_state, |winner| winner == P1 || winner == P3);
    let mut state = new_state(seed);

    let start = start_game(&mut state);

    let winner = contest_winner(&start.events).expect("the contest ran");
    assert!(
        winner == P1 || winner == P3,
        "reach: a team's second seat won the roll"
    );
    let representative = if winner == P1 { P0 } else { P2 };
    assert_eq!(
        state.waiting_for,
        WaitingFor::ChooseStartingPlayer {
            player: representative,
            candidates: vec![P0, P2],
        },
        "CR 103.1a: the winner's team chooses between the starting teams"
    );
}

/// CR 800.4h: a chooser who leaves during a real prompt passes the choice to
/// the next player in turn order.
#[test]
fn departure_during_the_real_prompt_passes_the_choice_on() {
    let mut state = GameState::new(FormatConfig::free_for_all(), 3, 42);
    seed_forest_libraries(&mut state);
    start_game(&mut state);
    let chooser = prompted_chooser(&state);
    let seat_order = state.seat_order.clone();
    let position = seat_order
        .iter()
        .position(|&seat| seat == chooser)
        .expect("the chooser is seated");
    let next = seat_order[(position + 1) % seat_order.len()];
    let living: Vec<PlayerId> = seat_order
        .iter()
        .copied()
        .filter(|&seat| seat != chooser)
        .collect();

    concede(&mut state, chooser);

    assert!(
        state.seat_order.contains(&chooser),
        "the departed seat stays in seat order"
    );
    assert_eq!(
        state.waiting_for,
        WaitingFor::ChooseStartingPlayer {
            player: next,
            candidates: living,
        },
        "CR 800.4h: the prompt survives and passes to the next player in turn order"
    );
    choose(&mut state, next, next).expect("the new chooser answers");
    assert_reached_opening_hands(&state);
}

/// A custom four-seat format with two teams on shared team turns (CR 805).
/// Unlike Two-Headed Giant (CR 810.8b) its teams share no life, so a member
/// who concedes leaves alone (CR 104.3a) and the teammate plays on.
fn shared_turn_teams() -> FormatConfig {
    let mut source = FormatConfig::free_for_all();
    source.team_based = true;
    source.min_players = 4;
    source.max_players = 4;
    let def = CustomFormatDef::from_lobby_config("Shared-turn teams".to_string(), &source)
        .expect("a team-based free-for-all is saveable as a custom format");
    FormatConfig::for_custom_rules(&def.rules)
}

/// CR 103.1a + CR 800.4a: a side whose representative concedes during the real
/// prompt keeps its living member as its candidate, so either team can still
/// be named the starting team.
#[test]
fn shared_turn_team_keeps_a_candidate_when_its_representative_concedes() {
    let new_state = |seed| {
        let mut state = GameState::new(shared_turn_teams(), 4, seed);
        seed_forest_libraries(&mut state);
        state
    };
    let seed = seed_whose_winner(new_state, |winner| winner == P0 || winner == P1);
    let mut state = new_state(seed);
    start_game(&mut state);
    assert_eq!(
        state.waiting_for,
        WaitingFor::ChooseStartingPlayer {
            player: P0,
            candidates: vec![P0, P2],
        },
        "reach: the real prompt offers one representative per team"
    );

    concede(&mut state, P2);

    assert!(
        state.players[P2.0 as usize].is_eliminated && !state.players[P3.0 as usize].is_eliminated,
        "reach: the representative left alone; its teammate plays on"
    );
    assert_eq!(
        state.waiting_for,
        WaitingFor::ChooseStartingPlayer {
            player: P0,
            candidates: vec![P0, P3],
        },
        "CR 103.1a: the team whose representative left is still a starting-team candidate"
    );
    choose(&mut state, P0, P3).expect("the chooser names the remaining team");
    assert_reached_opening_hands(&state);
    assert_eq!(state.current_starting_player, P3);
}

/// CR 800.4h: the next player in turn order inherits a departed chooser's
/// choice even when it is not its team's representative; the prompt still
/// issues its own team's representative as an answer.
#[test]
fn inherited_choice_issues_the_new_choosers_team() {
    let mut state = seeded_prompt(shared_turn_teams(), 4, P2, vec![P0, P2]);
    state.turn_direction = TurnDirection::Reversed;

    concede(&mut state, P2);

    assert_eq!(
        state.waiting_for,
        WaitingFor::ChooseStartingPlayer {
            player: P1,
            candidates: vec![P0, P3],
        },
        "CR 800.4h: P1 inherits the choice; each team keeps one candidate"
    );
    let issued = legal_actions(&state);
    assert!(
        issued.contains(&GameAction::ChooseStartingPlayer { player: P0 }),
        "the chooser's own team is an issued answer, got {issued:?}"
    );
    assert!(
        !issued.contains(&GameAction::ChooseStartingPlayer { player: P1 }),
        "the chooser's own seat is not its team's representative, got {issued:?}"
    );
    choose(&mut state, P1, P0).expect("the new chooser names its own team");
    assert_eq!(state.current_starting_player, P0);
    assert_eq!(state.next_game_chooser, Some(P1));
}

/// CR 103.1: "If the previous game was a draw, the player who made the choice
/// in that game makes the choice in this game." The game-one chooser who put
/// the opponent on the play chooses again after a drawn game one.
#[test]
fn drawn_game_one_returns_the_choice_to_its_chooser() {
    let db = shared_card_db().expect("integration card fixture is required");
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest must be in the card database");
    let deck = || PlayerDeckPayload {
        main_deck: vec![DeckEntry::from_resolved_face(db, forest, 20)],
        ..Default::default()
    };
    let mut state = GameState::new_two_player(42);
    state.match_config.match_type = MatchType::Bo3;
    load_deck_into_state(
        &mut state,
        &DeckPayload {
            player: deck(),
            opponent: deck(),
            ..Default::default()
        },
    );
    start_game(&mut state);
    let winner = prompted_chooser(&state);
    choose(&mut state, winner, other(winner)).expect("the chooser names the other player");
    assert_eq!(state.current_starting_player, other(winner));

    state.waiting_for = WaitingFor::GameOver { winner: None };
    handle_game_over_transition(&mut state);
    assert_eq!(state.match_score.draws, 1);
    for submission in 0..2 {
        let WaitingFor::BetweenGamesSideboard { player, .. } = state.waiting_for else {
            panic!(
                "submission {submission} waits on a sideboard, got {:?}",
                state.waiting_for
            );
        };
        apply(
            &mut state,
            player,
            GameAction::SubmitSideboard {
                main: vec![DeckCardCount {
                    name: "Forest".to_string(),
                    count: 20,
                }],
                sideboard: vec![],
            },
        )
        .expect("resubmitting the registered deck is accepted");
    }

    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::BetweenGamesChoosePlayDraw { player, .. } if player == winner
        ),
        "CR 103.1: the game-one chooser chooses after the draw, got {:?}",
        state.waiting_for
    );
}

/// Gemstone Caverns: "If this card is in your opening hand and you're not the
/// starting player, you may begin the game with Gemstone Caverns on the
/// battlefield with a luck counter on it. If you do, exile a card from your
/// hand." Both seats hold it; the starting player the roll winner names, not
/// the roll, decides who is offered it (CR 103.6a).
fn gemstone_offer_after_the_roll_winner_names(starter_is_winner: bool) -> (PlayerId, WaitingFor) {
    let db = shared_card_db().expect("integration card fixture is required");
    let gemstone = db
        .get_face_by_name("Gemstone Caverns")
        .expect("Gemstone Caverns must be in the card database");
    let forest = db
        .get_face_by_name("Forest")
        .expect("Forest must be in the card database");
    // Seven-card libraries, so each library becomes its owner's opening hand.
    let mut state = GameState::new_two_player(42);
    for seat in [P0, P1] {
        create_object_from_card_face(&mut state, gemstone, seat);
        for _ in 0..6 {
            create_object_from_card_face(&mut state, forest, seat);
        }
    }
    start_game(&mut state);
    let winner = prompted_chooser(&state);
    let starter = if starter_is_winner {
        winner
    } else {
        other(winner)
    };
    choose(&mut state, winner, starter).expect("the chooser names the starter");
    keep_all_hands(&mut state);
    (winner, state.waiting_for)
}

#[test]
fn gemstone_caverns_is_offered_to_the_roll_winner_who_draws() {
    let (winner, waiting_for) = gemstone_offer_after_the_roll_winner_names(false);
    assert!(
        matches!(
            waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == winner
        ),
        "CR 103.6a: the roll winner on the draw is offered Gemstone Caverns, got {waiting_for:?}"
    );
}

#[test]
fn gemstone_caverns_skips_the_roll_winner_who_plays() {
    let (winner, waiting_for) = gemstone_offer_after_the_roll_winner_names(true);
    assert!(
        matches!(
            waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == other(winner)
        ),
        "CR 103.6a: the opponent on the draw is offered Gemstone Caverns, got {waiting_for:?}"
    );
}

/// The match-type ceiling applies before the prompt, so a concession during
/// the choice ends a Dandan match (ceiling Bo1) and continues a Momir match
/// (ceiling Bo3).
#[test]
fn ceiling_holds_for_a_concession_during_the_choice() {
    let db = shared_card_db().expect("integration card fixture is required");
    for (format, expected_match_type) in [
        (FormatConfig::dandan(), MatchType::Bo1),
        (FormatConfig::momir(), MatchType::Bo3),
    ] {
        let mut state = GameState::new(format, 2, 7);
        state.match_config.match_type = MatchType::Bo3;
        load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(db));
        start_game(&mut state);
        prompted_chooser(&state);
        assert_eq!(state.match_config.match_type, expected_match_type);

        concede(&mut state, P1);

        match expected_match_type {
            MatchType::Bo1 => {
                assert_eq!(state.match_phase, MatchPhase::Completed);
                assert!(
                    matches!(state.waiting_for, WaitingFor::GameOver { .. }),
                    "a Bo1 concession ends the match, got {:?}",
                    state.waiting_for
                );
            }
            MatchType::Bo3 => assert!(
                matches!(state.waiting_for, WaitingFor::BetweenGamesSideboard { .. }),
                "a Bo3 concession continues the match, got {:?}",
                state.waiting_for
            ),
        }
    }
}

/// CR 103.1 with the contest winner electing to play: the legacy authority
/// reproduces the pre-choice start, and Archenemy passes through.
#[test]
fn winner_plays_first_reproduces_the_pre_choice_start() {
    let new_state = |seed| {
        let mut state = GameState::new_two_player(seed);
        seed_forest_libraries(&mut state);
        state
    };
    let seed = seed_whose_winner(new_state, |winner| winner == P1);

    let mut prompted = new_state(seed);
    start_game(&mut prompted);
    assert_eq!(
        prompted_chooser(&prompted),
        P1,
        "the same seed through `start_game` waits on P1's choice"
    );

    let mut state = new_state(seed);
    let result = start_game_winner_plays_first(&mut state);
    assert!(
        matches!(
            result.events.as_slice(),
            [
                GameEvent::StartingPlayerContest { winner: P1, .. },
                GameEvent::GameStarted,
                GameEvent::TurnStarted { turn_number: 1, .. },
                ..
            ]
        ),
        "the pre-choice event sequence: {:?}",
        result.events
    );
    assert_eq!(state.current_starting_player, P1);
    assert_eq!(state.next_game_chooser, Some(P1));
    assert!(matches!(
        state.waiting_for,
        WaitingFor::MulliganDecision { .. }
    ));

    // CR 103.1b + CR 904.6: the archenemy takes the first turn, with no contest.
    let mut config = FormatConfig::archenemy();
    config.archenemy_player = Some(P2);
    let mut archenemy = GameState::new(config, 4, 7);
    let result = start_game_winner_plays_first(&mut archenemy);
    assert_eq!(contest_winner(&result.events), None);
    assert_eq!(archenemy.current_starting_player, P2);
}

/// The legacy authority starts the raw roll winner under shared team turns,
/// even a team's second seat, as the pre-choice start did.
#[test]
fn winner_plays_first_starts_the_raw_two_headed_giant_winner() {
    let new_state = |seed| GameState::new(FormatConfig::two_headed_giant(), 4, seed);
    let seed = seed_whose_winner(new_state, |winner| winner == P1 || winner == P3);

    let mut prompted = new_state(seed);
    let start = start_game(&mut prompted);
    let winner = contest_winner(&start.events).expect("the contest ran");
    let representative = if winner == P1 { P0 } else { P2 };
    assert_eq!(
        prompted_chooser(&prompted),
        representative,
        "the same seed through `start_game` prompts the team's first seat"
    );

    let mut state = new_state(seed);
    start_game_winner_plays_first(&mut state);
    assert_eq!(state.current_starting_player, winner);
    assert_eq!(state.seat_order[0], winner);
    assert_eq!(state.next_game_chooser, Some(winner));
}
