use serde::{Deserialize, Serialize};

use super::counter::CounterType;

use super::ability::{AbilityTag, EffectKind, TargetRef};
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

fn default_nth_in_turn() -> u32 {
    1
}

/// CR 605.1a + CR 605.1b + CR 605.4a: Records whether a `ManaAdded` event was
/// produced by tapping a mana source, and whether the coupled `TapsForMana`
/// triggered mana abilities have already been resolved.
///
/// A triggered mana ability (CR 605.1b) resolves immediately after the mana
/// ability that triggered it, without using the stack (CR 605.4a). During an
/// auto-tapped cost payment the engine resolves those triggers inline so the
/// bonus mana is available for the affordability check; `FromTapTriggersResolved`
/// marks such events so the deferred post-action trigger scan does not resolve
/// them a second time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaTapState {
    /// Mana not produced by a tap — effects, triggers, convoke, doublers.
    #[default]
    NotFromTap,
    /// Produced by tapping a source (CR 605.1a tap cost); coupled `TapsForMana`
    /// triggered mana abilities have not yet been resolved.
    FromTap,
    /// Produced by tapping a source; coupled triggered mana abilities were
    /// already resolved inline during cost payment (CR 605.4a).
    FromTapTriggersResolved,
}

impl ManaTapState {
    /// True when the mana was produced by tapping a source, regardless of
    /// whether the coupled triggered mana abilities have been resolved yet.
    pub fn tapped_for_mana(self) -> bool {
        !matches!(self, ManaTapState::NotFromTap)
    }

    /// Pre-resolution tap state for a freshly produced mana event: `FromTap`
    /// when the source was tapped, `NotFromTap` otherwise.
    pub fn from_tap(tapped: bool) -> Self {
        if tapped {
            ManaTapState::FromTap
        } else {
            ManaTapState::NotFromTap
        }
    }

    /// Serde `skip_serializing_if` predicate — omit the default from the wire.
    fn is_not_from_tap(&self) -> bool {
        matches!(self, ManaTapState::NotFromTap)
    }
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
    /// CR 701.24a: A player shuffled their library.
    ShuffledLibrary,
    /// CR 701.34a: A player proliferated.
    Proliferate,
    /// CR 701.16a: A player investigated (created a Clue token).
    Investigate,
}

/// CR 701.30d: Result of a clash — whether the controller won, lost, or tied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClashResult {
    Won,
    Lost,
    Tied,
}

