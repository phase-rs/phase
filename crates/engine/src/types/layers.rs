use serde::{Deserialize, Serialize};

use super::ability::{ContinuousModification, TargetFilter};
use super::identifiers::ObjectId;
use super::player::PlayerId;
use super::statics::StaticMode;

/// The seven layers of continuous effect evaluation per CR 613.
/// Sublayers of layer 7 (P/T) are represented as separate variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Layer {
    /// CR 613.1a: Layer 1 — Copy effects.
    Copy,
    /// CR 613.1b: Layer 2 — Control-changing effects.
    Control,
    /// CR 613.1c: Layer 3 — Text-changing effects.
    Text,
    /// CR 613.1d: Layer 4 — Type-changing effects.
    Type,
    /// CR 613.1e: Layer 5 — Color-changing effects.
    Color,
    /// CR 613.1f: Layer 6 — Ability-adding and ability-removing effects.
    Ability,
    /// CR 613.4a: Layer 7a — Characteristic-defining abilities that set P/T.
    CharDef,
    /// CR 613.4b: Layer 7b — Effects that set P/T to specific values.
    SetPT,
    /// CR 613.4c: Layer 7c — Effects that modify P/T (+N/+N).
    ModifyPT,
    /// CR 613.4d: Layer 7d — Effects that switch P/T.
    SwitchPT,
    /// CR 613.4c: Layer 7e — +1/+1 and -1/-1 counters modifying P/T (applied after other 7c effects).
    CounterPT,
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

impl ContinuousModification {
    /// Returns the appropriate Layer for this modification type.
    pub fn layer(&self) -> Layer {
        match self {
            ContinuousModification::CopyValues { .. } => Layer::Copy,
            // CR 707.9b + CR 613.1a: Copy-effect name override applies in Layer 1
            // after CopyValues, per timestamp order within the layer.
            ContinuousModification::SetName { .. } => Layer::Copy,
            ContinuousModification::AddPower { .. }
            | ContinuousModification::AddToughness { .. }
            | ContinuousModification::AddDynamicPower { .. }
            | ContinuousModification::AddDynamicToughness { .. } => Layer::ModifyPT,
            ContinuousModification::SetPower { .. }
            | ContinuousModification::SetToughness { .. }
            | ContinuousModification::SetPowerDynamic { .. }
            | ContinuousModification::SetToughnessDynamic { .. } => Layer::SetPT,
            ContinuousModification::SetDynamicPower { .. }
            | ContinuousModification::SetDynamicToughness { .. } => Layer::CharDef,
            ContinuousModification::AddKeyword { .. }
            | ContinuousModification::RemoveKeyword { .. }
            | ContinuousModification::AddDynamicKeyword { .. }
            | ContinuousModification::GrantAbility { .. }
            | ContinuousModification::GrantTrigger { .. }
            | ContinuousModification::RemoveAllAbilities
            | ContinuousModification::AddStaticMode { .. } => Layer::Ability,
            ContinuousModification::AddType { .. }
            | ContinuousModification::RemoveType { .. }
            | ContinuousModification::AddSubtype { .. }
            | ContinuousModification::RemoveSubtype { .. }
            | ContinuousModification::AddSupertype { .. }
            | ContinuousModification::RemoveSupertype { .. }
            | ContinuousModification::AddAllCreatureTypes
            | ContinuousModification::AddAllBasicLandTypes
            | ContinuousModification::AddChosenSubtype { .. }
            | ContinuousModification::SetBasicLandType { .. } => Layer::Type, // CR 613.1d + CR 205.4b
            // CR 122.1 + CR 614.1c: One-shot counter placement at copy
            // resolution. Consumed by the BecomeCopy / CopyTokenOf resolvers
            // before any continuous-effect machinery is reached. Reaching this
            // arm via `apply_continuous_effect` indicates a wiring bug.
            ContinuousModification::AddCounterOnEnter { .. } => unreachable!(
                "AddCounterOnEnter is consumed at resolution; never layered. \
                 Verify resolver dispatch in token_copy.rs / become_copy.rs."
            ),
            ContinuousModification::SetColor { .. }
            | ContinuousModification::AddColor { .. }
            | ContinuousModification::AddChosenColor => Layer::Color,
            // CR 613.4d: Switch P/T is applied in layer 7d.
            ContinuousModification::SwitchPowerToughness => Layer::SwitchPT,
            // CR 510.1c: Rule-modification effect processed in Ability layer (layer 6).
            ContinuousModification::AssignDamageFromToughness
            | ContinuousModification::AssignDamageAsThoughUnblocked
            | ContinuousModification::AssignNoCombatDamage => Layer::Ability,
            // CR 613.2: Control-changing effects are applied in Layer 2.
            ContinuousModification::ChangeController => Layer::Control,
            // CR 707.9a: A copy effect that grants "this ability" makes that
            // ability part of the copiable values. Applied at Layer 1 alongside
            // CopyValues / SetName so downstream copy effects observe the
            // retained ability when reading copiable values.
            ContinuousModification::RetainPrintedTriggerFromSource { .. } => Layer::Copy,
        }
    }
}

