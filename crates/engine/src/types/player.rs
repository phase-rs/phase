use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;
use super::mana::ManaPool;

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    JsonSchema,
)]
#[serde(transparent)]
pub struct PlayerId(pub u8);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub life: i32,
    pub mana_pool: ManaPool,

    // Per-player zones
    pub library: Vec<ObjectId>,
    pub hand: Vec<ObjectId>,
    pub graveyard: Vec<ObjectId>,

    // Tracking
    pub has_drawn_this_turn: bool,
    pub lands_played_this_turn: u8,
    pub poison_counters: u32,
    #[serde(default)]
    pub life_gained_this_turn: u32,
    #[serde(default)]
    pub descended_this_turn: bool,

    // Elimination tracking (N-player support)
    #[serde(default)]
    pub is_eliminated: bool,

    // Derived fields (computed in WASM bridge, not persisted)
    #[serde(skip_deserializing, default)]
    pub can_look_at_top_of_library: bool,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            id: PlayerId(0),
            life: 20,
            mana_pool: ManaPool::default(),
            library: Vec::new(),
            hand: Vec::new(),
            graveyard: Vec::new(),
            has_drawn_this_turn: false,
            lands_played_this_turn: 0,
            poison_counters: 0,
            life_gained_this_turn: 0,
            descended_this_turn: false,
            is_eliminated: false,
            can_look_at_top_of_library: false,
        }
    }
}
