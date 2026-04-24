use std::sync::Mutex;

use engine::ai_support::{auto_pass_recommended, legal_actions_full};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use std::collections::HashMap;
use serde::Serialize;
use engine::game::combat::has_summoning_sickness;
use engine::game::coverage::unimplemented_mechanics;
use engine::game::engine::apply;
use engine::game::static_abilities::{check_static_ability, StaticCheckContext};
use engine::types::statics::StaticMode;
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
    actor: u8,
    action: GameAction,
) -> Result<ActionResult, String> {
    // `actor` is the local player's PlayerId as tracked by the frontend
    // adapter. In desktop/Tauri mode there is a single local human so the
    // trust boundary is trivial, but we still pass it through so the
    // engine's guard enforces identity the same way every transport does.
    let mut guard = state.game.lock().map_err(|e| e.to_string())?;
    let game = guard.as_mut().ok_or("Game not initialized")?;

    apply(
        game,
        engine::types::player::PlayerId(actor),
        action,
    )
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_game_state(
    state: tauri::State<AppState>,
) -> Result<engine::game::derived_views::ClientGameState, String> {
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
            check_static_ability(game, StaticMode::MayLookAtTopOfLibrary, &ctx)
        })
        .collect();
    for (i, flag) in peek_flags.into_iter().enumerate() {
        game.players[i].can_look_at_top_of_library = flag;
    }

    // Return the wire envelope `{ state, derived }` — same shape produced
    // by the engine-wasm getter, so the frontend adapter unwraps identically
    // regardless of platform.
    let derived = engine::game::derived_views::derive_views(game);
    Ok(engine::game::derived_views::ClientGameState {
        state: game.clone(),
        derived,
    })
}

/// Mirror of the `LegalActionsResult` shape exposed by `engine-wasm`. Keeps
/// the Tauri desktop adapter aligned with the browser/WASM path so the
/// frontend's `collectObjectActions(legalActionsByObject, objectId)` lookup
/// works identically on both transports.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LegalActionsPayload {
    actions: Vec<GameAction>,
    auto_pass_recommended: bool,
    spell_costs: HashMap<ObjectId, ManaCost>,
    legal_actions_by_object: HashMap<ObjectId, Vec<GameAction>>,
}

#[tauri::command]
pub fn get_legal_actions(state: tauri::State<AppState>) -> Result<LegalActionsPayload, String> {
    let guard = state.game.lock().map_err(|e| e.to_string())?;
    let game = guard.as_ref().ok_or("Game not initialized")?;

    let (actions, spell_costs, legal_actions_by_object) = legal_actions_full(game);
    let auto_pass_recommended = auto_pass_recommended(game, &actions);
    Ok(LegalActionsPayload {
        actions,
        auto_pass_recommended,
        spell_costs,
        legal_actions_by_object,
    })
}

#[tauri::command]
pub fn get_ai_action(
    state: tauri::State<AppState>,
    difficulty: String,
    player_id: u8,
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

    Ok(choose_action(game, PlayerId(player_id), &config, &mut rng))
}

#[tauri::command]
pub fn dispose_game(state: tauri::State<AppState>) -> Result<(), String> {
    *state.game.lock().map_err(|e| e.to_string())? = None;
    Ok(())
}
