use std::collections::HashMap;

use crate::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PlayerDeckPool, WaitingFor};
use crate::types::match_config::{DeckCardCount, MatchPhase, MatchType};
use crate::types::player::PlayerId;

fn opponent(player: PlayerId) -> PlayerId {
    if player == PlayerId(0) {
        PlayerId(1)
    } else {
        PlayerId(0)
    }
}

fn total_count(entries: &[DeckEntry]) -> u32 {
    entries.iter().map(|e| e.count).sum()
}

fn to_count_map(cards: &[DeckCardCount]) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    for card in cards {
        if card.count > 0 {
            *map.entry(card.name.clone()).or_insert(0) += card.count;
        }
    }
    map
}

fn entries_to_count_map(entries: &[DeckEntry]) -> HashMap<String, u32> {
    let mut map = HashMap::new();
    for entry in entries {
        if entry.count > 0 {
            *map.entry(entry.card.name.clone()).or_insert(0) += entry.count;
        }
    }
    map
}

fn counts_to_entries(
    counts: &[DeckCardCount],
    card_faces: &HashMap<String, crate::types::card::CardFace>,
) -> Result<Vec<DeckEntry>, String> {
    let mut entries = Vec::new();
    for card in counts {
        if card.count == 0 {
            continue;
        }
        let face = card_faces
            .get(&card.name)
            .ok_or_else(|| format!("Unknown card in sideboard submission: {}", card.name))?;
        entries.push(DeckEntry {
            card: face.clone(),
            count: card.count,
        });
    }
    Ok(entries)
}

fn build_card_face_map(pool: &PlayerDeckPool) -> HashMap<String, crate::types::card::CardFace> {
    let mut faces = HashMap::new();
    for entry in pool
        .registered_main
        .iter()
        .chain(pool.registered_sideboard.iter())
        .chain(pool.registered_commander.iter())
    {
        faces
            .entry(entry.card.name.clone())
            .or_insert_with(|| entry.card.clone());
    }
    faces
}

fn deck_payload_from_current_pools(state: &GameState) -> Result<DeckPayload, String> {
    let p0 = state
        .deck_pools
        .iter()
        .find(|p| p.player == PlayerId(0))
        .ok_or_else(|| "Missing player 0 deck pool".to_string())?;
    let p1 = state
        .deck_pools
        .iter()
        .find(|p| p.player == PlayerId(1))
        .ok_or_else(|| "Missing player 1 deck pool".to_string())?;

    // `PlayerDeckPayload`'s deck fields are plain `Vec<DeckEntry>` — deref
    // the Arc then deep-clone so the payload owns its own vec.
    Ok(DeckPayload {
        player: PlayerDeckPayload {
            main_deck: (*p0.current_main).clone(),
            sideboard: (*p0.current_sideboard).clone(),
            commander: (*p0.current_commander).clone(),
        },
        opponent: PlayerDeckPayload {
            main_deck: (*p1.current_main).clone(),
            sideboard: (*p1.current_sideboard).clone(),
            commander: (*p1.current_commander).clone(),
        },
        ai_decks: vec![],
    })
}

pub fn handle_game_over_transition(state: &mut GameState) {
    if state.match_phase != MatchPhase::InGame {
        return;
    }

    let winner = match state.waiting_for {
        WaitingFor::GameOver { winner } => winner,
        _ => return,
    };

    if state.match_config.match_type != MatchType::Bo3 || state.players.len() != 2 {
        state.match_phase = MatchPhase::Completed;
        return;
    }

    match winner {
        Some(PlayerId(0)) => {
            state.match_score.p0_wins = state.match_score.p0_wins.saturating_add(1)
        }
        Some(PlayerId(1)) => {
            state.match_score.p1_wins = state.match_score.p1_wins.saturating_add(1)
        }
        Some(_) => {}
        None => state.match_score.draws = state.match_score.draws.saturating_add(1),
    }

    let match_complete = state.match_score.p0_wins >= 2 || state.match_score.p1_wins >= 2;
    if match_complete {
        state.match_phase = MatchPhase::Completed;
        return;
    }

    state.match_phase = MatchPhase::BetweenGames;
    state.game_number = state.game_number.saturating_add(1);
    state.sideboard_submitted.clear();
    state.next_game_chooser = match winner {
        Some(w) => Some(opponent(w)),
        None => state
            .next_game_chooser
            .or(Some(state.current_starting_player)),
    };
    state.waiting_for = WaitingFor::BetweenGamesSideboard {
        player: PlayerId(0),
        game_number: state.game_number,
        score: state.match_score,
    };
}

