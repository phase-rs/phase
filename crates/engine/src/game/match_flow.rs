use std::collections::HashMap;

use crate::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use crate::types::events::GameEvent;
use crate::types::format::SideboardPolicy;
use crate::types::game_state::{GameState, PlayerDeckPool, WaitingFor};
use crate::types::match_config::{
    DeckCardCount, MatchForfeitCause, MatchForfeitResult, MatchPhase, MatchType,
};
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

/// Restores the single companion promoted from a normal-format sideboard to
/// the editable current partition before a BetweenGames sideboard submission.
/// Registered sideboards remain immutable: legal post-board swaps are kept in
/// `current_main`/`current_sideboard` and only the revealed companion copy is
/// returned (CR 400.11a).
fn restore_revealed_sideboard_companions(state: &mut GameState) {
    if state.format_config.uses_commander {
        return;
    }

    for pool in &mut state.deck_pools {
        let Some(companion) = state
            .players
            .iter()
            .find(|player| player.id == pool.player)
            .and_then(|player| player.companion.as_ref())
        else {
            continue;
        };
        let name = &companion.card.card.name;
        if !pool
            .registered_sideboard
            .iter()
            .any(|entry| entry.card.name == *name)
        {
            continue;
        }
        let sideboard = std::sync::Arc::make_mut(&mut pool.current_sideboard);
        if let Some(entry) = sideboard.iter_mut().find(|entry| entry.card.name == *name) {
            entry.count += 1;
        } else {
            sideboard.push(companion.card.clone());
        }
    }
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
        .chain(pool.registered_companion.iter())
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
            // Dedicated companions are rebuilt from their registered external
            // slot, never from the consumed current offer.
            companion: (*p.registered_companion).clone(),
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
            companion: (*p0.registered_companion).clone(),
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
            companion: (*p1.registered_companion).clone(),
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
    restore_revealed_sideboard_companions(state);
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
    state.waiting_for = between_games_sideboard_prompt(state, PlayerId(0));
}

/// Completes an unfinished two-seat best-of-three through a transport-trusted
/// match forfeit. This is intentionally not a `GameAction`: callers must bind
/// the forfeiting seat to authenticated transport identity before selecting the
/// closed cause.
pub fn apply_trusted_match_forfeit(
    state: &mut GameState,
    forfeiting_player: PlayerId,
    cause: MatchForfeitCause,
) -> Result<Vec<GameEvent>, String> {
    if state.match_config.match_type != MatchType::Bo3 {
        return Err("Match forfeits require a best-of-three match".to_string());
    }
    if state.players.len() != 2 {
        return Err("Match forfeits require exactly two players".to_string());
    }
    if state.match_phase == MatchPhase::Completed {
        return Err("Match is already complete".to_string());
    }
    let winner = match forfeiting_player {
        PlayerId(0) => PlayerId(1),
        PlayerId(1) => PlayerId(0),
        _ => return Err("Forfeiting player is not a match seat".to_string()),
    };

    // A match forfeit ends the match rather than manufacturing a current-game
    // elimination. Retain already-earned games and make the winner's clinch
    // explicit in the frozen score used by terminal presentation.
    if winner == PlayerId(0) {
        state.match_score.p0_wins = state.match_score.p0_wins.max(2);
    } else {
        state.match_score.p1_wins = state.match_score.p1_wins.max(2);
    }
    state.match_phase = MatchPhase::Completed;
    state.match_forfeit_result = Some(MatchForfeitResult {
        winner,
        forfeiting_player,
        cause,
    });
    state.sideboard_submitted.clear();
    state.next_game_chooser = None;
    state.waiting_for = WaitingFor::GameOver {
        winner: Some(winner),
    };

    Ok(vec![GameEvent::GameOver {
        winner: Some(winner),
    }])
}

