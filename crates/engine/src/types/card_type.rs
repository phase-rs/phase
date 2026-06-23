use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// CR 205.4: Supertypes — Legendary, Basic, Snow, World, Ongoing, plus
/// supplemental set-specific supertypes such as Host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Supertype {
    Legendary,
    Basic,
    Snow,
    World,
    Ongoing,
    Host,
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
            "Host" => Ok(Supertype::Host),
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
            Supertype::Host => write!(f, "Host"),
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
    /// CR 311: Planes — nontraditional cards used in the Planechase variant that
    /// remain face up in the command zone (CR 311.2).
    Plane,
    /// CR 312: Phenomena — nontraditional cards used in the Planechase variant
    /// that are encountered from the planar deck (CR 312.2).
    Phenomenon,
    /// CR 314: Schemes — nontraditional cards used in the Archenemy variant that
    /// remain in the command zone (CR 314.2) and are set in motion from the
    /// scheme deck (CR 904.9).
    Scheme,
    /// CR 905: Conspiracies — nontraditional cards used in the Conspiracy Draft
    /// variant that exist only in the command zone (CR 905.4), where a face-up
    /// conspiracy applies its abilities and a hidden-agenda conspiracy
    /// (CR 905.4a + CR 702.106) starts face down.
    Conspiracy,
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
            "Plane" => Ok(CoreType::Plane),
            "Phenomenon" => Ok(CoreType::Phenomenon),
            "Scheme" => Ok(CoreType::Scheme),
            "Conspiracy" => Ok(CoreType::Conspiracy),
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
            CoreType::Plane => write!(f, "Plane"),
            CoreType::Phenomenon => write!(f, "Phenomenon"),
            CoreType::Scheme => write!(f, "Scheme"),
            CoreType::Conspiracy => write!(f, "Conspiracy"),
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

    /// CR 205.2a: The seven card types offered by a "choose a card type"
    /// prompt (`ChoiceType::CardType`) — Battle, Kindred, Dungeon, and the
    /// other supplemental types are never offered. Mirrors the `CARD_TYPES`
    /// display-name list in `game/effects/choose.rs`; restricted enumerations
    /// ("choose artifact, enchantment, instant, sorcery, or planeswalker")
    /// compute their `excluded` set as the complement of this list.
    pub const CHOOSABLE_TYPES: [CoreType; 7] = [
        CoreType::Artifact,
        CoreType::Creature,
        CoreType::Enchantment,
        CoreType::Instant,
        CoreType::Land,
        CoreType::Planeswalker,
        CoreType::Sorcery,
    ];

    /// CR 702.16a: The lowercase singular noun used to express "protection from
    /// [card type]" — e.g. "protection from creatures". Returns `None` for the
    /// supplemental types (Tribal/Kindred/Dungeon/Battle) which are never offered
    /// as a chosen card type (`CARD_TYPES` in `choose.rs` offers only the seven
    /// main types); callers `continue`/skip on `None`.
    pub const fn protection_quality_str(self) -> Option<&'static str> {
        match self {
            CoreType::Artifact => Some("artifact"),
            CoreType::Creature => Some("creature"),
            CoreType::Enchantment => Some("enchantment"),
            CoreType::Instant => Some("instant"),
            CoreType::Sorcery => Some("sorcery"),
            CoreType::Planeswalker => Some("planeswalker"),
            CoreType::Land => Some("land"),
            CoreType::Tribal
            | CoreType::Battle
            | CoreType::Kindred
            | CoreType::Dungeon
            | CoreType::Plane
            | CoreType::Phenomenon
            | CoreType::Scheme
            | CoreType::Conspiracy => None,
        }
    }
}

/// CR 205.3: The classification of subtype sets. Each card type has its own
/// correlated subtype pool — creature types, land types, artifact types, etc.
/// Used by `ContinuousModification::RemoveAllSubtypes` to express "loses all
/// other creature types" without enumerating individual subtypes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SubtypeSet {
    /// CR 205.3m: Creature subtypes (creature types).
    Creature,
    /// CR 205.3i: Land subtypes (land types).
    Land,
    /// CR 205.3g: Artifact subtypes.
    Artifact,
    /// CR 205.3h: Enchantment subtypes.
    Enchantment,
    /// CR 205.3j: Planeswalker subtypes.
    Planeswalker,
    /// CR 205.3k: Spell subtypes (instant/sorcery).
    Spell,
    /// CR 205.3q: Battle subtypes.
    Battle,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardType {
    pub supertypes: Vec<Supertype>,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
}

pub const LAND_SUBTYPES: &[&str] = &[
    "Cave",
    "Desert",
    "Forest",
    "Gate",
    "Island",
    "Lair",
    "Locus",
    "Mine",
    "Mountain",
    "Plains",
    "Planet",
    "Power-Plant",
    "Sphere",
    "Swamp",
    "Tower",
    "Town",
    "Urza's",
];

pub const ARTIFACT_SUBTYPES: &[&str] = &[
    "Attraction",
    "Blood",
    "Bobblehead",
    "Book",
    "Clue",
    "Contraption",
    "Equipment",
    "Food",
    "Fortification",
    "Gold",
    "Incubator",
    "Infinity",
    "Junk",
    "Lander",
    "Map",
    "Mutagen",
    "Powerstone",
    "Spacecraft",
    "Stone",
    "Treasure",
    "Vehicle",
];

