use serde::{Deserialize, Serialize};

use crate::game::game_object::CounterType;

use super::ability::{EffectKind, TargetRef};
use super::identifiers::{CardId, ObjectId};
use super::mana::ManaType;
use super::phase::Phase;
use super::player::PlayerId;
use super::zones::Zone;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    DamageDealt {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
        is_combat: bool,
    },
    SpellCountered {
        object_id: ObjectId,
        countered_by: ObjectId,
    },
    CounterAdded {
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
    },
    CounterRemoved {
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
    },
    TokenCreated {
        object_id: ObjectId,
        name: String,
    },
    CreatureDestroyed {
        object_id: ObjectId,
    },
    PermanentSacrificed {
        object_id: ObjectId,
        player_id: PlayerId,
    },
    EffectResolved {
        kind: EffectKind,
        source_id: ObjectId,
    },
    AttackersDeclared {
        attacker_ids: Vec<ObjectId>,
        defending_player: PlayerId,
    },
    BlockersDeclared {
        assignments: Vec<(ObjectId, ObjectId)>,
    },
    BecomesTarget {
        object_id: ObjectId,
        source_id: ObjectId,
    },
    ReplacementApplied {
        source_id: ObjectId,
        event_type: String,
    },
    Transformed {
        object_id: ObjectId,
    },
    DayNightChanged {
        new_state: String,
    },
    TurnedFaceUp {
        object_id: ObjectId,
    },
    CardsRevealed {
        player: PlayerId,
        card_names: Vec<String>,
    },
    CombatDamageDealtToPlayer {
        player_id: PlayerId,
        source_ids: Vec<ObjectId>,
    },
    PlayerEliminated {
        player_id: PlayerId,
    },
    CrimeCommitted {
        player_id: PlayerId,
    },
    Cycled {
        player_id: PlayerId,
        object_id: ObjectId,
    },
    /// CR 701.15: A permanent's regeneration shield was consumed, preventing destruction.
    Regenerated {
        object_id: ObjectId,
    },
    /// CR 702.157a: A creature was suspected.
    CreatureSuspected {
        object_id: ObjectId,
    },
    /// CR 719.2: A Case enchantment became solved.
    CaseSolved {
        object_id: ObjectId,
    },
    /// CR 716.5: A Class enchantment gained a new level.
    ClassLevelGained {
        object_id: ObjectId,
        level: u8,
    },
    /// CR 722: A player became the monarch.
    MonarchChanged {
        player_id: PlayerId,
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

    #[test]
    fn damage_dealt_event_roundtrips() {
        use crate::types::ability::TargetRef;
        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn effect_resolved_event_roundtrips() {
        let event = GameEvent::EffectResolved {
            kind: EffectKind::DealDamage,
            source_id: ObjectId(5),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn combat_damage_dealt_to_player_roundtrips() {
        let event = GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_ids: vec![ObjectId(10), ObjectId(11)],
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }
}