/// CR 100.2a / CR 100.4a / CR 100.5: the size bounds a between-games
/// submission must satisfy for `player`.
///
/// Single authority for the sideboarding gate: `handle_submit_sideboard`
/// validates against it, and `WaitingFor::BetweenGamesSideboard` publishes it
/// so the client's submit button enforces the engine's own predicate rather
/// than a reimplementation of it.
///
/// Returns `(min_main_deck_size, max_sideboard_size)`; a `None` sideboard cap
/// means the format imposes none.
pub(crate) fn sideboard_submission_bounds(
    state: &GameState,
    player: PlayerId,
) -> (u32, Option<u32>) {
    // CR 100.2a / CR 100.2b: `deck_size` is a *minimum* deck size, and CR 100.5
    // adds that there is no maximum deck size for non-Commander decks.
    // Sideboarding is therefore not a one-for-one swap: a player who registered
    // 60/15 may legally present 61, 70, or all 75 cards in their main deck. The
    // registered total bounds the *pool* (checked separately), never the
    // main-deck size.
    //
    // Clamping to the registered total keeps the floor satisfiable: a match
    // whose deck was registered below the format minimum (scenario decks, and
    // any deck the session admitted without a legality gate) would otherwise
    // have no legal submission at all, softlocking the between-games step. The
    // clamp still forbids shrinking the deck below what was already in play,
    // which is the property the minimum exists to protect.
    let registered_main_total = state
        .deck_pools
        .iter()
        .find(|p| p.player == player)
        .map_or(0, |pool| total_count(&pool.registered_main));
    let min_main_deck_size = u32::from(state.format_config.deck_size).min(registered_main_total);

    // CR 100.4a: the sideboard cap is per-format. `Forbidden` formats (the
    // Commander family) have no sideboard at all, which bounds it at zero and
    // therefore pins the whole pool in the main deck.
    let max_sideboard_size = match state.format_config.sideboard_policy {
        SideboardPolicy::Forbidden => Some(0),
        SideboardPolicy::Limited(max) => Some(max),
        SideboardPolicy::Unlimited => None,
    };

    (min_main_deck_size, max_sideboard_size)
}

