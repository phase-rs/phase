use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// CR 508.3a: Filter for attack target type in "attacks [a target]" triggers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttackTargetFilter {
    Player,
    Planeswalker,
    Battle,
}

/// All trigger modes from Forge's TriggerType enum (CR 603).
///
/// Triggered abilities have a trigger condition and an effect, written as
/// "[When/Whenever/At] [trigger condition], [effect]" (CR 603.1). When a game event
/// matches a trigger condition, the ability automatically triggers (CR 603.2) and is
/// placed on the stack the next time a player would receive priority (CR 603.3).
///
/// Matched case-sensitively against Forge trigger mode strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerMode {
    // Zone changes — CR 603.6: zone-change triggers look for objects in their new zone.
    /// CR 603.6a: Enters-the-battlefield and other zone-change triggers.
    ChangesZone,
    /// CR 603.6: Zone change affecting all objects matching a filter.
    ChangesZoneAll,
    /// CR 603.2e: "Becomes" trigger — fires when control changes, not while state persists.
    ChangesController,
    /// CR 603.6c: Leaves-the-battlefield trigger — fires when a permanent moves from battlefield.
    LeavesBattlefield,

    // Damage — CR 120 (Damage)
    /// CR 120.2: Trigger when a source deals damage.
    DamageDone,
    DamageDoneOnce,
    DamageAll,
    DamageDealtOnce,
    DamageDoneOnceByController,
    /// CR 120.2: Trigger when an object or player is dealt damage.
    DamageReceived,
    DamagePreventedOnce,
    ExcessDamage,
    ExcessDamageAll,

    // Spells and abilities — CR 601.2i: triggers when a spell is cast or put on the stack.
    /// CR 601.2i: Triggers when a spell becomes cast.
    SpellCast,
    SpellCopy,
    SpellCastOrCopy,
    AbilityCast,
    AbilityResolves,
    AbilityTriggered,
    SpellAbilityCast,
    SpellAbilityCopy,
    /// CR 603.2g: Triggers when a spell or ability is countered (event must actually occur).
    Countered,

    // Combat -- attackers (CR 508.3: trigger conditions for attackers being declared)
    /// CR 508.3a: "Whenever [a creature] attacks" — triggers when declared as attacker.
    Attacks,
    /// CR 508.3d: "Whenever [a player] attacks" — triggers when one or more creatures attack.
    AttackersDeclared,
    /// CR 508.3d: "Whenever you attack" — triggers for the attacking player.
    YouAttack,
    AttackersDeclaredOneTarget,
    /// CR 509.1h: Triggers when an attacking creature becomes blocked.
    AttackerBlocked,
    AttackerBlockedOnce,
    /// CR 509.1h: Triggers when a specific creature blocks the attacker.
    AttackerBlockedByCreature,
    AttackerUnblocked,
    AttackerUnblockedOnce,

    // Combat -- blockers (CR 509)
    /// CR 509.1h: "Whenever [a creature] blocks" — triggers when declared as blocker.
    Blocks,
    /// CR 509.4: Triggers after all blockers are declared.
    BlockersDeclared,
    /// CR 509.1h + CR 603.2e: "Becomes blocked" trigger.
    BecomesBlocked,

    // Counters — CR 122 (Counters)
    /// CR 122.6: Triggers when one or more counters are placed on a permanent or player.
    CounterAdded,
    CounterAddedOnce,
    CounterAddedAll,
    CounterPlayerAddedAll,
    CounterTypeAddedAll,
    CounterRemoved,
    CounterRemovedOnce,

    // Permanents
    /// CR 701.21: Triggers when a permanent is sacrificed.
    Sacrificed,
    SacrificedOnce,
    /// CR 701.8: Triggers when a permanent is destroyed.
    Destroyed,
    /// CR 701.26: Triggers when a permanent becomes tapped.
    Taps,
    /// CR 106.12a: Triggers when a permanent is tapped for mana.
    TapsForMana,
    TapAll,
    /// CR 701.26: Triggers when a permanent becomes untapped.
    Untaps,
    UntapAll,

    // Targeting — CR 115 (Targets)
    /// CR 603.2e: "Becomes the target" trigger — fires when a spell/ability targets an object.
    BecomesTarget,
    BecomesTargetOnce,

    // Cards
    /// CR 121.1: Triggers when a player draws a card.
    Drawn,
    /// CR 701.9: Triggers when a player discards a card.
    Discarded,
    DiscardedAll,
    /// CR 701.17: Triggers when cards are milled (put from library into graveyard).
    Milled,
    MilledOnce,
    MilledAll,
    Exiled,
    Revealed,
    /// CR 701.24: Triggers when a library is shuffled.
    Shuffled,

    // Life — CR 119 (Life)
    /// CR 119.4: Triggers when a player gains life.
    LifeGained,
    /// CR 119.3: Triggers when a player loses life.
    LifeLost,
    LifeLostAll,
    PayLife,
    /// CR 702.24: Cumulative upkeep trigger.
    PayCumulativeUpkeep,
    /// CR 702.30: Echo trigger.
    PayEcho,

    // Tokens — CR 111 (Tokens)
    /// CR 111.1: Triggers when a token is created.
    TokenCreated,
    TokenCreatedOnce,

    // Face / transform
    /// CR 702.37e: Triggers when a face-down permanent is turned face up (morph/manifest/cloak).
    TurnFaceUp,
    /// CR 701.27: Triggers when a permanent transforms.
    Transformed,

    // Phase / turn — CR 603.2b: "at the beginning of" phase/step triggers.
    /// CR 603.2b: "At the beginning of [phase/step]" — triggers at phase start.
    Phase,
    /// CR 702.26: Triggers when a phased-out permanent phases in.
    PhaseIn,
    /// CR 702.26: Triggers when a permanent phases out.
    PhaseOut,
    PhaseOutAll,
    /// CR 603.2b: "At the beginning of [a player's] turn" trigger.
    TurnBegin,
    NewGame,

    // Monarch / initiative
    /// CR 724: Triggers when a player becomes the monarch.
    BecomeMonarch,
    /// CR 725: Triggers when a player takes the initiative.
    TakesInitiative,

    // Game state
    /// CR 104.3a: Triggers when a player loses the game.
    LosesGame,

    // Triggered mechanics
    /// CR 702.72: Champion trigger.
    Championed,
    /// CR 701.43: Triggers when a creature is exerted.
    Exerted,
    /// CR 702.122: Triggers when a Vehicle becomes crewed.
    Crewed,
    /// CR 702.122: Actor-side crew trigger — fires when this permanent crews a Vehicle.
    Crews,
    /// CR 702.171b: Triggers when a creature becomes saddled.
    Saddled,
    /// CR 702.171c: Actor-side saddle trigger — fires when this permanent saddles a Mount.
    /// Reserved — no cards today print this without the compound form.
    Saddles,
    /// CR 702.122 + CR 702.171c: Compound actor-side trigger — fires on either
    /// saddling a Mount OR crewing a Vehicle.
    SaddlesOrCrews,
    /// CR 702.29: Triggers when a card is cycled.
    Cycled,
    /// CR 702.29d: Triggers when a card is cycled or discarded.
    /// Fires on either event but only once per cycling action.
    CycledOrDiscarded,
    /// CR 702.49a: Triggers when a player activates a ninjutsu-family ability.
    NinjutsuActivated,
    /// CR 702.100: Evolve trigger — when a creature enters with greater power/toughness.
    Evolved,
    /// CR 701.44: Triggers when a creature explores.
    Explored,
    /// CR 702.110: Exploit trigger — when a creature exploits another creature.
    Exploited,
    /// CR 702.154: Triggers when a creature becomes enlisted.
    Enlisted,

    // Mana
    /// CR 106.4: Triggers when mana is added to a player's mana pool.
    ManaAdded,
    ManaExpend,

    // Land
    /// CR 305.1 + CR 505.6b: Triggers when a land is played.
    LandPlayed,

    // Equipment / aura — CR 701.3 (Attach)
    /// CR 701.3: Triggers when an Aura, Equipment, or Fortification becomes attached.
    Attached,
    /// CR 701.3: Triggers when an Equipment or Aura becomes unattached.
    Unattach,

    // Adapt / amass / learn / venture
    /// CR 701.46: Triggers when a creature adapts.
    Adapt,
    /// CR 702.143: Triggers when a card is foretold.
    Foretell,
    /// CR 701.16: Triggers when a player investigates.
    Investigated,

    // Dungeon
    DungeonCompleted,
    RoomEntered,

    // Planar
    PlanarDice,
    PlaneswalkedFrom,
    PlaneswalkedTo,
    ChaosEnsues,

    // Dice / coin
    RolledDie,
    RolledDieOnce,
    FlippedCoin,
    Clashed,

    // Day/night — CR 730 (Day and Night)
    /// CR 730: Triggers when it becomes day or night.
    DayTimeChanges,

    // Class
    ClassLevelGained,

    // Copy
    Copied,
    ConjureAll,

    // Vote
    Vote,

    // Renown / monstrous
    /// CR 702.112: Triggers when a creature becomes renowned.
    BecomeRenowned,
    /// CR 702.99: Triggers when a creature becomes monstrous.
    BecomeMonstrous,

    // Prowl / misc mechanics
    /// CR 701.34: Triggers when a player proliferates.
    Proliferate,
    RingTemptsYou,

    // Surveil / scry
    /// CR 701.25: Triggers when a player surveils.
    Surveil,
    /// CR 701.22: Triggers when a player scries.
    Scry,
    /// General typed player-action trigger for joined action lists.
    PlayerPerformedAction,

    // Combat events
    /// CR 701.14: Triggers when creatures fight.
    Fight,
    FightOnce,

    // New mechanics (recent sets)
    Abandoned,
    CaseSolved,
    ClaimPrize,
    CollectEvidence,
    CommitCrime,
    CrankContraption,
    Devoured,
    Discover,
    Forage,
    FullyUnlock,
    GiveGift,
    ManifestDread,
    Mentored,
    Mutates,
    SearchedLibrary,
    SeekAll,
    SetInMotion,
    Specializes,
    Stationed,
    Trains,
    UnlockDoor,
    VisitAttraction,
    BecomesCrewed,
    BecomesPlotted,
    BecomesSaddled,
    Immediate,
    /// CR 603.1: "Always" — a special trigger mode representing a continuous trigger condition.
    Always,

    // Compound triggers
    /// "Whenever ~ enters or attacks" — fires on both ETB (CR 603.6a) and attack (CR 508.3a) events.
    EntersOrAttacks,
    /// "Whenever ~ attacks or blocks" — fires on both attack (CR 508.3a) and block (CR 509.1h) events.
    AttacksOrBlocks,

    /// CR 603.8: State trigger — fires when a game-state condition becomes true, rather than
    /// in response to an event. Checked whenever a player would receive priority.
    /// The engine tracks whether the trigger is already on the stack to prevent re-triggering
    /// until the ability has resolved, been countered, or otherwise left the stack.
    StateCondition,

    // Elemental bending
    Airbend,
    Earthbend,
    Firebend,
    Waterbend,
    ElementalBend,

    /// Fallback for unrecognized trigger mode strings.
    Unknown(String),
}

