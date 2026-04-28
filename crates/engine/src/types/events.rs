use serde::{Deserialize, Serialize};

use super::counter::CounterType;

use super::ability::{EffectKind, TargetRef};
use super::game_state::ZoneChangeRecord;
use super::identifiers::{CardId, ObjectId};
use super::mana::ManaType;
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::zones::Zone;

/// CR 121.1: Default `nth_in_step` for `CardDrawn` events deserialized from
/// older serialized state that predates the field. `1` means "first draw" —
/// the most permissive default for `ExceptFirstDrawInDrawStep` evaluators
/// (mirrors the natural draw-step behavior).
fn default_nth_in_step() -> u32 {
    1
}

/// Avatar crossover: The four elemental bending types, tracked per-turn on each player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BendingType {
    Fire,
    Air,
    Earth,
    Water,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlayerActionKind {
    SearchedLibrary,
    Scry,
    Surveil,
    CollectEvidence,
}

/// CR 701.30d: Result of a clash — whether the controller won, lost, or tied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClashResult {
    Won,
    Lost,
    Tied,
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
    /// CR 107.1b + CR 601.2f: The caster has chosen the value of X for a
    /// pending cast whose cost contained `ManaCostShard::X`.
    XValueChosen {
        player: PlayerId,
        object_id: ObjectId,
        value: u32,
    },
    AbilityActivated {
        source_id: ObjectId,
    },
    /// CR 603.6a: Enters-the-battlefield and zone-change triggers fire on this
    /// event. `from` is `None` when an object is created directly in a zone
    /// without a prior zone — e.g., a token is created on the battlefield
    /// (CR 111.1 + CR 603.6a: "an object that enters the battlefield as a
    /// token is created in the battlefield zone"). Treating token creation
    /// as a `ZoneChanged` event means every ETB trigger matcher (Elvish
    /// Vanguard, Soul Warden, Panharmonicon) automatically fires for tokens
    /// without bespoke per-matcher code paths.
    ZoneChanged {
        object_id: ObjectId,
        from: Option<Zone>,
        to: Zone,
        /// CR 603.10: Boxed to keep `GameEvent` variant size small. The record
        /// can be ~200 bytes and is only populated for this one variant; every
        /// other consumer (and every other event) would pay that cost inline.
        record: Box<ZoneChangeRecord>,
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
        /// CR 121.1 + CR 504.1: Ordinal of this draw within the current step
        /// (1-indexed). Set by the emitter to `player.cards_drawn_this_step`
        /// AFTER incrementing for this draw, so the first card drawn in a step
        /// has `nth_in_step == 1`. Used by `TriggerCondition::ExceptFirstDrawInDrawStep`
        /// to suppress the trigger on the draw step's mandatory first draw.
        #[serde(default = "default_nth_in_step")]
        nth_in_step: u32,
    },
    PermanentUntapped {
        object_id: ObjectId,
    },
    /// CR 702.26b: A permanent phased out (status changed to phased out).
    /// `indirect` is true iff this permanent was phased out because a host
    /// it was attached to phased out (CR 702.26g).
    PermanentPhasedOut {
        object_id: ObjectId,
        #[serde(default)]
        indirect: bool,
    },
    /// CR 702.26c: A permanent phased in (status changed to phased in).
    PermanentPhasedIn {
        object_id: ObjectId,
    },
    /// A player phased out. Player phasing is not formally governed by CR 702.26
    /// (which is permanent-only); semantics mirror the permanent rule and are
    /// driven by the small set of card Oracle text that says "you phase out".
    /// While phased out, the player is excluded from targeting, attacking,
    /// damage, and the 0-or-less life SBA.
    PlayerPhasedOut {
        player_id: PlayerId,
    },
    /// A player phased back in (typically at the start of their next turn or
    /// when an `UntilYourNextTurn` duration ends).
    PlayerPhasedIn {
        player_id: PlayerId,
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
        /// CR 120.10: Excess damage beyond lethal for creatures/planeswalkers/battles.
        #[serde(default)]
        excess: u32,
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
    /// Digital-only: A card was conjured from outside the game into a zone.
    ObjectConjured {
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
    /// CR 508.1h + CR 509.1d: The aggregate combat tax was paid; the declaration
    /// proceeds with every declared creature intact.
    CombatTaxPaid {
        player: PlayerId,
        total_mana_value: u32,
    },
    /// CR 508.1d + CR 509.1c: The combat tax was declined; the listed taxed
    /// creatures have been dropped from the declaration before it completes.
    CombatTaxDeclined {
        player: PlayerId,
        dropped: Vec<ObjectId>,
    },
    BecomesTarget {
        object_id: ObjectId,
        source_id: ObjectId,
    },
    /// CR 702.122d: A Vehicle's crew ability resolved.
    /// Carries creature list for trigger conditions that reference "creatures that crewed it".
    VehicleCrewed {
        vehicle_id: ObjectId,
        creatures: Vec<ObjectId>,
    },
    /// CR 702.184a: A Spacecraft's station ability resolved.
    /// Fires the `TriggerMode::Stationed` event for triggers on the Spacecraft
    /// that care about the act of being stationed. Carries the tapped creature
    /// and the number of counters added so downstream consumers (logs, future
    /// "whenever ~ is stationed by a creature with X" triggers) can see the
    /// inputs without re-deriving them.
    Stationed {
        spacecraft_id: ObjectId,
        creature_id: ObjectId,
        counters_added: u32,
    },
    /// CR 702.171a: A Mount's saddle ability resolved.
    /// Fires the `TriggerMode::Saddled` / `TriggerMode::BecomesSaddled` events
    /// for triggers that care about the act of being saddled. Carries the
    /// tapped creatures so trigger conditions referencing "creatures that
    /// saddled it" can resolve against last-known information.
    Saddled {
        mount_id: ObjectId,
        creatures: Vec<ObjectId>,
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
    /// CR 702.xxx: Prepare (Strixhaven) — a creature became prepared.
    /// Emitted only when the toggle actually flips (idempotent resolvers).
    /// Assign when WotC publishes SOS CR update.
    BecamePrepared {
        object_id: ObjectId,
    },
    /// CR 702.xxx: Prepare (Strixhaven) — a creature became unprepared.
    /// Emitted only when the toggle actually flips (idempotent resolvers).
    /// Assign when WotC publishes SOS CR update.
    BecameUnprepared {
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
    /// CR 725: A player became the monarch.
    MonarchChanged {
        player_id: PlayerId,
    },
    /// CR 702.131b: A player gained the city's blessing (Ascend).
    CityBlessingGained {
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
    /// CR 309.4c: A player moved their venture marker into a dungeon room.
    RoomEntered {
        player_id: PlayerId,
        dungeon: crate::game::dungeon::DungeonId,
        room_index: u8,
        room_name: String,
    },
    /// CR 709.5h-i: A Room permanent was given an unlocked designation.
    RoomDoorUnlocked {
        player_id: PlayerId,
        object_id: ObjectId,
        door: crate::game::game_object::RoomDoor,
        fully_unlocked: bool,
    },
    /// CR 309.7: A player completed a dungeon (removed from game).
    DungeonCompleted {
        player_id: PlayerId,
        dungeon: crate::game::dungeon::DungeonId,
    },
    /// CR 725: A player took the initiative.
    InitiativeTaken {
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
    /// CR 702.49a: A ninjutsu-family ability was activated (ninjutsu, commander ninjutsu, sneak).
    /// This is a special action, not an activated ability on the stack, so it does not fire
    /// AbilityActivated. Enables "whenever you activate a ninjutsu ability" triggers.
    NinjutsuActivated {
        player_id: PlayerId,
        source_id: ObjectId,
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
    /// CR 701.30: A clash occurred between two players.
    Clash {
        controller: PlayerId,
        opponent: PlayerId,
        controller_mana_value: Option<u32>,
        opponent_mana_value: Option<u32>,
        result: ClashResult,
    },
    /// CR 701.38a: A player cast a single vote in a Council's-dilemma
    /// resolution. One event per vote (so a player with multiple votes
    /// produces multiple events). `choice` is the lowercase canonical
    /// option name from `Effect::Vote.choices`.
    VoteCast {
        voter: PlayerId,
        choice: String,
        source_id: ObjectId,
    },
    /// CR 701.38: All voters have voted. Emitted before the per-choice tally
    /// sub-effects fire. `tallies` is `(choice, count)` pairs in `options`
    /// declaration order.
    VoteResolved {
        source_id: ObjectId,
        tallies: Vec<(String, u32)>,
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
    /// CR 702.85a: Cascade exiled the entire library (or whatever remained
    /// after replacement effects) without finding a nonland card with
    /// `mana_value < source_mv`. Emitted before the bottom-shuffle so the
    /// log/UI can announce the miss without inferring it from absence.
    CascadeMissed {
        controller: PlayerId,
        source_id: ObjectId,
        exiled_count: u32,
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
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Test".to_string(),
                ..ZoneChangeRecord::test_minimal(ObjectId(5), Some(Zone::Hand), Zone::Battlefield)
            }),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "ZoneChanged");
        assert_eq!(json["data"]["from"], "Hand");
        assert_eq!(json["data"]["to"], "Battlefield");
        assert_eq!(json["data"]["record"]["name"], "Test");
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
            excess: 0,
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