/// CR 103.1 / CR 706: one round of the starting-player d20 roll-off.
/// `rolls` are in seat order; round 1 contains every seat, and each later
/// round contains exactly the previous round's tied-max group (CR 103.1
/// reroll). The high roller of the final round becomes the starting player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContestRound {
    pub rolls: Vec<(PlayerId, u8)>,
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
    /// CR 707.10: A spell was copied onto the stack. A copy of a spell isn't
    /// cast, so this is a distinct event from `SpellCast` — copy-sensitive
    /// triggers (Magecraft, "whenever you copy a spell") fire on this, while
    /// cast-only triggers (Prowess, storm, cascade) do not.
    SpellCopied {
        card_id: CardId,
        controller: PlayerId,
        object_id: ObjectId,   // the copy's stack object id
        original_id: ObjectId, // CR 707.10: the spell that was copied
    },
    /// CR 107.1b + CR 601.2f: The caster has chosen the value of X for a
    /// pending cast whose cost contained `ManaCostShard::X`.
    XValueChosen {
        player: PlayerId,
        object_id: ObjectId,
        value: u32,
    },
    /// CR 602.1 + CR 605.3b: An activated ability has been activated and put on
    /// the stack. **Not emitted for mana abilities** (CR 605.3b: mana abilities
    /// resolve immediately without using the stack and follow a separate code
    /// path that never reaches this event). This invariant — `AbilityActivated`
    /// fires only for non-mana activations — is what makes
    /// `TriggerCondition::ActivatedAbilityIsNonMana` trivially satisfied when
    /// matched against this event, and is what lets the generic
    /// "Whenever a player activates an ability that isn't a mana ability"
    /// trigger class (Burning-Tree Shaman, Flamescroll Celebrant) listen here.
    AbilityActivated {
        /// CR 602.2a: "Its controller is the player who activated the ability."
        /// Required so `extract_player_from_event` can resolve "that player" /
        /// `TargetFilter::TriggeringPlayer` references in the resolving
        /// ability's effect (Burning-Tree Shaman, Flamescroll Celebrant).
        player_id: PlayerId,
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
        /// Whether this mana came from tapping a source, and whether the
        /// coupled `TapsForMana` triggered mana abilities (CR 605.1a + CR 605.1b)
        /// have already been resolved. Consumed by the `TapsForMana` trigger
        /// matcher and by the post-action trigger scan's double-resolution guard.
        #[serde(default, skip_serializing_if = "ManaTapState::is_not_from_tap")]
        tap_state: ManaTapState,
    },
    /// CR 106.12a: A mana ability whose activation cost includes the `{T}`
    /// symbol (CR 106.12) resolved and produced mana. Emitted **exactly once
    /// per resolution** — unlike `ManaAdded`, which is per mana unit (CR 106.4)
    /// pool accounting. The `TapsForMana` trigger matcher keys off this event
    /// so triggers like Vorinclex fire once per tap, not once per mana point.
    TappedForMana {
        player_id: PlayerId,
        source_id: ObjectId,
        /// The full set of mana units produced by this resolution. Consumed by
        /// `TriggerEventManaType` (one trigger-event mana per distinct color).
        produced: Vec<ManaType>,
        /// CR 605.4a: Tracks whether the coupled `TapsForMana` triggered mana
        /// abilities have already resolved — the post-action double-resolution
        /// guard and the inline resolver's Pass-2 flip key off this.
        #[serde(default, skip_serializing_if = "ManaTapState::is_not_from_tap")]
        tap_state: ManaTapState,
    },
    /// CR 500.5 + CR 703.4q: A single mana unit was emptied from a player's
    /// pool during the step-end empty event after the CR 616.1 replacement
    /// pipeline resolved. `source_id` is the unit's original producer
    /// (mirrors `ManaAdded::source_id`).
    ManaPoolEmptied {
        player_id: PlayerId,
        source_id: ObjectId,
        color: ManaType,
    },
    /// CR 614.1a + CR 703.4q: A `Transform(_)` step-end mana handler (Horizon
    /// Stone, Kruphix, Omnath, Ozai) recolored a unit in place during the
    /// step-end empty event. The unit stays in the pool with its new color.
    ManaRecolored {
        player_id: PlayerId,
        from: ManaType,
        to: ManaType,
    },
    PermanentTapped {
        object_id: ObjectId,
        /// The source that caused the tap, if tapped by an external effect.
        /// `None` for self-initiated taps (mana abilities, attacking, crew, costs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caused_by: Option<ObjectId>,
    },
    /// CR 701.43a + CR 701.43d: A creature was exerted as it attacked. Fires the
    /// linked `TriggerMode::Exerted` "when you do" trigger (Combat Celebrant,
    /// Glory-Bound Initiate, ...).
    CreatureExerted {
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
        /// Ordinal of this draw within the current turn (1-indexed). Set by
        /// the emitter after incrementing `player.cards_drawn_this_turn`, so
        /// Nth-card draw triggers evaluate against the individual draw event
        /// rather than the final post-batch turn total.
        #[serde(default = "default_nth_in_turn")]
        nth_in_turn: u32,
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
        from_zone: Zone,
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
        /// CR 109.5: "you control" on counter triggers refers to the countering
        /// spell or ability's controller, not necessarily the source object's
        /// current controller.
        countered_by_controller: PlayerId,
    },
    CounterAdded {
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
    },
    /// CR 702.100b: A creature evolved because one or more +1/+1 counters were
    /// put on it as a result of its evolve ability resolving.
    Evolved {
        object_id: ObjectId,
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
    /// CR 701.3d: An Aura, Equipment, or Fortification became unattached from
    /// the object or player it was attached to.
    Unattached {
        attachment_id: ObjectId,
        old_target: TargetRef,
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
        target: TargetRef,
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
        /// CR 120.1 + CR 510.2: Per-source combat damage amounts for this
        /// specific combat damage step. Using step-local amounts instead of a
        /// bare `Vec<ObjectId>` prevents double-strike / extra-combat inflation
        /// in `matching_damage_done_once_by_controller_event`: each
        /// `apply_combat_damage` call produces exactly one event per player with
        /// the amounts from that step only.
        ///
        /// Migration note: this field replaces the former `source_ids:
        /// Vec<ObjectId>`. `#[serde(default)]` keeps deserialization of older
        /// persisted state infallible, but an old-format event (a game persisted
        /// mid-combat-damage-trigger by a pre-rename binary and restored after an
        /// upgrade) decodes to an empty set — the legacy `source_ids` array is
        /// dropped. This is acceptable: the event is transient (produced and
        /// consumed within one combat-damage step), the window is the rare
        /// mid-trigger save across a server upgrade, and it degrades to "no
        /// matching sources" rather than crashing. The old format carried no
        /// amounts, so no migration shim could recover `total_damage` regardless.
        #[serde(default)]
        source_amounts: Vec<(ObjectId, u32)>,
        /// CR 120.1: Total actual damage dealt to this player in this combat
        /// damage step — the sum of all `source_amounts` entries.
        #[serde(default)]
        total_damage: u32,
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
    /// CR 701.35a: A permanent was detained — until the detaining player's next
    /// turn it can't attack or block and its activated abilities can't be
    /// activated. Display-relevant for mana sources: detaining a mana dork
    /// makes its mana ability un-activatable.
    Detained {
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
    /// CR 103.1 / CR 706: The game-1 starting-player roll-off, emitted as one
    /// authoritative structured event so the contest can be rendered round by
    /// round (including tie rerolls) with no downstream re-derivation. `rounds`
    /// preserves the round boundaries the engine computes; `winner` is the
    /// engine's authoritative starting player (unique max of the final round, or
    /// the lowest-seat fallback when tied at the reroll cap). Replaces the prior
    /// flat per-roll `DieRolled` batch on the starting-player contest path; in-game
    /// die rolls still emit `DieRolled`.
    StartingPlayerContest {
        rounds: Vec<ContestRound>,
        winner: PlayerId,
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
    /// CR 702.170c-d: A card in exile became plotted for the specified player.
    BecomesPlotted {
        object_id: ObjectId,
        player_id: PlayerId,
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
    /// CR 701.51c: An Attraction was opened onto the battlefield.
    AttractionOpened {
        player_id: PlayerId,
        object_id: ObjectId,
    },
    /// CR 701.52: The active player rolled to visit their Attractions.
    AttractionsRolledToVisit {
        player_id: PlayerId,
        roll: u8,
    },
    /// CR 701.52a + CR 702.159a: A specific Attraction was visited this roll.
    AttractionVisited {
        player_id: PlayerId,
        roll: u8,
        attraction_id: ObjectId,
    },
    /// Avatar crossover: A firebending ability resolved and produced mana.
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

    /// CR 702.107a + CR 702.142b + CR 702.177a: A keyword ability was activated.
    /// Emitted alongside `AbilityActivated` when the activated ability has a recognized
    /// `ability_tag`. `is_mana_ability` is `true` only for exhaust mana abilities; it is
    /// always `false` for boast and outlast activations. Parameterized to avoid per-keyword
    /// variant proliferation (boast, exhaust, outlast share identical event structure).
    KeywordAbilityActivated {
        ability_tag: AbilityTag,
        player_id: PlayerId,
        source_id: ObjectId,
        is_mana_ability: bool,
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
    /// Sandbox audit log: a player with debug permission submitted a
    /// `GameAction::Debug(_)`. `description` is the engine-authored summary
    /// from `DebugAction::describe`; the FE renders it verbatim.
    DebugActionUsed {
        player_id: PlayerId,
        description: String,
    },
    /// Sandbox audit log: the host granted a player permission to submit
    /// `GameAction::Debug(_)`.
    DebugPermissionGranted {
        host: PlayerId,
        player_id: PlayerId,
    },
    /// Sandbox audit log: the host revoked a player's debug permission.
    DebugPermissionRevoked {
        host: PlayerId,
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
            source_amounts: vec![(ObjectId(10), 3), (ObjectId(11), 4)],
            total_damage: 7,
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
