use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;
use super::mana::ManaColor;

/// The seven layers of continuous effect evaluation per CR 613.
/// Sublayers of layer 7 (P/T) are represented as separate variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Layer {
    Copy,      // Layer 1
    Control,   // Layer 2
    Text,      // Layer 3
    Type,      // Layer 4
    Color,     // Layer 5
    Ability,   // Layer 6
    CharDef,   // Layer 7a - Characteristic-defining abilities
    SetPT,     // Layer 7b - Setting P/T
    ModifyPT,  // Layer 7c - Modifying P/T (+N/+N)
    SwitchPT,  // Layer 7d - Switching P/T
    CounterPT, // Layer 7e - Counter-based P/T
}

impl Layer {
    /// Returns all layer variants in evaluation order.
    pub fn all() -> &'static [Layer] {
        &[
            Layer::Copy,
            Layer::Control,
            Layer::Text,
            Layer::Type,
            Layer::Color,
            Layer::Ability,
            Layer::CharDef,
            Layer::SetPT,
            Layer::ModifyPT,
            Layer::SwitchPT,
            Layer::CounterPT,
        ]
    }

    /// Whether this layer uses dependency ordering per CR 613.
    /// Layers where one effect's outcome can change another effect's applicability.
    pub fn has_dependency_ordering(&self) -> bool {
        matches!(
            self,
            Layer::Copy
                | Layer::Control
                | Layer::Text
                | Layer::Type
                | Layer::Ability
                | Layer::CharDef
                | Layer::SetPT
        )
    }
}

/// An active continuous effect targeting a specific layer, collected during evaluation.
#[derive(Debug, Clone)]
pub struct ActiveContinuousEffect {
    pub source_id: ObjectId,
    pub def_index: usize,
    pub layer: Layer,
    pub timestamp: u64,
    pub params: HashMap<String, String>,
    pub affected_filter: String,
    pub mode: String,
}

/// What modification a continuous effect applies to an object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuousEffectApplication {
    AddPower(i32),
    AddToughness(i32),
    SetPower(i32),
    SetToughness(i32),
    AddKeyword(String),
    RemoveKeyword(String),
    AddAbility(String),
    AddType(String),
    RemoveType(String),
    SetColor(Vec<ManaColor>),
    AddColor(ManaColor),
    RemoveAllAbilities,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_all_returns_eleven_variants() {
        assert_eq!(Layer::all().len(), 11);
    }

    #[test]
    fn layer_ordering_is_correct() {
        let all = Layer::all();
        for i in 1..all.len() {
            assert!(
                all[i - 1] < all[i],
                "Layer {:?} should be before {:?}",
                all[i - 1],
                all[i]
            );
        }
    }

    #[test]
    fn dependency_ordering_layers() {
        assert!(Layer::Copy.has_dependency_ordering());
        assert!(Layer::Type.has_dependency_ordering());
        assert!(Layer::Ability.has_dependency_ordering());
        assert!(!Layer::ModifyPT.has_dependency_ordering());
        assert!(!Layer::SwitchPT.has_dependency_ordering());
        assert!(!Layer::CounterPT.has_dependency_ordering());
    }
}
