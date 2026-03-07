use serde::{Deserialize, Serialize};

use super::mana::ManaPool;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlayerId(pub u8);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Player {
    pub id: PlayerId,
    pub life: i32,
    pub mana_pool: ManaPool,
}

impl Default for Player {
    fn default() -> Self {
        Player {
            id: PlayerId(0),
            life: 20,
            mana_pool: ManaPool::default(),
        }
    }
}