/// An active continuous effect targeting a specific layer, collected during evaluation.
#[derive(Debug, Clone)]
pub struct ActiveContinuousEffect {
    pub source_id: ObjectId,
    pub controller: PlayerId,
    /// Index into the source object's `static_definitions` array, or `None` for
    /// transient effects that have no backing static definition on any object.
    pub def_index: Option<usize>,
    pub layer: Layer,
    pub timestamp: u64,
    pub modification: ContinuousModification,
    pub affected_filter: TargetFilter,
    pub mode: StaticMode,
    /// True for characteristic-defining abilities (CDAs), which are processed
    /// before other effects within their layer per CR 604.3.
    pub characteristic_defining: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::CopiableValues;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaColor;

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

    #[test]
    fn continuous_modification_layer_mapping() {
        assert_eq!(
            ContinuousModification::CopyValues {
                values: Box::new(CopiableValues {
                    name: "Clone".to_string(),
                    mana_cost: crate::types::mana::ManaCost::default(),
                    color: vec![],
                    card_types: crate::types::card_type::CardType::default(),
                    power: None,
                    toughness: None,
                    loyalty: None,
                    keywords: vec![],
                    abilities: Default::default(),
                    trigger_definitions: Default::default(),
                    replacement_definitions: Default::default(),
                    static_definitions: Default::default(),
                })
            }
            .layer(),
            Layer::Copy
        );
        assert_eq!(
            ContinuousModification::AddPower { value: 1 }.layer(),
            Layer::ModifyPT
        );
        assert_eq!(
            ContinuousModification::AddToughness { value: 1 }.layer(),
            Layer::ModifyPT
        );
        assert_eq!(
            ContinuousModification::SetPower { value: 3 }.layer(),
            Layer::SetPT
        );
        assert_eq!(
            ContinuousModification::SetToughness { value: 3 }.layer(),
            Layer::SetPT
        );
        assert_eq!(
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying
            }
            .layer(),
            Layer::Ability
        );
        assert_eq!(
            ContinuousModification::RemoveKeyword {
                keyword: Keyword::Defender
            }
            .layer(),
            Layer::Ability
        );
        assert_eq!(
            ContinuousModification::GrantAbility {
                definition: Box::new(crate::types::ability::AbilityDefinition::new(
                    crate::types::ability::AbilityKind::Spell,
                    crate::types::ability::Effect::Unimplemented {
                        name: "Hexproof".to_string(),
                        description: None,
                    },
                ))
            }
            .layer(),
            Layer::Ability
        );
        assert_eq!(
            ContinuousModification::RemoveAllAbilities.layer(),
            Layer::Ability
        );
        assert_eq!(
            ContinuousModification::AddType {
                core_type: crate::types::card_type::CoreType::Artifact
            }
            .layer(),
            Layer::Type
        );
        assert_eq!(
            ContinuousModification::RemoveType {
                core_type: crate::types::card_type::CoreType::Creature
            }
            .layer(),
            Layer::Type
        );
        assert_eq!(
            ContinuousModification::SetColor {
                colors: vec![ManaColor::Blue]
            }
            .layer(),
            Layer::Color
        );
        assert_eq!(
            ContinuousModification::AddColor {
                color: ManaColor::Red
            }
            .layer(),
            Layer::Color
        );
        assert_eq!(
            ContinuousModification::ChangeController.layer(),
            Layer::Control
        );
        // CR 613.1d: SetBasicLandType is a type-changing effect (Layer 4).
        assert_eq!(
            ContinuousModification::SetBasicLandType {
                land_type: crate::types::ability::BasicLandType::Mountain,
            }
            .layer(),
            Layer::Type
        );
        // CR 613.4d: SwitchPowerToughness is layer 7d.
        assert_eq!(
            ContinuousModification::SwitchPowerToughness.layer(),
            Layer::SwitchPT
        );
    }
}
