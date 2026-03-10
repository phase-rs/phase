use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
};
use super::card_type::CardType;
use super::mana::{ManaColor, ManaCost};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardFace {
    pub name: String,
    pub mana_cost: ManaCost,
    pub card_type: CardType,
    pub power: Option<String>,
    pub toughness: Option<String>,
    pub loyalty: Option<String>,
    pub defense: Option<String>,
    pub oracle_text: Option<String>,
    pub non_ability_text: Option<String>,
    pub flavor_name: Option<String>,
    pub keywords: Vec<String>,
    pub abilities: Vec<AbilityDefinition>,
    pub triggers: Vec<TriggerDefinition>,
    pub static_abilities: Vec<StaticDefinition>,
    pub replacements: Vec<ReplacementDefinition>,
    pub svars: HashMap<String, String>,
    pub color_override: Option<Vec<ManaColor>>,
    #[serde(default)]
    pub scryfall_oracle_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CardLayout {
    Single(CardFace),
    Split(CardFace, CardFace),
    Flip(CardFace, CardFace),
    Transform(CardFace, CardFace),
    Meld(CardFace, CardFace),
    Adventure(CardFace, CardFace),
    Modal(CardFace, CardFace),
    Omen(CardFace, CardFace),
    Specialize(CardFace, Vec<CardFace>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardRules {
    pub layout: CardLayout,
    pub meld_with: Option<String>,
    pub partner_with: Option<String>,
}

impl CardRules {
    pub fn name(&self) -> &str {
        match &self.layout {
            CardLayout::Single(face)
            | CardLayout::Split(face, _)
            | CardLayout::Flip(face, _)
            | CardLayout::Transform(face, _)
            | CardLayout::Meld(face, _)
            | CardLayout::Adventure(face, _)
            | CardLayout::Modal(face, _)
            | CardLayout::Omen(face, _)
            | CardLayout::Specialize(face, _) => &face.name,
        }
    }

    pub fn face_names(&self) -> Vec<&str> {
        match &self.layout {
            CardLayout::Single(face) => vec![&face.name],
            CardLayout::Split(a, b)
            | CardLayout::Flip(a, b)
            | CardLayout::Transform(a, b)
            | CardLayout::Meld(a, b)
            | CardLayout::Adventure(a, b)
            | CardLayout::Modal(a, b)
            | CardLayout::Omen(a, b) => vec![&a.name, &b.name],
            CardLayout::Specialize(base, variants) => {
                let mut names = vec![base.name.as_str()];
                for v in variants {
                    names.push(&v.name);
                }
                names
            }
        }
    }
}
