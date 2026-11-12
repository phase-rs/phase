//! CR 103.1 + CR 103.1a + CR 800.4h: the game-one starting-player prompt
//! (`WaitingFor::ChooseStartingPlayer`) and its answer
//! (`GameAction::ChooseStartingPlayer`).
//!
//! No production path emits the prompt yet, so every test seeds it on a turn-0
//! state and drives the production `apply` boundary from there: the answer
//! starts the game through `start_game_with_starting_player`, and a departure
//! during the prompt is reconciled by the elimination sweep.

use std::collections::HashSet;

use engine::ai_support::legal_actions;
use engine::game::apply;
use engine::game::deck_loading::create_object_from_card_face;
use engine::game::derived_views::{derive_views, PlayDrawPosition};
use engine::game::engine::EngineError;
use engine::game::match_flow::handle_game_over_transition;
use engine::game::zones::create_object;
use engine::types::actions::{GameAction, MulliganChoice};
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::CardId;
use engine::types::match_config::{MatchPhase, MatchType};
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
