use serde::{Deserialize, Serialize};

use super::identifiers::{CardId, ObjectId};
use super::mana::{ManaColor, ManaType};
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
        mana_type: ManaType,
        source_id: ObjectId,
    },
    PermanentTapped {
        object_id: ObjectId,
    },
    PlayerLost {
        player_id: PlayerId,
    },
    MulliganStarted,
    CardsDrawn {
        player_id: PlayerId,
        count: u32,
    },
    CardDrawn {
        player_id: PlayerId,
        object_id: ObjectId,
    },
    PermanentUntapped {
        object_id: ObjectId,
    },
    LandPlayed {
        object_id: ObjectId,
        player_id: PlayerId,
    },
    StackPushed {
        object_id: ObjectId,
    },
    StackResolved {
        object_id: ObjectId,
    },
    Discarded {
        player_id: PlayerId,
        object_id: ObjectId,
    },
    DamageCleared {
        object_id: ObjectId,
    },
    GameOver {
        winner: Option<PlayerId>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_started_serializes_as_tagged_union() {
        let event = GameEvent::GameStarted;
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "GameStarted");
    }

    #[test]
    fn turn_started_serializes_with_data() {
        let event = GameEvent::TurnStarted {
            player_id: PlayerId(0),
            turn_number: 1,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "TurnStarted");
        assert_eq!(json["data"]["turn_number"], 1);
    }

    #[test]
    fn zone_changed_serializes_all_fields() {
        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Zone::Hand,
            to: Zone::Battlefield,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "ZoneChanged");
        assert_eq!(json["data"]["from"], "Hand");
        assert_eq!(json["data"]["to"], "Battlefield");
    }

    #[test]
    fn game_over_with_winner_roundtrips() {
        let event = GameEvent::GameOver {
            winner: Some(PlayerId(1)),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn game_over_without_winner_roundtrips() {
        let event = GameEvent::GameOver { winner: None };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }
}
