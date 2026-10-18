//! A format's best-of-three ceiling is enforced where the game starts: a Bo3
//! config above the ceiling plays as Bo1 and never enters the between-games path.

use engine::game::deck_loading::{load_and_hydrate_decks, DeckPayload, MOMIR_SNOW_BASICS};
use engine::game::engine::{start_game, start_game_with_starting_player};
use engine::game::scenario::GameRunner;
use engine::types::actions::GameAction;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::format::FormatConfig;
use engine::types::game_state::{GameState, LoopDetectionMode, WaitingFor};
use engine::types::mana::ManaCost;
use engine::types::match_config::{DeckCardCount, MatchConfig, MatchPhase, MatchType};
use engine::types::player::PlayerId;

use crate::support::{install_synthetic_card_db, shared_card_db};

fn snow_basic_face(name: &str) -> CardFace {
    CardFace {
        name: name.to_string(),
        mana_cost: ManaCost::NoCost,
        card_type: CardType {
            supertypes: vec![Supertype::Basic, Supertype::Snow],
            core_types: vec![CoreType::Land],
            subtypes: vec![name.trim_start_matches("Snow-Covered ").to_string()],
        },
        ..Default::default()
    }
}

fn started(events: &[GameEvent]) -> bool {
    events.iter().any(|e| matches!(e, GameEvent::GameStarted))
}

fn concede_p1(runner: &mut GameRunner) {
    runner
        .act(GameAction::Concede {
            player_id: PlayerId(1),
        })
        .expect("conceding game one must be accepted");
}

fn dandan_bo3_runner() -> GameRunner {
    let db = shared_card_db().expect("integration card fixture is required");
    let mut state = GameState::new(FormatConfig::dandan(), 2, 7);
    state.match_config.match_type = MatchType::Bo3;
    load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(db));
    let result = start_game(&mut state);
    assert!(started(&result.events), "reach: the game started");
    GameRunner::from_state(state)
}

#[test]
fn r1_dandan_bo3_plays_as_bo1_and_never_enters_between_games() {
    let mut runner = dandan_bo3_runner();
    assert_eq!(runner.state().match_config.match_type, MatchType::Bo1);
    concede_p1(&mut runner);
    assert_eq!(runner.state().match_phase, MatchPhase::Completed);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::GameOver { .. }
    ));
}

#[test]
fn r2_momir_keeps_bo3_and_restarts_between_games() {
    let faces: Vec<CardFace> = MOMIR_SNOW_BASICS
        .iter()
        .map(|name| snow_basic_face(name))
        .collect();
    let mut state = GameState::new(FormatConfig::momir(), 2, 42);
    state.match_config.match_type = MatchType::Bo3;
    let db = install_synthetic_card_db(&mut state, &faces);
    load_and_hydrate_decks(&mut state, &DeckPayload::default(), Some(&db));
    let result = start_game(&mut state);
    assert!(started(&result.events));
    assert_eq!(state.match_config.match_type, MatchType::Bo3);

    let mut runner = GameRunner::from_state(state);
    concede_p1(&mut runner);
    let fixed_deck: Vec<DeckCardCount> = MOMIR_SNOW_BASICS
        .iter()
        .map(|name| DeckCardCount {
            name: name.to_string(),
            count: 12,
        })
        .collect();
    for _ in 0..2 {
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::BetweenGamesSideboard { .. }
        ));
        runner
            .act(GameAction::SubmitSideboard {
                main: fixed_deck.clone(),
                sideboard: vec![],
            })
            .expect("resubmitting the fixed Momir deck must be accepted");
    }
    runner
        .act(GameAction::ChoosePlayDraw { play_first: true })
        .expect("choosing play/draw starts game two");
    assert_eq!(runner.state().game_number, 2);
}

#[test]
fn r3_two_seat_standard_keeps_bo3() {
    let mut state = GameState::new_two_player(7);
    state.match_config.match_type = MatchType::Bo3;
    let result = start_game_with_starting_player(&mut state, PlayerId(0));
    assert!(started(&result.events));
    assert_eq!(state.match_config.match_type, MatchType::Bo3);
}

#[test]
fn r4_seat_count_term_and_archenemy_exemption_are_preserved() {
    let mut three = GameState::new(FormatConfig::commander(), 3, 7);
    three.match_config.match_type = MatchType::Bo3;
    let result = start_game_with_starting_player(&mut three, PlayerId(0));
    assert!(started(&result.events));
    assert_eq!(three.players.len(), 3);
    assert_eq!(three.match_config.match_type, MatchType::Bo1);

    let mut config = FormatConfig::archenemy();
    config.archenemy_player = Some(PlayerId(2));
    let mut arch = GameState::new(config, 4, 7);
    arch.match_config.match_type = MatchType::Bo3;
    let result = start_game_with_starting_player(&mut arch, PlayerId(2));
    assert!(started(&result.events));
    assert_eq!(arch.format_config.archenemy_player, Some(PlayerId(2)));
    assert_eq!(arch.match_config.match_type, MatchType::Bo3);
}

#[test]
fn r5_configured_bo1_stays_bo1_under_both_ceilings() {
    for config in [FormatConfig::dandan(), FormatConfig::standard()] {
        let mut state = GameState::new(config, 2, 7);
        state.match_config.match_type = MatchType::Bo1;
        let result = start_game_with_starting_player(&mut state, PlayerId(0));
        assert!(started(&result.events));
        assert_eq!(state.match_config.match_type, MatchType::Bo1);
    }
}

#[test]
fn r6_ceiling_comes_from_the_format_and_only_match_type_is_written() {
    let mut state = GameState::new(FormatConfig::dandan(), 2, 7);
    state.set_match_config(MatchConfig {
        match_type: MatchType::Bo3,
        loop_detection: LoopDetectionMode::Interactive,
    });
    let result = start_game_with_starting_player(&mut state, PlayerId(0));
    assert!(started(&result.events));
    assert_eq!(state.match_config.match_type, MatchType::Bo1);
    assert_eq!(
        state.match_config.loop_detection,
        LoopDetectionMode::Interactive
    );
    assert_eq!(state.loop_detection, LoopDetectionMode::Interactive);
}
