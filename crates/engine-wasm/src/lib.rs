use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use tsify::Tsify;
use wasm_bindgen::prelude::*;

use engine::game::engine::apply;
use engine::types::{ActionResult, GameAction, GameEvent, GameState, ManaColor, ManaPool, ManaType, Phase, Zone};
use engine::types::player::PlayerId;

use forge_ai::config::{AiDifficulty, Platform, create_config};
use forge_ai::choose_action;

thread_local! {
    static GAME_STATE: RefCell<Option<GameState>> = const { RefCell::new(None) };
}

/// Verify WASM integration works.
#[wasm_bindgen]
pub fn ping() -> String {
    "forge-ts engine ready".to_string()
}

/// Create a default 2-player game state.
#[wasm_bindgen]
pub fn create_initial_state() -> JsValue {
    let state = GameState::default();
    serde_wasm_bindgen::to_value(&state).unwrap()
}

/// Initialize a new game with two players.
/// Accepts deck_data as JSON (reserved for future deck loading).
/// Returns the initial ActionResult (events + waiting_for).
#[wasm_bindgen]
pub fn initialize_game(_deck_data: JsValue) -> JsValue {
    let state = GameState::new_two_player(42);
    let waiting_for = state.waiting_for.clone();
    GAME_STATE.with(|gs: &RefCell<Option<GameState>>| {
        *gs.borrow_mut() = Some(state);
    });
    let result = ActionResult {
        events: vec![GameEvent::GameStarted],
        waiting_for,
    };
    serde_wasm_bindgen::to_value(&result).unwrap()
}

/// Submit a game action and return the ActionResult (events + waiting_for).
#[wasm_bindgen]
pub fn submit_action(action: JsValue) -> JsValue {
    let action: GameAction = serde_wasm_bindgen::from_value(action)
        .expect("Failed to deserialize GameAction");

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
    PlayLand { card_id: u64 },
    CastSpell { card_id: u64, targets: Vec<u64> },
    ActivateAbility { source_id: u64, ability_index: usize },
    DeclareAttackers { attacker_ids: Vec<u64> },
    DeclareBlockers { assignments: Vec<(u64, u64)> },
    MulliganDecision { keep: bool },
    TapLandForMana { object_id: u64 },
    SelectCards { cards: Vec<u64> },
    ChooseReplacement { index: usize },
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
