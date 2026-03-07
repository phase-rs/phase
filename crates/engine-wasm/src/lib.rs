use serde::{Deserialize, Serialize};
use tsify::Tsify;
use wasm_bindgen::prelude::*;

use engine::types::{GameAction, GameEvent, GameState, ManaColor, ManaPool, Phase, Zone};

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
        let _ = std::any::type_name::<Phase>();
        let _ = std::any::type_name::<Zone>();
    }
};
