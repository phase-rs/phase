use std::sync::Mutex;

use engine::ai_support::{auto_pass_recommended, legal_actions_full};
use engine::database::legality::{any_ai_difficulty_is_cedh, validate_cedh_bracket};
use engine::database::CardDatabase;
use engine::game::deck_loading::PlayerDeckPayload;
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
    /// Loaded by the frontend at adapter init via `load_card_database`.
    /// Needed by `initialize_game` so `load_and_hydrate_decks` can populate
    /// `back_face` on dual-faced cards (Adventure, Omen, MDFC, Transform,
    /// Meld, Prepare). Without it, dual-faced-card behavior silently no-ops
    /// on desktop. Mirrors `CARD_DB` in the WASM bridge.
    pub card_db: Mutex<Option<CardDatabase>>,
}

/// Structured error for `initialize_game` so the frontend can discriminate a
/// cEDH bracket violation without substring-matching the Rust `Display` output.
/// Serializes as `{ "kind": "...", "message": "..." }`, mirroring the WASM
/// bridge's `cedh_bracket_violation` envelope flag — both transports surface the
/// violation as a typed signal the adapter maps to `BRACKET_VIOLATION`.
#[derive(Serialize)]
#[serde(tag = "kind", content = "message")]
pub enum CommandError {
    BracketViolation(String),
    Generic(String),
}

impl CommandError {
    fn generic(e: impl std::fmt::Display) -> Self {
        Self::Generic(e.to_string())
    }
}

#[tauri::command]
pub fn initialize_game(
    state: tauri::State<AppState>,
    deck_data: Option<DeckPayload>,
    seed: Option<u64>,
    match_config: Option<MatchConfig>,
) -> Result<ActionResult, CommandError> {
    let seed = seed.unwrap_or(42);
    let mut game = GameState::new_two_player(seed);
    game.match_config = match_config.unwrap_or_default();

    if let Some(payload) = deck_data {
        // Validate the table as a cEDH game only when any AI seat has CEDH
        // difficulty. This prevents a spurious bracket-lock error when a user
        // brings a bracket-5 tagged deck against a non-cEDH AI — that pairing
        // is allowed by spec (section 5.5). Gating on AI difficulty (not deck
        // bracket tier) is the correct signal for "is this a cEDH game?"
        if any_ai_difficulty_is_cedh(&payload.ai_difficulties) {
            let all_decks: Vec<&PlayerDeckPayload> =
                std::iter::once(&payload.player)
                    .chain(std::iter::once(&payload.opponent))
                    .chain(payload.ai_decks.iter())
                    .collect();
            validate_cedh_bracket(&all_decks)
                .map_err(|e| CommandError::BracketViolation(e.to_string()))?;
        }

        // Canonical init sequence shared with WASM + server-core transports.
        // Passes the CardDatabase so `load_and_hydrate_decks` can populate
        // `back_face` on dual-faced cards (Adventure, Omen, MDFC, Transform,
        // Meld, Prepare). Frontend loads the DB once at adapter startup via
        // `load_card_database` — if that hasn't happened yet, `db` is None
        // and `load_and_hydrate_decks` logs a once-per-process warn.
        let db_guard = state.card_db.lock().map_err(CommandError::generic)?;
        load_and_hydrate_decks(&mut game, &payload, db_guard.as_ref());
    }

    let result = start_game(&mut game);
    *state.game.lock().map_err(CommandError::generic)? = Some(game);

    Ok(result)
}

/// Parse `card-data.json` contents into a `CardDatabase` and stash it on
/// `AppState`. Frontend calls this once at adapter init so `initialize_game`
/// can rehydrate dual-faced cards. Returns the number of faces loaded so
/// the frontend can sanity-check the parse. Mirrors the WASM bridge's
/// `load_card_database`.
#[tauri::command]
pub fn load_card_database(
    state: tauri::State<AppState>,
    json_str: String,
) -> Result<u32, String> {
    let db = CardDatabase::from_json_str(&json_str)
        .map_err(|e| format!("Failed to parse card database: {e}"))?;
    let count = db.card_count() as u32;
    *state.card_db.lock().map_err(|e| e.to_string())? = Some(db);
    Ok(count)
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
