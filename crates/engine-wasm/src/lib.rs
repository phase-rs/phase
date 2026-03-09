use std::cell::RefCell;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use tsify::Tsify;
use wasm_bindgen::prelude::*;

use engine::game::engine::apply;
use engine::game::{load_deck_into_state, start_game, DeckPayload};
use engine::types::player::PlayerId;
use engine::types::{GameAction, GameEvent, GameState, ManaColor, ManaPool, ManaType, Phase, Zone};

use forge_ai::choose_action;
use forge_ai::config::{create_config, AiDifficulty, Platform};
use forge_ai::legal_actions::get_legal_actions;

thread_local! {
    static GAME_STATE: RefCell<Option<GameState>> = const { RefCell::new(None) };
}

/// Verify WASM integration works.
#[wasm_bindgen]
pub fn ping() -> String {
    "forge-rs engine ready".to_string()
}

/// Create a default 2-player game state.
#[wasm_bindgen]
pub fn create_initial_state() -> JsValue {
    let state = GameState::default();
    serde_wasm_bindgen::to_value(&state).unwrap()
}

/// Initialize a new game with two players.
/// Accepts deck_data as a DeckPayload JSON (or null/undefined for empty libraries).
/// Returns the initial ActionResult (events + waiting_for).
#[wasm_bindgen]
pub fn initialize_game(deck_data: JsValue, seed: Option<f64>) -> JsValue {
    let seed = seed.map(|s| s as u64).unwrap_or(42);
    let mut state = GameState::new_two_player(seed);

    // Load deck data if provided
    if !deck_data.is_null() && !deck_data.is_undefined() {
        if let Ok(payload) = serde_wasm_bindgen::from_value::<DeckPayload>(deck_data) {
            load_deck_into_state(&mut state, &payload);
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
/// Computes the `has_unimplemented_mechanics` flag on each object before
/// serializing so the frontend can display coverage warnings.
#[wasm_bindgen]
pub fn get_game_state() -> JsValue {
    GAME_STATE.with(|gs: &RefCell<Option<GameState>>| {
        let mut state_ref = gs.borrow_mut();
        match state_ref.as_mut() {
            Some(state) => {
                let turn = state.turn_number;
                for obj in state.objects.values_mut() {
                    obj.has_unimplemented_mechanics =
                        engine::game::coverage::has_unimplemented_mechanics(obj);
                    obj.has_summoning_sickness =
                        engine::game::combat::has_summoning_sickness(obj, turn);
                }
                serde_wasm_bindgen::to_value(state).unwrap()
            }
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

/// Restore the game state from a serialized GameState (for undo support).
/// Replaces the thread-local GAME_STATE and reconstructs the RNG from seed.
#[wasm_bindgen]
pub fn restore_game_state(state_js: JsValue) -> Result<(), JsValue> {
    let mut state: GameState = serde_wasm_bindgen::from_value(state_js)
        .map_err(|e| JsValue::from_str(&format!("Failed to deserialize GameState: {}", e)))?;
    state.rng = ChaCha20Rng::seed_from_u64(state.rng_seed);
    GAME_STATE.with(|gs| {
        *gs.borrow_mut() = Some(state);
    });
    Ok(())
}

/// Get the AI's chosen action for the current game state.
/// `difficulty` is one of: "VeryEasy", "Easy", "Medium", "Hard", "VeryHard".
#[wasm_bindgen]
pub fn get_ai_action(difficulty: &str) -> Result<JsValue, JsValue> {
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

        // AI is always player 1
        let ai_player = PlayerId(1);
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
        attacker_ids: Vec<u64>,
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
