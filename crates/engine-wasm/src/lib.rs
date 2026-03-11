use std::cell::RefCell;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use tsify::Tsify;
use wasm_bindgen::prelude::*;

use engine::database::CardDatabase;
use engine::game::engine::apply;
use engine::game::{load_deck_into_state, resolve_deck_list, start_game, DeckList};
use engine::types::{
    GameAction, GameEvent, GameState, ManaColor, ManaPool, ManaType, Phase, PlayerId, Zone,
};
use engine::types::format::FormatConfig;

use phase_ai::choose_action;
use phase_ai::config::{create_config, AiDifficulty, Platform};
use phase_ai::legal_actions::get_legal_actions;

thread_local! {
    static GAME_STATE: RefCell<Option<GameState>> = const { RefCell::new(None) };
    static CARD_DB: RefCell<Option<CardDatabase>> = const { RefCell::new(None) };
}

/// Verify WASM integration works.
#[wasm_bindgen]
pub fn ping() -> String {
    "phase-rs engine ready".to_string()
}

/// Create a default 2-player game state.
#[wasm_bindgen]
pub fn create_initial_state() -> JsValue {
    let state = GameState::default();
    serde_wasm_bindgen::to_value(&state).unwrap()
}

/// Load the card database from a JSON string (card-data.json contents).
/// Must be called before initialize_game to enable name-based deck resolution.
#[wasm_bindgen]
pub fn load_card_database(json_str: &str) -> Result<u32, JsValue> {
    let db = CardDatabase::from_json_str(json_str)
        .map_err(|e| JsValue::from_str(&format!("Failed to parse card database: {}", e)))?;
    let count = db.card_count() as u32;
    CARD_DB.with(|cell| {
        *cell.borrow_mut() = Some(db);
    });
    Ok(count)
}

/// Initialize a new game.
/// Accepts deck_data as a DeckList (name-only) or null/undefined for empty libraries.
/// format_config_js: optional FormatConfig JSON — defaults to Standard if null/undefined.
/// player_count: number of players — defaults to 2 if not provided.
/// Names are resolved against the card database loaded via load_card_database().
/// Returns the initial ActionResult (events + waiting_for).
#[wasm_bindgen]
pub fn initialize_game(
    deck_data: JsValue,
    seed: Option<f64>,
    format_config_js: JsValue,
    player_count: Option<u8>,
) -> JsValue {
    let seed = seed.map(|s| s as u64).unwrap_or(42);

    let format_config = if !format_config_js.is_null() && !format_config_js.is_undefined() {
        serde_wasm_bindgen::from_value::<FormatConfig>(format_config_js)
            .unwrap_or_else(|_| FormatConfig::standard())
    } else {
        FormatConfig::standard()
    };
    let count = player_count.unwrap_or(2);

    let mut state = GameState::new(format_config, count, seed);

    // Load deck data if provided — resolve names via the loaded card database
    if !deck_data.is_null() && !deck_data.is_undefined() {
        if let Ok(deck_list) = serde_wasm_bindgen::from_value::<DeckList>(deck_data) {
            CARD_DB.with(|cell| {
                if let Some(db) = cell.borrow().as_ref() {
                    let payload = resolve_deck_list(db, &deck_list);
                    load_deck_into_state(&mut state, &payload);
                }
            });
        }
    }

    // Start the game (auto-detects libraries for mulligan vs skip)
    let result = start_game(&mut state);

    GAME_STATE.with(|gs: &RefCell<Option<GameState>>| {
        *gs.borrow_mut() = Some(state);
    });

    serde_wasm_bindgen::to_value(&result).unwrap()
}

/// Submit a game action and return the ActionResult (events + waiting_for).
#[wasm_bindgen]
pub fn submit_action(action: JsValue) -> JsValue {
    let action: GameAction =
        serde_wasm_bindgen::from_value(action).expect("Failed to deserialize GameAction");

    GAME_STATE.with(|gs: &RefCell<Option<GameState>>| {
        let mut state_ref = gs.borrow_mut();
        let state = state_ref
            .as_mut()
            .expect("Game not initialized. Call initialize_game first.");
        match apply(state, action) {
            Ok(result) => serde_wasm_bindgen::to_value(&result).unwrap(),
            Err(e) => {
                let error_msg = format!("Engine error: {}", e);
                JsValue::from_str(&error_msg)
            }
        }
    })
}

/// Get the current game state as JSON.
/// Derived display fields (summoning sickness, devotion, etc.) are computed
/// automatically by the engine in apply()/start_game().
#[wasm_bindgen]
pub fn get_game_state() -> JsValue {
    GAME_STATE.with(|gs: &RefCell<Option<GameState>>| {
        let state_ref = gs.borrow();
        match state_ref.as_ref() {
            Some(state) => serde_wasm_bindgen::to_value(state).unwrap(),
            None => JsValue::NULL,
        }
    })
}

