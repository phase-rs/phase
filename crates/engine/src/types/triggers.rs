use std::convert::Infallible;
use std::fmt;
use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// All trigger modes from Forge's TriggerType enum.
/// Matched case-sensitively against Forge trigger mode strings.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum TriggerMode {
    // Zone changes
    ChangesZone,
    ChangesZoneAll,
    ChangesController,
    LeavesBattlefield,

    // Damage
    DamageDone,
    DamageDoneOnce,
    DamageAll,
    DamageDealtOnce,
    DamageDoneOnceByController,
    DamageReceived,
    DamagePreventedOnce,
    ExcessDamage,
    ExcessDamageAll,

    // Spells and abilities
    SpellCast,
    SpellCopy,
    SpellCastOrCopy,
    AbilityCast,
    AbilityResolves,
    AbilityTriggered,
    SpellAbilityCast,
    SpellAbilityCopy,
    Countered,

    // Combat -- attackers
    Attacks,
    AttackersDeclared,
    YouAttack,
    AttackersDeclaredOneTarget,
    AttackerBlocked,
    AttackerBlockedOnce,
    AttackerBlockedByCreature,
    AttackerUnblocked,
    AttackerUnblockedOnce,

    // Combat -- blockers
    Blocks,
    BlockersDeclared,
    BecomesBlocked,

    // Counters
    CounterAdded,
    CounterAddedOnce,
    CounterAddedAll,
    CounterPlayerAddedAll,
    CounterTypeAddedAll,
    CounterRemoved,
    CounterRemovedOnce,

    // Permanents
    Sacrificed,
    SacrificedOnce,
    Destroyed,
    Taps,
    TapsForMana,
    TapAll,
    Untaps,
    UntapAll,

    // Targeting
    BecomesTarget,
    BecomesTargetOnce,

    // Cards
    Drawn,
    Discarded,
    DiscardedAll,
    Milled,
    MilledOnce,
    MilledAll,
    Exiled,
    Revealed,
    Shuffled,

    // Life
    LifeGained,
    LifeLost,
    LifeLostAll,
    PayLife,
    PayCumulativeUpkeep,
    PayEcho,

    // Tokens
    TokenCreated,
    TokenCreatedOnce,

    // Face / transform
    TurnFaceUp,
    Transformed,

    // Phase / turn
    Phase,
    PhaseIn,
    PhaseOut,
    PhaseOutAll,
    TurnBegin,
    NewGame,

    // Monarch / initiative
    BecomeMonarch,
    TakesInitiative,

    // Game state
    LosesGame,

    // Triggered mechanics
    Championed,
    Exerted,
    Crewed,
    Saddled,
    Cycled,
    Evolved,
    Explored,
    Exploited,
    Enlisted,

    // Mana
    ManaAdded,
    ManaExpend,

    // Land
    LandPlayed,

    // Equipment / aura
    Attached,
    Unattach,

    // Adapt / amass / learn / venture
    Adapt,
    Foretell,
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

    // Day/night
    DayTimeChanges,

    // Class
    ClassLevelGained,

    // Copy
    Copied,
    ConjureAll,

    // Vote
    Vote,

    // Renown / monstrous
    BecomeRenowned,
    BecomeMonstrous,

    // Prowl / misc mechanics
    Proliferate,
    RingTemptsYou,

    // Surveil / scry
    Surveil,
    Scry,

    // Combat events
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
    Always,

    // Compound triggers
    /// "Whenever ~ enters or attacks" — fires on both ETB and attack events.
    EntersOrAttacks,
    /// "Whenever ~ attacks or blocks" — fires on both attack and block events.
    AttacksOrBlocks,

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
            "Cycled" => TriggerMode::Cycled,
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
            "Sacrificed" => TriggerMode::Sacrificed,
            "SacrificedOnce" => TriggerMode::SacrificedOnce,
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
            known_count >= 143,
            "Expected 143+ known trigger modes, got {known_count}"
        );
    }
}
