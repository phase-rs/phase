use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, PtValue,
    ReplacementDefinition, SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use super::card_type::CardType;
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost};
use crate::parser::oracle_ir::diagnostic::OracleDiagnostic;

/// Card rarity as assigned per-printing in MTGJSON set data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Rarity {
    Common,
    Uncommon,
    Rare,
    Mythic,
    Special,
    Bonus,
}

/// Diagnostic metadata for a card face. Grouped here to keep debug/pipeline
/// concerns separate from game-logic fields. Omitted from JSON when empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardMetadata {
    /// Number of abilities translated from Forge card scripts (fallback for Oracle parser gaps).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub forge_abilities: u32,
    /// Number of triggers translated from Forge card scripts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub forge_triggers: u32,
    /// Number of static abilities translated from Forge card scripts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub forge_statics: u32,
    /// Number of replacement effects translated from Forge card scripts.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub forge_replacements: u32,
}

impl CardMetadata {
    pub fn is_empty(&self) -> bool {
        self.forge_abilities == 0
            && self.forge_triggers == 0
            && self.forge_statics == 0
            && self.forge_replacements == 0
    }
}

fn is_zero(v: &u32) -> bool {
    *v == 0
}

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub color_identity: Vec<ManaColor>,
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
    /// CR 207.2c + CR 601.2f: Strive per-target surcharge cost.
    /// "This spell costs {X} more to cast for each target beyond the first."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strive_cost: Option<ManaCost>,
    /// Whether this card can serve as a Brawl commander.
    /// Derived from MTGJSON `leadershipSkills.brawl` OR type-line analysis
    /// (legendary creature, legendary planeswalker, or "can be your commander").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub brawl_commander: bool,
    /// Parser diagnostic warnings — silent fallbacks, ignored remainders, bare filters.
    /// Populated at build time by the Oracle parser warning accumulator.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parse_warnings: Vec<OracleDiagnostic>,
    /// Diagnostic metadata (forge source counts, etc.). Omitted from JSON when empty.
    #[serde(default, skip_serializing_if = "CardMetadata::is_empty")]
    pub metadata: CardMetadata,
    /// Set of all rarities this card has been printed at (across all sets).
    /// Used for format legality checks (e.g. PDH commander must have an uncommon printing).
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub rarities: BTreeSet<Rarity>,
}

/// Runtime layout discriminant for double-faced cards.
///
/// Stored on `BackFaceData` so the engine can distinguish Modal DFCs
/// (which allow face-choice per CR 712.12) from Transform DFCs at runtime.
/// Intentionally separate from `database::synthesis::LayoutKind` which is a
/// build-pipeline type without serialization derives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LayoutKind {
    Single,
    Split,
    Flip,
    Transform,
    Meld,
    Adventure,
    Modal,
    Omen,
    /// CR 702.xxx: Prepare (Strixhaven) frame mechanic — a two-face card whose
    /// face `b` is a "prepare spell" (Sorcery/Instant). When face `a` is
    /// prepared, a copy of face `b` can be cast. Structurally an Adventure
    /// analog. Assign when WotC publishes SOS CR update.
    Prepare,
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
    /// CR 702.xxx: Prepare (Strixhaven) — face `a` is the creature, face `b` is
    /// the prepare-spell (Sorcery/Instant). When the creature is prepared, its
    /// controller may cast a copy of face `b`. Assign when WotC publishes SOS
    /// CR update.
    Prepare(CardFace, CardFace),
    Specialize(CardFace, Vec<CardFace>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardRules {
    pub layout: CardLayout,
    pub meld_with: Option<String>,
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
            | CardLayout::Prepare(face, _)
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
            | CardLayout::Omen(a, b)
            | CardLayout::Prepare(a, b) => vec![&a.name, &b.name],
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
