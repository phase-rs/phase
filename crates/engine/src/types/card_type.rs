use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CoreType {
    Artifact,
    Creature,
    Enchantment,
    Instant,
    Land,
    Planeswalker,
    Sorcery,
    Tribal,
    Battle,
    Kindred,
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

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardType {
    pub supertypes: Vec<Supertype>,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
}
