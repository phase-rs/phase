use serde::{Deserialize, Serialize};

use super::identifiers::{CardId, ObjectId};
use super::mana::ManaColor;
use super::phase::Phase;
use super::player::PlayerId;
use super::zones::Zone;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum GameEvent {
    GameStarted,
    TurnStarted {
        player_id: PlayerId,
        turn_number: u32,
    },
    PhaseChanged {
        phase: Phase,
    },
    PriorityPassed {
        player_id: PlayerId,
    },
    SpellCast {
        card_id: CardId,
        controller: PlayerId,
    },
    AbilityActivated {
        source_id: ObjectId,
    },
    ZoneChanged {
        object_id: ObjectId,
        from: Zone,
        to: Zone,
    },
    LifeChanged {
        player_id: PlayerId,
        amount: i32,
    },
    ManaAdded {
        player_id: PlayerId,
        color: ManaColor,
        amount: u32,
    },
    GameOver {
        winner: Option<PlayerId>,
    },
}
