//! Document-level Oracle IR types.
//!
//! `OracleDocIr` represents the complete parsed output of a card's Oracle text.
//! `OracleItemIr` categorizes each parsed item. In Phase 47, variants carry
//! existing engine types directly (per D-05). Phases 48-49 swap in proper IR
//! types as each parser branch gets its lowering split.

use super::trigger::TriggerIr;
use crate::types::ability::{
    AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;

/// Document-level IR: the complete parsed representation of a card's Oracle text.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Constructed in tests now; wired into parser in Phase 48.
pub(crate) struct OracleDocIr {
    /// Parsed items in source order.
    pub(crate) items: Vec<OracleItemIr>,
    /// Original Oracle text (provenance).
    pub(crate) source_text: String,
    /// Card name for self-reference context.
    pub(crate) card_name: String,
}

/// Individual parsed item from Oracle text.
///
/// Each variant carries existing engine types directly — these will be replaced
/// by proper IR types (EffectChainIr, TriggerIr, etc.) in Phases 48-49.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // Used in tests now; wired into parser in Phase 48.
#[allow(clippy::large_enum_variant)] // Intentional: variants carry existing engine types directly per D-05.
pub(crate) enum OracleItemIr {
    /// Spell or activated ability effect chain.
    Spell(AbilityDefinition),
    /// Triggered ability (carries TriggerIr since Phase 49).
    Trigger(TriggerIr),
    /// Static ability.
    Static(StaticDefinition),
    /// Replacement effect.
    Replacement(ReplacementDefinition),
    /// Keyword ability from keyword-only line.
    Keyword(Keyword),
    /// Modal spell block (Choose one/two/etc.).
    Modal(ModalChoice),
    /// Additional casting cost.
    AdditionalCost(AdditionalCost),
    /// Casting restriction.
    CastingRestriction(CastingRestriction),
    /// Casting option (alternative/additional modes).
    CastingOption(SpellCastingOption),
    /// Case enchantment solve condition.
    SolveCondition(SolveCondition),
    /// Strive per-target surcharge.
    StriveCost(ManaCost),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_doc_ir_empty_construction() {
        let doc = OracleDocIr {
            items: vec![],
            source_text: "Flying".to_string(),
            card_name: "Serra Angel".to_string(),
        };
        assert!(doc.items.is_empty());
        assert_eq!(doc.source_text, "Flying");
        assert_eq!(doc.card_name, "Serra Angel");
    }

    #[test]
    fn oracle_item_ir_keyword_variant() {
        let item = OracleItemIr::Keyword(Keyword::Flying);
        assert!(matches!(item, OracleItemIr::Keyword(Keyword::Flying)));
    }

    #[test]
    fn oracle_doc_ir_mixed_items() {
        let doc = OracleDocIr {
            items: vec![
                OracleItemIr::Keyword(Keyword::Flying),
                OracleItemIr::Keyword(Keyword::Vigilance),
            ],
            source_text: "Flying\nVigilance".to_string(),
            card_name: "Test Angel".to_string(),
        };
        assert_eq!(doc.items.len(), 2);
    }
}