pub fn handle_submit_sideboard(
    state: &mut GameState,
    player: PlayerId,
    main: Vec<DeckCardCount>,
    sideboard: Vec<DeckCardCount>,
) -> Result<WaitingFor, String> {
    if state.match_phase != MatchPhase::BetweenGames {
        return Err("Cannot submit sideboard outside BetweenGames phase".to_string());
    }

    let Some(pool) = state.deck_pools.iter_mut().find(|p| p.player == player) else {
        return Err("Deck pool not found for player".to_string());
    };

    let submitted_main_total: u32 = main.iter().map(|c| c.count).sum();
    let registered_main_total = total_count(&pool.registered_main);
    if submitted_main_total != registered_main_total {
        return Err(format!(
            "Main deck size mismatch: expected {}, got {}",
            registered_main_total, submitted_main_total
        ));
    }

    let submitted_pool_map = {
        let mut map = to_count_map(&main);
        for (name, count) in to_count_map(&sideboard) {
            *map.entry(name).or_insert(0) += count;
        }
        map
    };
    let registered_pool_map = {
        let mut map = entries_to_count_map(&pool.registered_main);
        for (name, count) in entries_to_count_map(&pool.registered_sideboard) {
            *map.entry(name).or_insert(0) += count;
        }
        map
    };
    if submitted_pool_map != registered_pool_map {
        return Err("Submitted main+sideboard must match registered card pool".to_string());
    }

    let face_map = build_card_face_map(pool);
    pool.current_main = std::sync::Arc::new(counts_to_entries(&main, &face_map)?);
    pool.current_sideboard = std::sync::Arc::new(counts_to_entries(&sideboard, &face_map)?);

    if !state.sideboard_submitted.contains(&player) {
        state.sideboard_submitted.push(player);
    }

    let waiting_for = if state.sideboard_submitted.contains(&PlayerId(0))
        && state.sideboard_submitted.contains(&PlayerId(1))
    {
        let chooser = state.next_game_chooser.unwrap_or(PlayerId(0));
        WaitingFor::BetweenGamesChoosePlayDraw {
            player: chooser,
            game_number: state.game_number,
            score: state.match_score,
        }
    } else {
        WaitingFor::BetweenGamesSideboard {
            player: opponent(player),
            game_number: state.game_number,
            score: state.match_score,
        }
    };
    state.waiting_for = waiting_for.clone();
    Ok(waiting_for)
}

