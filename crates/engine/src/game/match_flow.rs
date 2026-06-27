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

fn bo3_sideboard_players(state: &GameState) -> Vec<PlayerId> {
    if crate::game::topology::archenemy(state).is_some() {
        state.deck_pools.iter().map(|pool| pool.player).collect()
    } else {
        vec![PlayerId(0), PlayerId(1)]
    }
}

fn next_unsubmitted_sideboard_player(state: &GameState) -> Option<PlayerId> {
    bo3_sideboard_players(state)
        .into_iter()
        .find(|player| !state.sideboard_submitted.contains(player))
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
    // Propagate `bracket_tier` so the pool rebuilt by `load_deck_into_state`
    // in the next game carries the same declared tier as the current game.
    //
    // Seats >= 2 are AI players (e.g., cEDH 4-player Bo3). Collect their pools
    // so `bracket_tier = Cedh` is not silently dropped between games.
    let ai_decks = state
        .deck_pools
        .iter()
        .filter(|p| p.player != PlayerId(0) && p.player != PlayerId(1))
        .map(|p| PlayerDeckPayload {
            main_deck: (*p.current_main).clone(),
            sideboard: (*p.current_sideboard).clone(),
            commander: (*p.current_commander).clone(),
            attraction_deck: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: (*p.registered_scheme_deck).clone(),
            contraption_deck: Vec::new(),
            sticker_sheets: state
                .players
                .iter()
                .find(|player| player.id == p.player)
                .map(|player| player.sticker_sheets.clone())
                .unwrap_or_default(),
            signature_spell: (*p.current_signature_spell).clone(),
            bracket_tier: p.bracket_tier,
        })
        .collect();

    Ok(DeckPayload {
        player: PlayerDeckPayload {
            main_deck: (*p0.current_main).clone(),
            sideboard: (*p0.current_sideboard).clone(),
            commander: (*p0.current_commander).clone(),
            attraction_deck: Vec::new(),
            planar_deck: (*p0.registered_planar_deck).clone(),
            scheme_deck: (*p0.registered_scheme_deck).clone(),
            contraption_deck: Vec::new(),
            sticker_sheets: state.players[0].sticker_sheets.clone(),
            signature_spell: (*p0.current_signature_spell).clone(),
            bracket_tier: p0.bracket_tier,
        },
        opponent: PlayerDeckPayload {
            main_deck: (*p1.current_main).clone(),
            sideboard: (*p1.current_sideboard).clone(),
            commander: (*p1.current_commander).clone(),
            attraction_deck: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: (*p1.registered_scheme_deck).clone(),
            contraption_deck: Vec::new(),
            sticker_sheets: state.players[1].sticker_sheets.clone(),
            signature_spell: (*p1.current_signature_spell).clone(),
            bracket_tier: p1.bracket_tier,
        },
        ai_decks,
        // cEDH bracket validation ran at game 1 setup; decks haven't
        // changed between games, so re-validation is unnecessary.
        ai_difficulties: vec![],
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

    let archenemy = crate::game::topology::archenemy(state);
    if state.match_config.match_type != MatchType::Bo3
        || (state.players.len() != 2 && archenemy.is_none())
    {
        state.match_phase = MatchPhase::Completed;
        return;
    }

    if let Some(archenemy) = archenemy {
        match winner {
            Some(winner) if winner == archenemy => {
                state.match_score.p0_wins = state.match_score.p0_wins.saturating_add(1)
            }
            Some(_) => state.match_score.p1_wins = state.match_score.p1_wins.saturating_add(1),
            None => state.match_score.draws = state.match_score.draws.saturating_add(1),
        }
    } else {
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
    }

    let match_complete = state.match_score.p0_wins >= 2 || state.match_score.p1_wins >= 2;
    if match_complete {
        state.match_phase = MatchPhase::Completed;
        return;
    }

    state.match_phase = MatchPhase::BetweenGames;
    state.game_number = state.game_number.saturating_add(1);
    state.sideboard_submitted.clear();
    state.next_game_chooser = if let Some(archenemy) = archenemy {
        Some(archenemy)
    } else {
        match winner {
            Some(w) => Some(opponent(w)),
            None => state
                .next_game_chooser
                .or(Some(state.current_starting_player)),
        }
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
    events: &mut Vec<GameEvent>,
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

    let waiting_for = if next_unsubmitted_sideboard_player(state).is_none() {
        if let Some(archenemy) = crate::game::topology::archenemy(state) {
            return restart_between_games_with_starting_player(state, archenemy, archenemy, events);
        }
        let chooser = state.next_game_chooser.unwrap_or(PlayerId(0));
        WaitingFor::BetweenGamesChoosePlayDraw {
            player: chooser,
            game_number: state.game_number,
            score: state.match_score,
        }
    } else {
        WaitingFor::BetweenGamesSideboard {
            player: next_unsubmitted_sideboard_player(state).unwrap_or_else(|| opponent(player)),
            game_number: state.game_number,
            score: state.match_score,
        }
    };
    state.waiting_for = waiting_for.clone();
    Ok(waiting_for)
}

fn restart_between_games_with_starting_player(
    state: &mut GameState,
    chooser: PlayerId,
    starting_player: PlayerId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, String> {
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
    // If the game is drawn, this chooser gets to choose again. Archenemy fixes
    // the chooser/starter to the archenemy per CR 904.6.
    next_state.next_game_chooser = Some(chooser);

    load_deck_into_state(&mut next_state, &payload);
    let start = super::engine::start_game_with_starting_player(&mut next_state, starting_player);
    events.extend(start.events);

    let waiting_for = start.waiting_for.clone();
    *state = next_state;
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
    restart_between_games_with_starting_player(state, chooser, starting_player, events)
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
            cleave_variant: None,
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
            is_commander: false,
            is_oathbreaker: false,
            deck_copy_limit: None,
            metadata: Default::default(),
            rarities: Default::default(),
            attraction_lights: vec![],
        }
    }

    fn entry(name: &str, count: u32) -> DeckEntry {
        DeckEntry {
            card: basic_land(name),
            count,
        }
    }

    fn plane_entry(name: &str, count: u32) -> DeckEntry {
        let mut card = basic_land(name);
        card.card_type.core_types = vec![CoreType::Plane];
        card.card_type.subtypes = Vec::new();
        DeckEntry { card, count }
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

        let mut events = Vec::new();
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
            &mut events,
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
            &mut events,
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
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![entry("P1", 7)],
                sideboard: vec![entry("P1SB", 1)],
                commander: vec![],
                ..Default::default()
            },
            ..Default::default()
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
        state
            .outside_game_cards_brought_in
            .push(crate::types::game_state::OutsideGameCardUse {
                player: PlayerId(0),
                sideboard_index: 0,
                count: 1,
            });

        let choose =
            apply_as_current(&mut state, GameAction::ChoosePlayDraw { play_first: true }).unwrap();

        assert_eq!(state.match_phase, MatchPhase::InGame);
        assert_eq!(state.match_score.p0_wins, 1);
        assert_eq!(state.game_number, 2);
        assert_eq!(state.current_starting_player, PlayerId(1));
        assert!(state.outside_game_cards_brought_in.is_empty());
        assert!(!state.players[0].hand.is_empty());
        assert!(!state.players[1].hand.is_empty());
        assert!(!matches!(choose.waiting_for, WaitingFor::GameOver { .. }));
    }

    #[test]
    fn bo3_planechase_restart_preserves_custom_planar_deck() {
        let mut state = GameState::new(crate::types::format::FormatConfig::planechase(), 2, 13);
        state.match_config.match_type = MatchType::Bo3;

        let custom_planes = vec![
            plane_entry("Custom Plane Alpha", 1),
            plane_entry("Custom Plane Beta", 1),
        ];
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                main_deck: vec![entry("P0", 7)],
                planar_deck: custom_planes.clone(),
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                main_deck: vec![entry("P1", 7)],
                ..Default::default()
            },
            ..Default::default()
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

        apply_as_current(
            &mut state,
            GameAction::SubmitSideboard {
                main: vec![DeckCardCount {
                    name: "P0".to_string(),
                    count: 7,
                }],
                sideboard: vec![],
            },
        )
        .unwrap();
        apply_as_current(
            &mut state,
            GameAction::SubmitSideboard {
                main: vec![DeckCardCount {
                    name: "P1".to_string(),
                    count: 7,
                }],
                sideboard: vec![],
            },
        )
        .unwrap();
        apply_as_current(&mut state, GameAction::ChoosePlayDraw { play_first: true }).unwrap();

        let registered_planar_names: Vec<_> = state.deck_pools[0]
            .registered_planar_deck
            .iter()
            .map(|entry| entry.card.name.as_str())
            .collect();
        assert_eq!(
            registered_planar_names,
            vec!["Custom Plane Alpha", "Custom Plane Beta"]
        );

        let live_planar_names: std::collections::HashSet<_> = state
            .planar_deck
            .iter()
            .map(|id| state.objects[id].name.as_str())
            .collect();
        assert_eq!(
            live_planar_names,
            ["Custom Plane Alpha", "Custom Plane Beta"]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn deck_payload_from_current_pools_propagates_ai_seat_bracket_tier() {
        use crate::game::bracket_estimate::CommanderBracketTier;
        use crate::types::game_state::PlayerDeckPool;

        let mut state = GameState::new_two_player(42);

        // Seed deck pools for seats 0, 1, and 2 (AI seat).
        state.deck_pools = vec![
            PlayerDeckPool {
                player: PlayerId(0),
                bracket_tier: CommanderBracketTier::Core,
                current_main: std::sync::Arc::new(vec![entry("P0", 1)]),
                ..Default::default()
            },
            PlayerDeckPool {
                player: PlayerId(1),
                bracket_tier: CommanderBracketTier::Optimized,
                current_main: std::sync::Arc::new(vec![entry("P1", 1)]),
                ..Default::default()
            },
            PlayerDeckPool {
                player: PlayerId(2),
                bracket_tier: CommanderBracketTier::Cedh,
                current_main: std::sync::Arc::new(vec![entry("AI", 1)]),
                ..Default::default()
            },
        ];

        let payload = deck_payload_from_current_pools(&state)
            .expect("deck_payload_from_current_pools must succeed with three pools");

        assert_eq!(
            payload.player.bracket_tier,
            CommanderBracketTier::Core,
            "player seat bracket_tier must round-trip"
        );
        assert!(
            payload.player.planar_deck.is_empty(),
            "no custom Planechase payload should remain empty so the default planar deck path can inject defaults"
        );
        assert_eq!(
            payload.opponent.bracket_tier,
            CommanderBracketTier::Optimized,
            "opponent seat bracket_tier must round-trip"
        );
        assert_eq!(
            payload.ai_decks.len(),
            1,
            "AI seat at index >= 2 must be collected into ai_decks"
        );
        assert_eq!(
            payload.ai_decks[0].bracket_tier,
            CommanderBracketTier::Cedh,
            "AI seat bracket_tier (Cedh) must be propagated — not silently dropped"
        );
    }
}
