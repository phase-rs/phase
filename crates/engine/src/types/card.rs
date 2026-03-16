use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, PtValue,
    ReplacementDefinition, SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use super::card_type::CardType;
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrintedCardRef {
    pub oracle_id: String,
    pub face_name: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardFace {
    pub name: String,
    pub mana_cost: ManaCost,
    pub card_type: CardType,
    pub power: Option<PtValue>,
    pub toughness: Option<PtValue>,
    pub loyalty: Option<String>,
    pub defense: Option<String>,
    pub oracle_text: Option<String>,
    pub non_ability_text: Option<String>,
    pub flavor_name: Option<String>,
    pub keywords: Vec<Keyword>,
    pub abilities: Vec<AbilityDefinition>,
    pub triggers: Vec<TriggerDefinition>,
    pub static_abilities: Vec<StaticDefinition>,
    pub replacements: Vec<ReplacementDefinition>,
    pub color_override: Option<Vec<ManaColor>>,
    #[serde(default)]
    pub scryfall_oracle_id: Option<String>,
    /// Modal spell metadata ("Choose one —", "Choose two —", etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    /// Additional casting cost ("As an additional cost to cast this spell, ...").
    /// Parsed from Oracle text or synthesized from keywords (e.g. kicker).
    /// When present, the casting flow prompts the player for a decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_cost: Option<AdditionalCost>,
    /// Spell-casting restrictions ("Cast this spell only during combat", etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_restrictions: Vec<CastingRestriction>,
    /// Spell-casting options ("you may pay ... rather than pay this spell's mana cost", etc.).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub casting_options: Vec<SpellCastingOption>,
    /// CR 719.1: Solve condition for Case enchantments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub solve_condition: Option<SolveCondition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