pub const ENCHANTMENT_SUBTYPES: &[&str] = &[
    "Aura",
    "Background",
    "Cartouche",
    "Case",
    "Class",
    "Curse",
    "Role",
    "Room",
    "Rune",
    "Saga",
    "Shard",
    "Shrine",
];

pub const SPELL_SUBTYPES: &[&str] = &["Adventure", "Arcane", "Lesson", "Omen", "Trap"];

pub const BATTLE_SUBTYPES: &[&str] = &["Siege"];

pub const PLANESWALKER_SUBTYPES: &[&str] = &[
    "Ajani",
    "Aminatou",
    "Angrath",
    "Arlinn",
    "Ashiok",
    "Bahamut",
    "Basri",
    "Bolas",
    "Calix",
    "Chandra",
    "Comet",
    "Dack",
    "Dakkon",
    "Daretti",
    "Davriel",
    "Dellian",
    "Dihada",
    "Domri",
    "Dovin",
    "Ellywick",
    "Elminster",
    "Elspeth",
    "Estrid",
    "Freyalise",
    "Garruk",
    "Gideon",
    "Grist",
    "Guff",
    "Huatli",
    "Jace",
    "Jared",
    "Jaya",
    "Jeska",
    "Kaito",
    "Karn",
    "Kasmina",
    "Kaya",
    "Kiora",
    "Koth",
    "Liliana",
    "Lolth",
    "Lukka",
    "Minsc",
    "Mordenkainen",
    "Nahiri",
    "Narset",
    "Niko",
    "Nissa",
    "Nixilis",
    "Oko",
    "Quintorius",
    "Ral",
    "Rowan",
    "Saheeli",
    "Samut",
    "Sarkhan",
    "Serra",
    "Sivitri",
    "Sorin",
    "Szat",
    "Tamiyo",
    "Tasha",
    "Teferi",
    "Teyo",
    "Tezzeret",
    "Tibalt",
    "Tyvar",
    "Ugin",
    "Urza",
    "Venser",
    "Vivien",
    "Vraska",
    "Vronos",
    "Will",
    "Windgrace",
    "Wrenn",
    "Xenagos",
    "Yanggu",
    "Yanling",
    "Zariel",
];

pub fn fixed_noncreature_subtypes() -> impl Iterator<Item = &'static str> {
    LAND_SUBTYPES
        .iter()
        .chain(ARTIFACT_SUBTYPES)
        .chain(ENCHANTMENT_SUBTYPES)
        .chain(SPELL_SUBTYPES)
        .chain(BATTLE_SUBTYPES)
        .chain(PLANESWALKER_SUBTYPES)
        .copied()
}

/// CR 205.3i: Returns true if the given string is a land subtype.
/// Used by `SetBasicLandType` to remove only land subtypes while preserving
/// non-land subtypes (e.g., creature subtypes on Land Creatures like Dryad Arbor).
pub fn is_land_subtype(s: &str) -> bool {
    LAND_SUBTYPES.contains(&s)
}

/// CR 205.3: Return the noncreature subtype set for subtypes whose membership
/// is fixed by the card-type rules. Creature subtypes are intentionally
/// excluded because the runtime card database owns that list.
pub fn noncreature_subtype_set(s: &str) -> Option<SubtypeSet> {
    match s {
        s if LAND_SUBTYPES.contains(&s) => Some(SubtypeSet::Land),
        s if ARTIFACT_SUBTYPES.contains(&s) => Some(SubtypeSet::Artifact),
        s if ENCHANTMENT_SUBTYPES.contains(&s) => Some(SubtypeSet::Enchantment),
        s if SPELL_SUBTYPES.contains(&s) => Some(SubtypeSet::Spell),
        s if BATTLE_SUBTYPES.contains(&s) => Some(SubtypeSet::Battle),
        s if PLANESWALKER_SUBTYPES.contains(&s) => Some(SubtypeSet::Planeswalker),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// CR 702.16a: `protection_quality_str` returns the lowercase singular
    /// protection noun for each card type the engine can offer as a chosen
    /// card type, and `None` for the four supplemental types that can never
    /// be chosen (`CARD_TYPES` offers only the seven main types).
    #[test]
    fn protection_quality_str_covers_all_core_types() {
        // 7 Some — the main card types.
        assert_eq!(
            CoreType::Artifact.protection_quality_str(),
            Some("artifact")
        );
        assert_eq!(
            CoreType::Creature.protection_quality_str(),
            Some("creature")
        );
        assert_eq!(
            CoreType::Enchantment.protection_quality_str(),
            Some("enchantment")
        );
        assert_eq!(CoreType::Instant.protection_quality_str(), Some("instant"));
        assert_eq!(CoreType::Sorcery.protection_quality_str(), Some("sorcery"));
        assert_eq!(
            CoreType::Planeswalker.protection_quality_str(),
            Some("planeswalker")
        );
        assert_eq!(CoreType::Land.protection_quality_str(), Some("land"));
        // 7 None — supplemental types never offered as a chosen card type.
        assert_eq!(CoreType::Tribal.protection_quality_str(), None);
        assert_eq!(CoreType::Battle.protection_quality_str(), None);
        assert_eq!(CoreType::Kindred.protection_quality_str(), None);
        assert_eq!(CoreType::Dungeon.protection_quality_str(), None);
        assert_eq!(CoreType::Plane.protection_quality_str(), None);
        assert_eq!(CoreType::Phenomenon.protection_quality_str(), None);
        assert_eq!(CoreType::Scheme.protection_quality_str(), None);
        assert_eq!(CoreType::Conspiracy.protection_quality_str(), None);
    }
}
