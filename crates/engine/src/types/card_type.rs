use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// CR 205.4: Supertypes — Legendary, Basic, Snow, World, Ongoing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Supertype {
    Legendary,
    Basic,
    Snow,
    World,
    Ongoing,
}

impl FromStr for Supertype {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Legendary" => Ok(Supertype::Legendary),
            "Basic" => Ok(Supertype::Basic),
            "Snow" => Ok(Supertype::Snow),
            "World" => Ok(Supertype::World),
            "Ongoing" => Ok(Supertype::Ongoing),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Supertype {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Supertype::Legendary => write!(f, "Legendary"),
            Supertype::Basic => write!(f, "Basic"),
            Supertype::Snow => write!(f, "Snow"),
            Supertype::World => write!(f, "World"),
            Supertype::Ongoing => write!(f, "Ongoing"),
        }
    }
}

/// CR 205.2a: Card types — the seven main types plus additional types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CoreType {
    /// CR 301: Artifacts — permanents cast at sorcery speed, with subtypes Equipment, Vehicle, etc.
    Artifact,
    Creature,
    Enchantment,
    /// CR 304: Instants — spells castable any time a player has priority.
    Instant,
    Land,
    /// CR 306: Planeswalkers — permanents with loyalty counters and loyalty abilities.
    Planeswalker,
    Sorcery,
    /// CR 308.3: Legacy "tribal" type — errata'd to Kindred in current rules.
    Tribal,
    /// CR 310: Battles — permanents with defense counters that can be attacked.
    Battle,
    /// CR 308: Kindreds — cards that share creature subtypes with another card type.
    Kindred,
    /// CR 309: Dungeons — nontraditional cards that exist in the command zone.
    Dungeon,
}

impl FromStr for CoreType {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Artifact" => Ok(CoreType::Artifact),
            "Creature" => Ok(CoreType::Creature),
            "Enchantment" => Ok(CoreType::Enchantment),
            "Instant" => Ok(CoreType::Instant),
            "Land" => Ok(CoreType::Land),
            "Planeswalker" => Ok(CoreType::Planeswalker),
            "Sorcery" => Ok(CoreType::Sorcery),
            "Tribal" => Ok(CoreType::Tribal),
            "Battle" => Ok(CoreType::Battle),
            "Kindred" => Ok(CoreType::Kindred),
            "Dungeon" => Ok(CoreType::Dungeon),
            _ => Err(()),
        }
    }
}

impl fmt::Display for CoreType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreType::Artifact => write!(f, "Artifact"),
            CoreType::Creature => write!(f, "Creature"),
            CoreType::Enchantment => write!(f, "Enchantment"),
            CoreType::Instant => write!(f, "Instant"),
            CoreType::Land => write!(f, "Land"),
            CoreType::Planeswalker => write!(f, "Planeswalker"),
            CoreType::Sorcery => write!(f, "Sorcery"),
            CoreType::Tribal => write!(f, "Tribal"),
            CoreType::Battle => write!(f, "Battle"),
            CoreType::Kindred => write!(f, "Kindred"),
            CoreType::Dungeon => write!(f, "Dungeon"),
        }
    }
}

impl CoreType {
    /// CR 110.4: Returns `true` if this core type is one of the six permanent
    /// types — artifact, battle, creature, enchantment, land, planeswalker.
    /// Instants and sorceries cannot enter the battlefield, so they are not
    /// permanent types. Kindred/Tribal/Dungeon are non-permanent supplemental
    /// types and also return false.
    pub const fn is_permanent_type(self) -> bool {
        matches!(
            self,
            CoreType::Artifact
                | CoreType::Battle
                | CoreType::Creature
                | CoreType::Enchantment
                | CoreType::Land
                | CoreType::Planeswalker
        )
    }

    /// CR 110.4: Canonical ordering of the six permanent types.
    ///
    /// Used by per-permanent-type cast trackers (e.g., Muldrotha) to give a
    /// deterministic auto-pick order when a multi-type card has more than one
    /// available slot. The order matches CR 110.4's enumeration.
    pub const PERMANENT_TYPES: [CoreType; 6] = [
        CoreType::Artifact,
        CoreType::Battle,
        CoreType::Creature,
        CoreType::Enchantment,
        CoreType::Land,
        CoreType::Planeswalker,
    ];
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardType {
    pub supertypes: Vec<Supertype>,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
}

/// CR 205.3i: Returns true if the given string is a land subtype.
/// Used by `SetBasicLandType` to remove only land subtypes while preserving
/// non-land subtypes (e.g., creature subtypes on Land Creatures like Dryad Arbor).
pub fn is_land_subtype(s: &str) -> bool {
    matches!(
        s,
        "Cave"
            | "Desert"
            | "Forest"
            | "Gate"
            | "Island"
            | "Lair"
            | "Locus"
            | "Mine"
            | "Mountain"
            | "Plains"
            | "Planet"
            | "Power-Plant"
            | "Sphere"
            | "Swamp"
            | "Tower"
            | "Town"
            | "Urza's"
    )
}