pub fn handle_choose_play_draw(
    state: &mut GameState,
    chooser: PlayerId,
    play_first: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
    if state.match_phase != MatchPhase::BetweenGames {
        return Err("Cannot choose play/draw outside BetweenGames phase".to_string());
    }
    let expected_chooser = state.next_game_chooser.unwrap_or(PlayerId(0));
    if chooser != expected_chooser {
        return Err("Only the designated chooser may choose play/draw".to_string());
    }

    let starting_player = if play_first {
        chooser
    } else {
        opponent(chooser)
    };
    let payload = deck_payload_from_current_pools(state)?;

    let mut next_state = GameState::new(
        state.format_config.clone(),
        state.players.len() as u8,
        state.rng_seed.wrapping_add(state.game_number as u64 + 1),
    );
    next_state.match_config = state.match_config;
    next_state.match_phase = MatchPhase::InGame;
    next_state.match_score = state.match_score;
    next_state.game_number = state.game_number;
    next_state.current_starting_player = starting_player;
    // If the game is drawn, this chooser gets to choose again.
    next_state.next_game_chooser = Some(chooser);

    load_deck_into_state(&mut next_state, &payload);
    let start = super::engine::start_game_with_starting_player(&mut next_state, starting_player);
    events.extend(start.events);

    let waiting_for = start.waiting_for.clone();
    *state = next_state;
    state.waiting_for = waiting_for.clone();
    Ok(waiting_for)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::deck_loading::PlayerDeckPayload;
    use crate::game::engine::{apply_as_current, start_game};
    use crate::types::actions::GameAction;
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};
    use crate::types::mana::ManaCost;

    fn basic_land(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Land],
                subtypes: vec!["Plains".to_string()],
            },
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: vec![],
            abilities: vec![],
            triggers: vec![],
            static_abilities: vec![],
            replacements: vec![],
            color_override: None,
            color_identity: vec![],
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            parse_warnings: vec![],
            brawl_commander: false,
            metadata: Default::default(),
            rarities: Default::default(),
        }
    }

    fn entry(name: &str, count: u32) -> DeckEntry {
        DeckEntry {
            card: basic_land(name),
            count,
        }
    }

    #[test]
    fn bo3_progression_reaches_match_completion() {
        let mut state = GameState::new_two_player(7);
        state.match_config.match_type = MatchType::Bo3;
        state.match_phase = MatchPhase::InGame;

        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        handle_game_over_transition(&mut state);
        assert_eq!(state.match_phase, MatchPhase::BetweenGames);
        assert_eq!(state.match_score.p0_wins, 1);
        assert_eq!(state.match_score.p1_wins, 0);
        assert_eq!(state.game_number, 2);
        assert_eq!(state.next_game_chooser, Some(PlayerId(1)));

        state.match_phase = MatchPhase::InGame;
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(1)),
        };
        handle_game_over_transition(&mut state);
        assert_eq!(state.match_phase, MatchPhase::BetweenGames);
        assert_eq!(state.match_score.p0_wins, 1);
        assert_eq!(state.match_score.p1_wins, 1);
        assert_eq!(state.game_number, 3);
        assert_eq!(state.next_game_chooser, Some(PlayerId(0)));

        state.match_phase = MatchPhase::InGame;
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        handle_game_over_transition(&mut state);
        assert_eq!(state.match_phase, MatchPhase::Completed);
        assert_eq!(state.match_score.p0_wins, 2);
        assert_eq!(state.match_score.p1_wins, 1);
    }

    #[test]
    fn draw_keeps_existing_chooser() {
        let mut state = GameState::new_two_player(9);
        state.match_config.match_type = MatchType::Bo3;
        state.match_phase = MatchPhase::InGame;
        state.next_game_chooser = Some(PlayerId(1));
        state.current_starting_player = PlayerId(0);
        state.waiting_for = WaitingFor::GameOver { winner: None };

        handle_game_over_transition(&mut state);

        assert_eq!(state.match_phase, MatchPhase::BetweenGames);
        assert_eq!(state.match_score.draws, 1);
        assert_eq!(state.next_game_chooser, Some(PlayerId(1)));
    }

    #[test]
    fn sideboard_validation_rejects_bad_submissions() {
        let mut state = GameState::new_two_player(3);
        state.match_phase = MatchPhase::BetweenGames;
        state.deck_pools = vec![PlayerDeckPool {
            player: PlayerId(0),
            registered_main: std::sync::Arc::new(vec![entry("A", 2)]),
            registered_sideboard: std::sync::Arc::new(vec![entry("B", 1)]),
            current_main: std::sync::Arc::new(vec![entry("A", 2)]),
            current_sideboard: std::sync::Arc::new(vec![entry("B", 1)]),
            ..Default::default()
        }];

        let bad_main_size = handle_submit_sideboard(
            &mut state,
            PlayerId(0),
            vec![DeckCardCount {
                name: "A".to_string(),
                count: 1,
            }],
            vec![DeckCardCount {
                name: "B".to_string(),
                count: 1,
            }],
        );
        assert!(bad_main_size.is_err());

        let bad_pool = handle_submit_sideboard(
            &mut state,
            PlayerId(0),
            vec![DeckCardCount {
                name: "A".to_string(),
                count: 2,
            }],
            vec![DeckCardCount {
                name: "C".to_string(),
                count: 1,
            }],
        );
        assert!(bad_pool.is_err());
    }

    #[test]
    fn bo3_game_one_starter_is_randomized() {
        let mut saw_p0 = false;
        let mut saw_p1 = false;

        for seed in 0..64u64 {
            let mut state = GameState::new_two_player(seed);
            state.match_config.match_type = MatchType::Bo3;
            state.game_number = 1;
            let _ = start_game(&mut state);
            if state.current_starting_player == PlayerId(0) {
                saw_p0 = true;
            }
            if state.current_starting_player == PlayerId(1) {
                saw_p1 = true;
            }
            if saw_p0 && saw_p1 {
                break;
            }
        }

        assert!(saw_p0 && saw_p1);
    }

    #[test]
    fn apply_between_games_actions_restarts_next_game() {
        let mut state = GameState::new_two_player(11);
        state.match_config.match_type = MatchType::Bo3;

        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![entry("P0", 7)],
                sideboard: vec![entry("P0SB", 1)],
                commander: vec![],
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![entry("P1", 7)],
                sideboard: vec![entry("P1SB", 1)],
                commander: vec![],
            },
            ai_decks: vec![],
        };
        load_deck_into_state(&mut state, &payload);
        let _ = start_game(&mut state);

        state.match_phase = MatchPhase::BetweenGames;
        state.match_score = crate::types::match_config::MatchScore {
            p0_wins: 1,
            p1_wins: 0,
            draws: 0,
        };
        state.game_number = 2;
        state.next_game_chooser = Some(PlayerId(1));
        state.sideboard_submitted.clear();
        state.waiting_for = WaitingFor::BetweenGamesSideboard {
            player: PlayerId(0),
            game_number: 2,
            score: state.match_score,
        };

        let submit_p0 = apply_as_current(
            &mut state,
            GameAction::SubmitSideboard {
                main: vec![DeckCardCount {
                    name: "P0".to_string(),
                    count: 7,
                }],
                sideboard: vec![DeckCardCount {
                    name: "P0SB".to_string(),
                    count: 1,
                }],
            },
        )
        .unwrap();
        assert!(matches!(
            submit_p0.waiting_for,
            WaitingFor::BetweenGamesSideboard {
                player: PlayerId(1),
                ..
            }
        ));

        let submit_p1 = apply_as_current(
            &mut state,
            GameAction::SubmitSideboard {
                main: vec![DeckCardCount {
                    name: "P1".to_string(),
                    count: 7,
                }],
                sideboard: vec![DeckCardCount {
                    name: "P1SB".to_string(),
                    count: 1,
                }],
            },
        )
        .unwrap();
        assert!(matches!(
            submit_p1.waiting_for,
            WaitingFor::BetweenGamesChoosePlayDraw {
                player: PlayerId(1),
                ..
            }
        ));

        let choose =
            apply_as_current(&mut state, GameAction::ChoosePlayDraw { play_first: true }).unwrap();

        assert_eq!(state.match_phase, MatchPhase::InGame);
        assert_eq!(state.match_score.p0_wins, 1);
        assert_eq!(state.game_number, 2);
        assert_eq!(state.current_starting_player, PlayerId(1));
        assert!(!state.players[0].hand.is_empty());
        assert!(!state.players[1].hand.is_empty());
        assert!(!matches!(choose.waiting_for, WaitingFor::GameOver { .. }));
    }
}
