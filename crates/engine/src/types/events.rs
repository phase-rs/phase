use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::game::game_object::CounterType;

use super::ability::{EffectKind, TargetRef};
use super::identifiers::{CardId, ObjectId};
use super::mana::ManaType;
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::zones::Zone;

/// Avatar crossover: The four elemental bending types, tracked per-turn on each player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BendingType {
    Fire,
    Air,
    Earth,
    Water,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum PlayerActionKind {
    SearchedLibrary,
    Scry,
    Surveil,
}

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
        object_id: ObjectId, // CR 601.2a: The spell object on the stack
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
        /// True when the source was tapped as part of producing this mana
        /// (mana ability with tap cost, or basic land tap). False for
        /// sacrifice-only mana abilities, effects, triggers, convoke, and
        /// doublers. Used by `TapsForMana` trigger matcher (CR 605.1a + CR 605.1b).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        tapped_for_mana: bool,
    },
    PermanentTapped {
        object_id: ObjectId,
        /// The source that caused the tap, if tapped by an external effect.
        /// `None` for self-initiated taps (mana abilities, attacking, crew, costs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caused_by: Option<ObjectId>,
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
    /// CR 615: Damage was prevented (by a prevention shield or protection).
    /// Enables "when damage is prevented" triggers.
    DamagePrevented {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
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
        /// Per-attacker targets — parallel to attacker_ids, same length and order.
        #[serde(default)]
        attacks: Vec<(ObjectId, crate::game::combat::AttackTarget)>,
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
        #[serde(default)]
        card_ids: Vec<ObjectId>,
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
    PlayerPerformedAction {
        player_id: PlayerId,
        action: PlayerActionKind,
    },
    /// CR 701.19a: Regeneration shield — consumed on use, expires at cleanup.
    Regenerated {
        object_id: ObjectId,
    },
    /// CR 701.60a: A creature was suspected.
    CreatureSuspected {
        object_id: ObjectId,
    },
    /// CR 719.3b: A Case enchantment became solved.
    CaseSolved {
        object_id: ObjectId,
    },
    /// CR 716.2a: A Class enchantment gained a new level.
    ClassLevelGained {
        object_id: ObjectId,
        level: u8,
    },
    /// CR 724: A player became the monarch.
    MonarchChanged {
        player_id: PlayerId,
    },
    /// CR 706: A die was rolled.
    DieRolled {
        player_id: PlayerId,
        sides: u8,
        result: u8,
    },
    /// CR 705: A coin was flipped.
    CoinFlipped {
        player_id: PlayerId,
        won: bool,
    },
    /// CR 701.54: The Ring tempted a player.
    RingTemptsYou {
        player_id: PlayerId,
    },
    /// Avatar crossover: A creature with firebending attacked, producing mana.
    Firebend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// Avatar crossover: A permanent or spell was airbent (exiled with alt-cast permission).
    Airbend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// Avatar crossover: A land was earthbent (animated with counters + return trigger).
    Earthbend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// Avatar crossover: A waterbend cost was paid (tap-to-pay for generic mana).
    Waterbend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// CR 702.139a: Companion revealed at game start.
    CompanionRevealed {
        player: PlayerId,
        card_name: String,
    },
    /// CR 702.139a: Companion moved to hand via {3} special action.
    CompanionMovedToHand {
        player: PlayerId,
        card_name: String,
    },
    /// CR 702.110: A creature exploited another creature (sacrificed via exploit ETB).
    CreatureExploited {
        exploiter: ObjectId,
        sacrificed: ObjectId,
    },
    /// CR 122.1: A player's energy counter total changed.
    EnergyChanged {
        player: PlayerId,
        delta: i32,
    },
    /// CR 702.179: A player's speed changed.
    SpeedChanged {
        player: PlayerId,
        old_speed: Option<u8>,
        new_speed: Option<u8>,
    },
    /// CR 122.1: A player counter (poison, experience, rad, ticket, etc.) changed.
    PlayerCounterChanged {
        player: PlayerId,
        counter_kind: PlayerCounterKind,
        delta: i32,
    },
    /// CR 700.14: Mana was spent on a spell cast, updating the cumulative total this turn.
    ManaExpended {
        player_id: PlayerId,
        amount_spent: u32,
        new_cumulative: u32,
    },
    /// Emitted when layer re-evaluation changes a creature's effective power/toughness.
    /// Generic event — not tied to any specific card or effect.
    PowerToughnessChanged {
        object_id: ObjectId,
        power: i32,
        toughness: i32,
        power_delta: i32,
        toughness_delta: i32,
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

    #[test]
    fn power_toughness_changed_roundtrips() {
        let event = GameEvent::PowerToughnessChanged {
            object_id: ObjectId(7),
            power: 5,
            toughness: 6,
            power_delta: 2,
            toughness_delta: 2,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }
}