impl FromStr for TriggerMode {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Case-sensitive match on Forge trigger mode strings
        let mode = match s {
            "Abandoned" => TriggerMode::Abandoned,
            "AbilityCast" => TriggerMode::AbilityCast,
            "AbilityResolves" => TriggerMode::AbilityResolves,
            "AbilityTriggered" => TriggerMode::AbilityTriggered,
            "Adapt" => TriggerMode::Adapt,
            "Airbend" => TriggerMode::Airbend,
            "Always" => TriggerMode::Always,
            "Attached" => TriggerMode::Attached,
            "AttackerBlocked" => TriggerMode::AttackerBlocked,
            "AttackerBlockedOnce" => TriggerMode::AttackerBlockedOnce,
            "AttackerBlockedByCreature" => TriggerMode::AttackerBlockedByCreature,
            "AttackersDeclared" => TriggerMode::AttackersDeclared,
            "AttackersDeclaredOneTarget" => TriggerMode::AttackersDeclaredOneTarget,
            "AttackerUnblocked" => TriggerMode::AttackerUnblocked,
            "AttackerUnblockedOnce" => TriggerMode::AttackerUnblockedOnce,
            "Attacks" => TriggerMode::Attacks,
            "BecomeMonarch" => TriggerMode::BecomeMonarch,
            "BecomeMonstrous" => TriggerMode::BecomeMonstrous,
            "BecomeRenowned" => TriggerMode::BecomeRenowned,
            "BecomesCrewed" => TriggerMode::BecomesCrewed,
            "BecomesPlotted" => TriggerMode::BecomesPlotted,
            "BecomesSaddled" => TriggerMode::BecomesSaddled,
            "BecomesBlocked" => TriggerMode::BecomesBlocked,
            "BecomesTarget" => TriggerMode::BecomesTarget,
            "BecomesTargetOnce" => TriggerMode::BecomesTargetOnce,
            "BlockersDeclared" => TriggerMode::BlockersDeclared,
            "Blocks" => TriggerMode::Blocks,
            "CaseSolved" => TriggerMode::CaseSolved,
            "Championed" => TriggerMode::Championed,
            "ChangesController" => TriggerMode::ChangesController,
            "ChangesZone" => TriggerMode::ChangesZone,
            "ChangesZoneAll" => TriggerMode::ChangesZoneAll,
            "ChaosEnsues" => TriggerMode::ChaosEnsues,
            "ClaimPrize" => TriggerMode::ClaimPrize,
            "Clashed" => TriggerMode::Clashed,
            "ClassLevelGained" => TriggerMode::ClassLevelGained,
            "CommitCrime" => TriggerMode::CommitCrime,
            "ConjureAll" => TriggerMode::ConjureAll,
            "CollectEvidence" => TriggerMode::CollectEvidence,
            "CounterAdded" => TriggerMode::CounterAdded,
            "CounterAddedOnce" => TriggerMode::CounterAddedOnce,
            "CounterPlayerAddedAll" => TriggerMode::CounterPlayerAddedAll,
            "CounterTypeAddedAll" => TriggerMode::CounterTypeAddedAll,
            "CounterAddedAll" => TriggerMode::CounterAddedAll,
            "Countered" => TriggerMode::Countered,
            "CounterRemoved" => TriggerMode::CounterRemoved,
            "CounterRemovedOnce" => TriggerMode::CounterRemovedOnce,
            "CrankContraption" => TriggerMode::CrankContraption,
            "Crewed" => TriggerMode::Crewed,
            "Crews" => TriggerMode::Crews,
            "Cycled" => TriggerMode::Cycled,
            "CycledOrDiscarded" => TriggerMode::CycledOrDiscarded,
            "DamageAll" => TriggerMode::DamageAll,
            "DamageDealtOnce" => TriggerMode::DamageDealtOnce,
            "DamageDone" => TriggerMode::DamageDone,
            "DamageDoneOnce" => TriggerMode::DamageDoneOnce,
            "DamageDoneOnceByController" => TriggerMode::DamageDoneOnceByController,
            "DamageReceived" => TriggerMode::DamageReceived,
            "DamagePreventedOnce" => TriggerMode::DamagePreventedOnce,
            "DayTimeChanges" => TriggerMode::DayTimeChanges,
            "Destroyed" => TriggerMode::Destroyed,
            "Devoured" => TriggerMode::Devoured,
            "Discarded" => TriggerMode::Discarded,
            "DiscardedAll" => TriggerMode::DiscardedAll,
            "Discover" => TriggerMode::Discover,
            "Drawn" => TriggerMode::Drawn,
            "DungeonCompleted" => TriggerMode::DungeonCompleted,
            "Earthbend" => TriggerMode::Earthbend,
            "ElementalBend" => TriggerMode::ElementalBend,
            "Enlisted" => TriggerMode::Enlisted,
            "AttacksOrBlocks" => TriggerMode::AttacksOrBlocks,
            "EntersOrAttacks" => TriggerMode::EntersOrAttacks,
            "Evolved" => TriggerMode::Evolved,
            "ExcessDamage" => TriggerMode::ExcessDamage,
            "ExcessDamageAll" => TriggerMode::ExcessDamageAll,
            "Exerted" => TriggerMode::Exerted,
            "Exiled" => TriggerMode::Exiled,
            "Exploited" => TriggerMode::Exploited,
            "Explores" => TriggerMode::Explored,
            "Fight" => TriggerMode::Fight,
            "FightOnce" => TriggerMode::FightOnce,
            "Firebend" => TriggerMode::Firebend,
            "FlippedCoin" => TriggerMode::FlippedCoin,
            "Forage" => TriggerMode::Forage,
            "Foretell" => TriggerMode::Foretell,
            "FullyUnlock" => TriggerMode::FullyUnlock,
            "GiveGift" => TriggerMode::GiveGift,
            "Immediate" => TriggerMode::Immediate,
            "Investigated" => TriggerMode::Investigated,
            "LandPlayed" => TriggerMode::LandPlayed,
            "LeavesBattlefield" => TriggerMode::LeavesBattlefield,
            "LifeGained" => TriggerMode::LifeGained,
            "LifeLost" => TriggerMode::LifeLost,
            "LifeLostAll" => TriggerMode::LifeLostAll,
            "LosesGame" => TriggerMode::LosesGame,
            "ManaAdded" => TriggerMode::ManaAdded,
            "ManaExpend" => TriggerMode::ManaExpend,
            "ManifestDread" => TriggerMode::ManifestDread,
            "Mentored" => TriggerMode::Mentored,
            "Milled" => TriggerMode::Milled,
            "MilledOnce" => TriggerMode::MilledOnce,
            "MilledAll" => TriggerMode::MilledAll,
            "Mutates" => TriggerMode::Mutates,
            "NewGame" => TriggerMode::NewGame,
            "NinjutsuActivated" => TriggerMode::NinjutsuActivated,
            "PayCumulativeUpkeep" => TriggerMode::PayCumulativeUpkeep,
            "PayEcho" => TriggerMode::PayEcho,
            "PayLife" => TriggerMode::PayLife,
            "Phase" => TriggerMode::Phase,
            "PhaseIn" => TriggerMode::PhaseIn,
            "PhaseOut" => TriggerMode::PhaseOut,
            "PhaseOutAll" => TriggerMode::PhaseOutAll,
            "PlanarDice" => TriggerMode::PlanarDice,
            "PlaneswalkedFrom" => TriggerMode::PlaneswalkedFrom,
            "PlaneswalkedTo" => TriggerMode::PlaneswalkedTo,
            "Proliferate" => TriggerMode::Proliferate,
            "Revealed" => TriggerMode::Revealed,
            "RingTemptsYou" => TriggerMode::RingTemptsYou,
            "RolledDie" => TriggerMode::RolledDie,
            "RolledDieOnce" => TriggerMode::RolledDieOnce,
            "RoomEntered" => TriggerMode::RoomEntered,
            "Saddled" => TriggerMode::Saddled,
            "Saddles" => TriggerMode::Saddles,
            "SaddlesOrCrews" => TriggerMode::SaddlesOrCrews,
            "Sacrificed" => TriggerMode::Sacrificed,
            "SacrificedOnce" => TriggerMode::SacrificedOnce,
            "PlayerPerformedAction" => TriggerMode::PlayerPerformedAction,
            "Scry" => TriggerMode::Scry,
            "SearchedLibrary" => TriggerMode::SearchedLibrary,
            "SeekAll" => TriggerMode::SeekAll,
            "SetInMotion" => TriggerMode::SetInMotion,
            "Shuffled" => TriggerMode::Shuffled,
            "Specializes" => TriggerMode::Specializes,
            "SpellAbilityCast" => TriggerMode::SpellAbilityCast,
            "SpellAbilityCopy" => TriggerMode::SpellAbilityCopy,
            "SpellCast" => TriggerMode::SpellCast,
            "SpellCastOrCopy" => TriggerMode::SpellCastOrCopy,
            "SpellCopy" => TriggerMode::SpellCopy,
            "StateCondition" => TriggerMode::StateCondition,
            "Stationed" => TriggerMode::Stationed,
            "Surveil" => TriggerMode::Surveil,
            "TakesInitiative" => TriggerMode::TakesInitiative,
            "TapAll" => TriggerMode::TapAll,
            "Taps" => TriggerMode::Taps,
            "TapsForMana" => TriggerMode::TapsForMana,
            "TokenCreated" => TriggerMode::TokenCreated,
            "TokenCreatedOnce" => TriggerMode::TokenCreatedOnce,
            "Trains" => TriggerMode::Trains,
            "Transformed" => TriggerMode::Transformed,
            "TurnBegin" => TriggerMode::TurnBegin,
            "TurnFaceUp" => TriggerMode::TurnFaceUp,
            "Unattach" => TriggerMode::Unattach,
            "UnlockDoor" => TriggerMode::UnlockDoor,
            "UntapAll" => TriggerMode::UntapAll,
            "Untaps" => TriggerMode::Untaps,
            "VisitAttraction" => TriggerMode::VisitAttraction,
            "Vote" => TriggerMode::Vote,
            "YouAttack" => TriggerMode::YouAttack,
            "Waterbend" => TriggerMode::Waterbend,
            _ => TriggerMode::Unknown(s.to_string()),
        };
        Ok(mode)
    }
}