/// Build the between-games prompt for `player`, stamping the submission bounds
/// the client gates on.
fn between_games_sideboard_prompt(state: &GameState, player: PlayerId) -> WaitingFor {
    let (min_main_deck_size, max_sideboard_size) = sideboard_submission_bounds(state, player);
    WaitingFor::BetweenGamesSideboard {
        player,
        game_number: state.game_number,
        score: state.match_score,
        min_main_deck_size,
        max_sideboard_size,
    }
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

    // Resolve the bounds before borrowing `state.deck_pools` mutably below.
    let (min_main_deck_size, max_sideboard_size) = sideboard_submission_bounds(state, player);

    let Some(pool) = state.deck_pools.iter_mut().find(|p| p.player == player) else {
        return Err("Deck pool not found for player".to_string());
    };

    let submitted_main_total: u32 = main.iter().map(|c| c.count).sum();
    if submitted_main_total < min_main_deck_size {
        return Err(format!(
            "Main deck has {submitted_main_total} cards (minimum {min_main_deck_size})"
        ));
    }
    let submitted_sideboard_total: u32 = sideboard.iter().map(|c| c.count).sum();
    if let Some(max) = max_sideboard_size {
        if submitted_sideboard_total > max {
            return Err(format!(
                "Sideboard has {submitted_sideboard_total} cards (maximum {max})"
            ));
        }
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
        between_games_sideboard_prompt(
            state,
            next_unsubmitted_sideboard_player(state).unwrap_or_else(|| opponent(player)),
        )
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
    // CR 732.2a: the between-games rebuild is a fresh `GameState::new` (loop_detection
    // defaults Off), so adopt the match config through the single authority — a raw
    // `next_state.match_config = …` would copy the struct but leave the runtime
    // `loop_detection` flag at the default, silently dropping the opt-in for game 2/3 of
    // a Bo3 (and archenemy restarts). `set_match_config` projects it, keeping the
    // detector setting consistent and immutable across every game of the match (#4603).
    next_state.set_match_config(state.match_config);
    next_state.match_phase = MatchPhase::InGame;
    next_state.match_score = state.match_score;
    next_state.game_number = state.game_number;
    next_state.current_starting_player = starting_player;
    // If the game is drawn, this chooser gets to choose again. Archenemy fixes
    // the chooser/starter to the archenemy per CR 904.6.
    next_state.next_game_chooser = Some(chooser);
    // Debug capability is a property of the match, not of a single game: it is
    // derived once (from the sandbox format flag, or from a single-user server
    // instance) and must survive this rebuild exactly as `match_config` does.
    // `GameState::new` defaults these to false/empty, so without an explicit
    // carry the sandbox panel silently stops working from game 2 onward.
    // Revocations are preserved because this is a continuation of the same
    // match — unlike `GameSession::rebuild_pregame_state`, which resets them
    // because it builds a fresh pregame context.
    //
    // A verbatim carry, not a re-derivation: this is engine state continuity
    // and must stay ignorant of the server's deployment shape. Re-seeding from
    // `format_config.allow_debug_actions` here would work for sandbox games and
    // would silently break desktop solo, whose flag is false.
    //
    // No CR annotation: debug capability implements no game rule (see
    // `visibility.rs` — "CR is silent; this is an out-of-game capability").
    // In particular this is NOT covered by the CR 732.2a note above.
    next_state.debug_mode = state.debug_mode;
    next_state.debug_permitted = state.debug_permitted.clone();

    // Interaction authority is likewise a property of the match, not of a single
    // game, and has the identical failure shape: `GameState::new` leaves
    // `interaction_session_id` as `None`, and while it is unset
    // `derive_viewer_interaction` yields no opportunities at all — so without this
    // carry every interaction surface silently goes dark from game 2 onward.
    //
    // Captured before the rebuild overwrites `state`, and re-applied after
    // `waiting_for` is final (below) rather than copied field-by-field, so the
    // slots the engine binds match the new game's pause.
    let interaction_session = state.interaction_session_id.clone();

    load_deck_into_state(&mut next_state, &payload);
    let start = super::engine::start_game_with_starting_player(&mut next_state, starting_player);
    events.extend(start.events);

    let waiting_for = start.waiting_for.clone();
    *state = next_state;
    state.waiting_for = waiting_for.clone();
    if let Some(session) = interaction_session {
        // Same `debug_assert` discipline as `ensure_interaction_authority`: the only
        // failure is decimal-serial exhaustion, which must not fail a live match.
        let bound = super::interaction::bind_interaction_authority(state, session);
        debug_assert!(bound.is_ok(), "between-games interaction rebind failed");
    }
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
    fn trusted_match_concede_completes_bo3_without_entering_sideboarding() {
        let mut state = GameState::new_two_player(7);
        state.match_config.match_type = MatchType::Bo3;
        state.match_score.p0_wins = 1;
        state.sideboard_submitted = vec![PlayerId(0)];

        let events =
            apply_trusted_match_forfeit(&mut state, PlayerId(0), MatchForfeitCause::MatchConcede)
                .expect("two-seat Bo3 match concede is trusted");

        assert_eq!(state.match_phase, MatchPhase::Completed);
        assert_eq!(state.match_score.p0_wins, 1);
        assert_eq!(state.match_score.p1_wins, 2);
        assert!(state.sideboard_submitted.is_empty());
        assert_eq!(
            state.waiting_for,
            WaitingFor::GameOver {
                winner: Some(PlayerId(1))
            }
        );
        assert_eq!(
            state.match_forfeit_result,
            Some(MatchForfeitResult {
                winner: PlayerId(1),
                forfeiting_player: PlayerId(0),
                cause: MatchForfeitCause::MatchConcede,
            })
        );
        assert_eq!(
            events,
            vec![GameEvent::GameOver {
                winner: Some(PlayerId(1))
            }]
        );
    }

    #[test]
    fn trusted_match_forfeit_rejects_non_bo3_and_completed_matches_without_mutation() {
        let mut state = GameState::new_two_player(7);
        let before = state.clone();
        assert!(apply_trusted_match_forfeit(
            &mut state,
            PlayerId(0),
            MatchForfeitCause::MatchConcede,
        )
        .is_err());
        assert_eq!(state, before);

        state.match_config.match_type = MatchType::Bo3;
        state.match_phase = MatchPhase::Completed;
        let before = state.clone();
        assert!(apply_trusted_match_forfeit(
            &mut state,
            PlayerId(0),
            MatchForfeitCause::MatchConcede,
        )
        .is_err());
        assert_eq!(state, before);
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
        // CR 100.2a: shrinking the main deck below the minimum is illegal. The
        // minimum here is the registered 2 (clamped down from Standard's 60),
        // so dropping to 1 main card must be rejected. The pool is kept intact
        // so this isolates the minimum check from the pool-equality check.
        let below_minimum = handle_submit_sideboard(
            &mut state,
            PlayerId(0),
            vec![DeckCardCount {
                name: "A".to_string(),
                count: 1,
            }],
            vec![
                DeckCardCount {
                    name: "A".to_string(),
                    count: 1,
                },
                DeckCardCount {
                    name: "B".to_string(),
                    count: 1,
                },
            ],
            &mut events,
        );
        assert_eq!(
            below_minimum,
            Err("Main deck has 1 cards (minimum 2)".to_string())
        );

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

    /// CR 100.2a + CR 100.5: `deck_size` is a minimum, not an exact size, so a
    /// player may side a card *in* without siding one out and submit a larger
    /// main deck than they registered. This is the case the old exact-equality
    /// check rejected.
    #[test]
    fn sideboard_accepts_main_deck_larger_than_registered() {
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
        let accepted = handle_submit_sideboard(
            &mut state,
            PlayerId(0),
            vec![
                DeckCardCount {
                    name: "A".to_string(),
                    count: 2,
                },
                DeckCardCount {
                    name: "B".to_string(),
                    count: 1,
                },
            ],
            Vec::new(),
            &mut events,
        );
        assert!(accepted.is_ok(), "{accepted:?}");

        let pool = &state.deck_pools[0];
        assert_eq!(total_count(&pool.current_main), 3);
        assert!(pool.current_sideboard.is_empty());
    }

    /// CR 100.4a: the sideboard may not exceed the format's cap. Standard is
    /// `Limited(15)`, so a 16-card sideboard is rejected even though the
    /// combined pool matches what was registered.
    #[test]
    fn sideboard_rejects_submission_over_the_format_cap() {
        let mut state = GameState::new_two_player(3);
        state.match_phase = MatchPhase::BetweenGames;
        state.deck_pools = vec![PlayerDeckPool {
            player: PlayerId(0),
            registered_main: std::sync::Arc::new(vec![entry("A", 60), entry("B", 16)]),
            registered_sideboard: std::sync::Arc::new(vec![]),
            current_main: std::sync::Arc::new(vec![entry("A", 60), entry("B", 16)]),
            current_sideboard: std::sync::Arc::new(vec![]),
            ..Default::default()
        }];

        let mut events = Vec::new();
        let over_cap = handle_submit_sideboard(
            &mut state,
            PlayerId(0),
            vec![DeckCardCount {
                name: "A".to_string(),
                count: 60,
            }],
            vec![DeckCardCount {
                name: "B".to_string(),
                count: 16,
            }],
            &mut events,
        );
        assert!(over_cap.is_err());
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
            min_main_deck_size: 0,
            max_sideboard_size: None,
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

    /// CR 732.2a opt-in persistence across the ENGINE between-games rebuild. A Bo3 match
    /// created with the detector On (projected onto `loop_detection` by `set_match_config`)
    /// must KEEP it On after `restart_between_games_with_starting_player` builds a fresh
    /// `GameState::new` for game 2. This guards the engine `match_flow` rebuild — distinct
    /// from the server-core `rebuild_pregame_state` path — which a raw `match_config = …`
    /// assignment silently drops, because a fresh `GameState::new` defaults the runtime
    /// `loop_detection` flag to Off (#4603 opt-in/immutability invariant).
    ///
    /// REVERT-FAIL: change the rebuild back to `next_state.match_config = state.match_config;`
    /// ⇒ `next_state.loop_detection` stays at the `GameState::new` default Off and the
    /// post-restart `On` assertion fails (the opt-in vanishes for game 2/3 of the match).
    #[test]
    fn bo3_restart_preserves_loop_detection_opt_in() {
        use crate::types::game_state::LoopDetectionMode;
        use crate::types::match_config::MatchConfig;

        let mut state = GameState::new_two_player(17);
        state.set_match_config(MatchConfig {
            match_type: MatchType::Bo3,
            loop_detection: LoopDetectionMode::On,
        });
        // Creation-time projection holds for game 1.
        assert_eq!(state.loop_detection, LoopDetectionMode::On);

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

        // Drive to the between-games rebuild for game 2.
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
            min_main_deck_size: 0,
            max_sideboard_size: None,
        };

        apply_as_current(
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
        apply_as_current(
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
        apply_as_current(&mut state, GameAction::ChoosePlayDraw { play_first: true }).unwrap();

        // Game 2 is live again...
        assert_eq!(state.match_phase, MatchPhase::InGame);
        assert_eq!(state.game_number, 2);
        // ...and the detector opt-in survived the fresh-state rebuild.
        assert_eq!(
            state.loop_detection,
            LoopDetectionMode::On,
            "detector opt-in must persist across the engine between-games rebuild"
        );
        assert_eq!(state.match_config.loop_detection, LoopDetectionMode::On);
    }

    /// Debug capability must survive the ENGINE between-games rebuild, exactly
    /// as the `loop_detection` opt-in above does. Without the carry, a sandbox
    /// or desktop-solo match gets a working debug panel for game 1 and a
    /// silently dead one from game 2 onward: `GameState::new` defaults
    /// `debug_mode` to false and `debug_permitted` to empty, and this rebuild
    /// never touches `GameSession`, so no server-side seeding authority can
    /// reach it.
    ///
    /// Mode-agnostic by construction (it drives the engine directly), which is
    /// why the two-line carry repairs desktop solo, browser solo, and sandbox
    /// multiplayer at once.
    ///
    /// REVERT-FAIL: remove either carry ⇒ that field reverts to its
    /// `GameState::new` default and its assertion below fails.
    #[test]
    fn bo3_restart_preserves_debug_capability() {
        let mut state = GameState::new_two_player(19);
        state.match_config.match_type = MatchType::Bo3;

        // Seat 0 only — the desktop-solo shape. Seat 1 must stay absent, which
        // is what proves the carry is a copy and not a re-seed of all seats.
        state.debug_mode = true;
        state.debug_permitted.insert(PlayerId(0));
        // The capability did NOT come from the sandbox format flag: this is
        // the desktop-solo case, where the flag is false and only a verbatim
        // carry can preserve it.
        assert!(!state.format_config.allow_debug_actions);

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
            min_main_deck_size: 0,
            max_sideboard_size: None,
        };

        apply_as_current(
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
        apply_as_current(
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
        apply_as_current(&mut state, GameAction::ChoosePlayDraw { play_first: true }).unwrap();

        // Sibling probe: if these fail, the test drove the wrong path and the
        // capability assertions below would be meaningless.
        assert_eq!(state.match_phase, MatchPhase::InGame);
        assert_eq!(state.game_number, 2);
        assert_eq!(state.match_score.p0_wins, 1);

        // Asserted AFTER `*state = next_state`, against a state this test did
        // not construct.
        assert!(
            state.debug_mode,
            "debug_mode must survive the between-games rebuild"
        );
        assert!(state.debug_permitted.contains(&PlayerId(0)));
        assert!(
            !state.debug_permitted.contains(&PlayerId(1)),
            "the carry is a copy, not a re-seed of every seat"
        );
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
            min_main_deck_size: 0,
            max_sideboard_size: None,
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

    /// Game 2 of a Bo3 is a fresh `GameState::new`, which defaults
    /// `interaction_session_id` to `None`. Without an explicit carry, every
    /// interaction surface reports `AuthorityUnbound` from game 2 onward — the
    /// same silent-from-game-2 failure the `debug_mode` carry above exists to
    /// prevent.
    #[test]
    fn between_games_restart_carries_interaction_authority() {
        use crate::game::interaction::bind_interaction_authority;
        use crate::types::game_state::PlayerDeckPool;
        use crate::types::interaction::InteractionSessionId;

        let mut state = GameState::new_two_player(21);
        state.match_config.match_type = MatchType::Bo3;
        state.match_phase = MatchPhase::BetweenGames;
        state.next_game_chooser = Some(PlayerId(0));
        state.deck_pools = vec![
            PlayerDeckPool {
                player: PlayerId(0),
                current_main: std::sync::Arc::new(vec![entry("P0", 40)]),
                ..Default::default()
            },
            PlayerDeckPool {
                player: PlayerId(1),
                current_main: std::sync::Arc::new(vec![entry("P1", 40)]),
                ..Default::default()
            },
        ];

        let session = InteractionSessionId("match-authority-carry".to_string());
        bind_interaction_authority(&mut state, session.clone())
            .expect("game 1 binds authority the way its creator does");

        let mut events = Vec::new();
        handle_choose_play_draw(&mut state, PlayerId(0), true, &mut events)
            .expect("game 2 of the Bo3 must start");

        // The rebuild replaces `state` wholesale with a `GameState::new`, so if
        // that constructor ever started binding authority itself the assertion
        // below would pass without the carry existing at all.
        assert_eq!(
            GameState::new(state.format_config.clone(), 2, 1).interaction_session_id,
            None,
            "probe is vacuous: a freshly constructed state is already bound, so \
             the carry is no longer what makes the next assertion hold"
        );

        assert_eq!(
            state.interaction_session_id.as_ref(),
            Some(&session),
            "the rebuilt game must keep the match's session; `GameState::new` \
             leaves it None, so this is None without the carry"
        );

        // The carry re-binds rather than copying the old slots, so the slots must
        // describe game 2's pause — stale game-1 slots would authorize decisions
        // that no longer exist.
        let acting = state.waiting_for.acting_players();
        assert!(
            !acting.is_empty(),
            "fixture must land on a pause with an acting player, or the slot \
             assertions below prove nothing"
        );
        for owner in acting {
            assert!(
                state
                    .active_interaction_slots
                    .iter()
                    .any(|slot| slot.semantic_owner == owner.0),
                "no slot bound for {owner:?}, who is acting in game 2"
            );
        }
    }

    #[test]
    fn choose_play_draw_logs_new_game_context_not_the_previous_game() {
        use crate::types::game_state::PlayerDeckPool;
        use crate::types::phase::Phase;

        let mut state = GameState::new_two_player(21);
        state.match_config.match_type = MatchType::Bo3;
        state.match_phase = MatchPhase::BetweenGames;
        state.game_number = 2;
        state.next_game_chooser = Some(PlayerId(0));
        // This is the action boundary snapshot consumed by the log resolver.
        // A restart must not stamp its GameStarted or TurnStarted entries with it.
        state.turn_number = 73;
        state.phase = Phase::End;
        state.deck_pools = vec![
            PlayerDeckPool {
                player: PlayerId(0),
                current_main: std::sync::Arc::new(vec![entry("P0", 40)]),
                ..Default::default()
            },
            PlayerDeckPool {
                player: PlayerId(1),
                current_main: std::sync::Arc::new(vec![entry("P1", 40)]),
                ..Default::default()
            },
        ];
        state.waiting_for = WaitingFor::BetweenGamesChoosePlayDraw {
            player: PlayerId(0),
            game_number: 2,
            score: state.match_score,
        };

        let result = apply_as_current(&mut state, GameAction::ChoosePlayDraw { play_first: true })
            .expect("between-games choose play/draw must start game two");

        assert!(matches!(
            result.events.as_slice(),
            [
                GameEvent::GameStarted,
                GameEvent::TurnStarted {
                    player_id: PlayerId(0),
                    turn_number: 1,
                },
                ..
            ]
        ));
        assert_eq!(result.log_entries[0].turn, 0);
        assert_eq!(result.log_entries[0].phase, Phase::Untap);
        assert_eq!(result.log_entries[1].turn, 1);
        assert_eq!(result.log_entries[1].phase, Phase::Untap);
        assert!(
            result.log_entries.iter().all(|entry| entry.turn <= 1),
            "a fresh game must not inherit the previous game's turn 73 context"
        );
    }
}
