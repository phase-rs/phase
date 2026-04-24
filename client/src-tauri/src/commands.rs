use std::sync::Mutex;

use engine::ai_support::{auto_pass_recommended, legal_actions_full};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use std::collections::HashMap;
use serde::Serialize;
use engine::game::derived::derive_display_state;
use engine::game::engine::apply;
use engine::game::{load_and_hydrate_decks, start_game, DeckPayload};
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
        // Canonical init sequence shared with WASM + server-core transports.
        // Passing `None` here is a known gap: dual-faced cards (Adventure,
        // Omen, MDFC, Transform, Meld, Prepare) won't have `back_face`
        // populated on Tauri desktop until an `AppState::card_db` is wired
        // and threaded through. `load_and_hydrate_decks` emits a `warn!`
        // so the gap is visible in logs rather than silent. TODO: load
        // card-data.json into a `CardDatabase` at app startup and pass it
        // here to unlock dual-faced cards on desktop.
        load_and_hydrate_decks(&mut game, &payload, None);
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

    // Populate display-only derived fields (unimplemented_mechanics,
    // has_summoning_sickness, devotion, commander_tax, per-player
    // can_look_at_top_of_library). Canonical implementation shared with
    // the WASM bridge — see `engine::game::derived::derive_display_state`.
    // Inline re-implementation was the source of CR-drift after the
    // `has_summoning_sickness` signature change; one authority avoids it.
    derive_display_state(game);

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
