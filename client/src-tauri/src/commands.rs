use std::sync::Mutex;

use engine::ai_support::legal_actions;
use engine::game::combat::has_summoning_sickness;
use engine::game::coverage::unimplemented_mechanics;
use engine::game::engine::apply;
use engine::game::static_abilities::{check_static_ability, StaticCheckContext};
use engine::game::{load_deck_into_state, start_game, DeckPayload};
use engine::types::game_state::ActionResult;
use engine::types::match_config::MatchConfig;
use engine::types::player::PlayerId;
use engine::types::{GameAction, GameState};

use phase_ai::choose_action;
use phase_ai::config::{create_config_for_players, AiDifficulty, Platform};

pub struct AppState {
    pub game: Mutex<Option<GameState>>,
}

#[tauri::command]
pub fn initialize_game(
    state: tauri::State<AppState>,
    deck_data: Option<DeckPayload>,
    seed: Option<u64>,
    match_config: Option<MatchConfig>,
) -> Result<ActionResult, String> {
    let seed = seed.unwrap_or(42);
    let mut game = GameState::new_two_player(seed);
    game.match_config = match_config.unwrap_or_default();

    if let Some(payload) = deck_data {
        load_deck_into_state(&mut game, &payload);
    }

    let result = start_game(&mut game);
    *state.game.lock().map_err(|e| e.to_string())? = Some(game);

    Ok(result)
}

#[tauri::command]
pub fn submit_action(
    state: tauri::State<AppState>,
    action: GameAction,
) -> Result<ActionResult, String> {
    let mut guard = state.game.lock().map_err(|e| e.to_string())?;
    let game = guard.as_mut().ok_or("Game not initialized")?;

    apply(game, action).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_game_state(state: tauri::State<AppState>) -> Result<GameState, String> {
    let mut guard = state.game.lock().map_err(|e| e.to_string())?;
    let game = guard.as_mut().ok_or("Game not initialized")?;

    // Compute derived fields (same as WASM bridge)
    let turn = game.turn_number;
    for obj in game.objects.values_mut() {
        obj.unimplemented_mechanics = unimplemented_mechanics(obj);
        obj.has_summoning_sickness = has_summoning_sickness(obj, turn);
    }

    let peek_flags: Vec<bool> = game
        .players
        .iter()
        .map(|p| {
            let ctx = StaticCheckContext {
                player_id: Some(p.id),
                ..Default::default()
            };
            check_static_ability(game, "MayLookAtTopOfLibrary", &ctx)
        })
        .collect();
    for (i, flag) in peek_flags.into_iter().enumerate() {
        game.players[i].can_look_at_top_of_library = flag;
    }

    Ok(game.clone())
}

#[tauri::command]
pub fn get_legal_actions(state: tauri::State<AppState>) -> Result<Vec<GameAction>, String> {
    let guard = state.game.lock().map_err(|e| e.to_string())?;
    let game = guard.as_ref().ok_or("Game not initialized")?;

    Ok(legal_actions(game))
}

#[tauri::command]
pub fn get_ai_action(
    state: tauri::State<AppState>,
    difficulty: String,
) -> Result<Option<GameAction>, String> {
    let guard = state.game.lock().map_err(|e| e.to_string())?;
    let game = guard.as_ref().ok_or("Game not initialized")?;

    let ai_difficulty = match difficulty.as_str() {
        "VeryEasy" => AiDifficulty::VeryEasy,
        "Easy" => AiDifficulty::Easy,
        "Medium" => AiDifficulty::Medium,
        "Hard" => AiDifficulty::Hard,
        "VeryHard" => AiDifficulty::VeryHard,
        _ => AiDifficulty::Medium,
    };

    let config =
        create_config_for_players(ai_difficulty, Platform::Native, game.players.len() as u8);
    let mut rng = rand::rng();

    Ok(choose_action(game, PlayerId(1), &config, &mut rng))
}

#[tauri::command]
pub fn dispose_game(state: tauri::State<AppState>) -> Result<(), String> {
    *state.game.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}