impl fmt::Display for TriggerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TriggerMode::Unknown(s) => write!(f, "{s}"),
            other => {
                // Use Debug formatting but strip the enum prefix for known variants.
                // Known variants serialize as their name (e.g. ChangesZone -> "ChangesZone").
                write!(f, "{other:?}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_trigger_modes() {
        assert_eq!(
            TriggerMode::from_str("ChangesZone").unwrap(),
            TriggerMode::ChangesZone
        );
        assert_eq!(
            TriggerMode::from_str("DamageDone").unwrap(),
            TriggerMode::DamageDone
        );
        assert_eq!(
            TriggerMode::from_str("SpellCast").unwrap(),
            TriggerMode::SpellCast
        );
        assert_eq!(
            TriggerMode::from_str("Attacks").unwrap(),
            TriggerMode::Attacks
        );
        assert_eq!(
            TriggerMode::from_str("Blocks").unwrap(),
            TriggerMode::Blocks
        );
        assert_eq!(
            TriggerMode::from_str("AttackerBlocked").unwrap(),
            TriggerMode::AttackerBlocked
        );
        assert_eq!(
            TriggerMode::from_str("LifeGained").unwrap(),
            TriggerMode::LifeGained
        );
        assert_eq!(
            TriggerMode::from_str("TokenCreated").unwrap(),
            TriggerMode::TokenCreated
        );
    }

    #[test]
    fn parse_unknown_trigger_mode() {
        assert_eq!(
            TriggerMode::from_str("NotARealTrigger").unwrap(),
            TriggerMode::Unknown("NotARealTrigger".to_string())
        );
    }

    #[test]
    fn trigger_mode_case_sensitive() {
        // Forge uses CamelCase -- lowercase should be Unknown
        assert_eq!(
            TriggerMode::from_str("changeszone").unwrap(),
            TriggerMode::Unknown("changeszone".to_string())
        );
    }

    #[test]
    fn trigger_mode_serialization_roundtrip() {
        let modes = vec![
            TriggerMode::ChangesZone,
            TriggerMode::DamageDone,
            TriggerMode::Unknown("Custom".to_string()),
        ];
        let json = serde_json::to_string(&modes).unwrap();
        let deserialized: Vec<TriggerMode> = serde_json::from_str(&json).unwrap();
        assert_eq!(modes, deserialized);
    }

    #[test]
    fn trigger_mode_hashable() {
        use std::collections::HashSet;
        let mut set = HashSet::new();
        set.insert(TriggerMode::ChangesZone);
        set.insert(TriggerMode::DamageDone);
        set.insert(TriggerMode::ChangesZone); // duplicate
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn trigger_mode_count_at_least_141() {
        let modes = [
            "Abandoned",
            "AbilityCast",
            "AbilityResolves",
            "AbilityTriggered",
            "Adapt",
            "Airbend",
            "Always",
            "Attached",
            "AttackerBlocked",
            "AttackerBlockedOnce",
            "AttackerBlockedByCreature",
            "AttackersDeclared",
            "AttackersDeclaredOneTarget",
            "AttackerUnblocked",
            "AttackerUnblockedOnce",
            "Attacks",
            "AttacksOrBlocks",
            "BecomesBlocked",
            "BecomeMonarch",
            "BecomeMonstrous",
            "BecomeRenowned",
            "BecomesCrewed",
            "BecomesPlotted",
            "BecomesSaddled",
            "BecomesTarget",
            "BecomesTargetOnce",
            "BlockersDeclared",
            "Blocks",
            "CaseSolved",
            "Championed",
            "ChangesController",
            "ChangesZone",
            "ChangesZoneAll",
            "ChaosEnsues",
            "ClaimPrize",
            "Clashed",
            "ClassLevelGained",
            "CommitCrime",
            "ConjureAll",
            "CollectEvidence",
            "CounterAdded",
            "CounterAddedOnce",
            "CounterPlayerAddedAll",
            "CounterTypeAddedAll",
            "CounterAddedAll",
            "Countered",
            "CounterRemoved",
            "CounterRemovedOnce",
            "CrankContraption",
            "Crewed",
            "Cycled",
            "CycledOrDiscarded",
            "DamageAll",
            "DamageDealtOnce",
            "DamageDone",
            "DamageDoneOnce",
            "DamageDoneOnceByController",
            "DamageReceived",
            "DamagePreventedOnce",
            "DayTimeChanges",
            "Destroyed",
            "Devoured",
            "Discarded",
            "DiscardedAll",
            "Discover",
            "Drawn",
            "DungeonCompleted",
            "Earthbend",
            "ElementalBend",
            "Enlisted",
            "EntersOrAttacks",
            "Evolved",
            "ExcessDamage",
            "ExcessDamageAll",
            "Exerted",
            "Exiled",
            "Exploited",
            "Explores",
            "Fight",
            "FightOnce",
            "Firebend",
            "FlippedCoin",
            "Forage",
            "Foretell",
            "FullyUnlock",
            "GiveGift",
            "Immediate",
            "Investigated",
            "LandPlayed",
            "LeavesBattlefield",
            "LifeGained",
            "LifeLost",
            "LifeLostAll",
            "LosesGame",
            "ManaAdded",
            "ManaExpend",
            "ManifestDread",
            "Mentored",
            "Milled",
            "MilledOnce",
            "MilledAll",
            "Mutates",
            "NewGame",
            "NinjutsuActivated",
            "PayCumulativeUpkeep",
            "PayEcho",
            "PayLife",
            "Phase",
            "PhaseIn",
            "PhaseOut",
            "PhaseOutAll",
            "PlanarDice",
            "PlaneswalkedFrom",
            "PlaneswalkedTo",
            "Proliferate",
            "Revealed",
            "RingTemptsYou",
            "RolledDie",
            "RolledDieOnce",
            "RoomEntered",
            "Saddled",
            "Sacrificed",
            "SacrificedOnce",
            "PlayerPerformedAction",
            "Scry",
            "SearchedLibrary",
            "SeekAll",
            "SetInMotion",
            "Shuffled",
            "Specializes",
            "SpellAbilityCast",
            "SpellAbilityCopy",
            "SpellCast",
            "SpellCastOrCopy",
            "SpellCopy",
            "Stationed",
            "Surveil",
            "TakesInitiative",
            "TapAll",
            "Taps",
            "TapsForMana",
            "TokenCreated",
            "TokenCreatedOnce",
            "Trains",
            "Transformed",
            "TurnBegin",
            "TurnFaceUp",
            "Unattach",
            "UnlockDoor",
            "UntapAll",
            "Untaps",
            "VisitAttraction",
            "Vote",
            "Waterbend",
            "YouAttack",
        ];

        let mut known_count = 0;
        for mode in &modes {
            let parsed = TriggerMode::from_str(mode).unwrap();
            if !matches!(parsed, TriggerMode::Unknown(_)) {
                known_count += 1;
            }
        }
        assert!(
            known_count >= 145,
            "Expected 145+ known trigger modes, got {known_count}"
        );
    }
}