/// Get the legal actions for the current game state.
/// Returns a JS array of GameAction values.
#[wasm_bindgen]
pub fn get_legal_actions_js() -> JsValue {
    GAME_STATE.with(|gs: &RefCell<Option<GameState>>| {
        let state_ref = gs.borrow();
        match state_ref.as_ref() {
            Some(state) => {
                let actions = get_legal_actions(state);
                serde_wasm_bindgen::to_value(&actions).unwrap()
            }
            None => JsValue::NULL,
        }
    })
}

/// Restore the game state from a JSON string.
/// Uses serde_json which handles string-keyed maps (from localStorage round-trip)
/// correctly deserializing into HashMap<ObjectId, V>.
#[wasm_bindgen]
pub fn restore_game_state(json_str: &str) -> Result<(), JsValue> {
    let mut state: GameState = serde_json::from_str(json_str)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize GameState: {}", e)))?;
    state.rng = ChaCha20Rng::seed_from_u64(state.rng_seed);
    GAME_STATE.with(|gs| {
        *gs.borrow_mut() = Some(state);
    });
    Ok(())
}

/// Get the AI's chosen action for the current game state.
/// `difficulty` is one of: "VeryEasy", "Easy", "Medium", "Hard", "VeryHard".
/// `player_id` is the seat index of the AI player (0-based).
#[wasm_bindgen]
pub fn get_ai_action(difficulty: &str, player_id: u8) -> Result<JsValue, JsValue> {
    let ai_difficulty = match difficulty {
        "VeryEasy" => AiDifficulty::VeryEasy,
        "Easy" => AiDifficulty::Easy,
        "Medium" => AiDifficulty::Medium,
        "Hard" => AiDifficulty::Hard,
        "VeryHard" => AiDifficulty::VeryHard,
        _ => AiDifficulty::Medium,
    };

    let config = create_config(ai_difficulty, Platform::Wasm);

    GAME_STATE.with(|gs: &RefCell<Option<GameState>>| {
        let state_ref = gs.borrow();
        let state = state_ref
            .as_ref()
            .ok_or_else(|| JsValue::from_str("Game not initialized"))?;

        let ai_player = PlayerId(player_id);
        let mut rng = rand::rng();

        match choose_action(state, ai_player, &config, &mut rng) {
            Some(action) => serde_wasm_bindgen::to_value(&action)
                .map_err(|e| JsValue::from_str(&format!("Serialization error: {}", e))),
            None => Ok(JsValue::NULL),
        }
    })
}

// Tsify re-exports for TypeScript type generation.
// Newtype wrappers expose engine types to the WASM boundary.

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", content = "data")]
pub enum WasmAttackTarget {
    Player(u64),
    Planeswalker(u64),
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
#[serde(tag = "type", content = "data")]
pub enum WasmGameAction {
    PassPriority,
    PlayLand {
        card_id: u64,
    },
    CastSpell {
        card_id: u64,
        targets: Vec<u64>,
    },
    ActivateAbility {
        source_id: u64,
        ability_index: usize,
    },
    DeclareAttackers {
        attacks: Vec<(u64, WasmAttackTarget)>,
    },
    DeclareBlockers {
        assignments: Vec<(u64, u64)>,
    },
    MulliganDecision {
        keep: bool,
    },
    TapLandForMana {
        object_id: u64,
    },
    SelectCards {
        cards: Vec<u64>,
    },
    ChooseReplacement {
        index: usize,
    },
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub enum WasmManaColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub struct WasmManaPool {
    pub white: u32,
    pub blue: u32,
    pub black: u32,
    pub red: u32,
    pub green: u32,
    pub colorless: u32,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub enum WasmZone {
    Library,
    Hand,
    Battlefield,
    Graveyard,
    Stack,
    Exile,
    Command,
}

#[derive(Tsify, Serialize, Deserialize)]
#[tsify(into_wasm_abi, from_wasm_abi)]
pub enum WasmPhase {
    Untap,
    Upkeep,
    Draw,
    PreCombatMain,
    BeginCombat,
    DeclareAttackers,
    DeclareBlockers,
    CombatDamage,
    EndCombat,
    PostCombatMain,
    End,
    Cleanup,
}

// Suppress unused import warnings -- types are used for tsify generation.
const _: () = {
    fn _assert_types() {
        let _ = std::any::type_name::<GameAction>();
        let _ = std::any::type_name::<GameEvent>();
        let _ = std::any::type_name::<GameState>();
        let _ = std::any::type_name::<ManaColor>();
        let _ = std::any::type_name::<ManaPool>();
        let _ = std::any::type_name::<ManaType>();
        let _ = std::any::type_name::<Phase>();
        let _ = std::any::type_name::<Zone>();
    }
};
