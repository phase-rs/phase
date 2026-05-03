//! Document-level Oracle IR types.
//!
//! `OracleDocIr` represents the complete parsed output of a card's Oracle text.
//! `OracleItemIr` categorizes each parsed item. Core variants carry proper IR
//! types (EffectChainIr, TriggerIr, StaticIr, ReplacementIr). PreLowered
//! variants carry already-assembled engine types from pre-processors and
//! dispatch paths that construct definitions directly.

use super::diagnostic::OracleDiagnostic;
use super::effect_chain::EffectChainIr;
use super::replacement::ReplacementIr;
use super::static_ir::StaticIr;
use super::trigger::TriggerIr;
use crate::types::ability::{
    AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;

/// Document-level IR: the complete parsed representation of a card's Oracle text.
///
/// Produced by `parse_oracle_ir`, consumed by `lower_oracle_ir`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OracleDocIr {
    /// Parsed items in source order.
    pub(crate) items: Vec<OracleItemIr>,
    /// Original Oracle text (provenance).
    pub(crate) source_text: String,
    /// Card name for self-reference context.
    pub(crate) card_name: String,
    /// Typed diagnostics accumulated during parsing (D-07).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<OracleDiagnostic>,
}

/// Individual parsed item from Oracle text.
///
/// Core variants carry IR types (EffectChainIr, TriggerIr, StaticIr, ReplacementIr).
/// PreLowered variants carry already-assembled engine types from pre-processors
/// and dispatch paths that construct definitions directly.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[allow(dead_code)] // Core IR variants used in tests + wired in future phases.
#[allow(clippy::large_enum_variant)] // Intentional: variants carry engine types directly.
pub(crate) enum OracleItemIr {
    /// Spell or activated ability effect chain (carries EffectChainIr since Phase 49).
    Spell(EffectChainIr),
    /// Triggered ability (carries TriggerIr since Phase 49).
    Trigger(TriggerIr),
    /// Static ability (carries StaticIr since Phase 49).
    Static(StaticIr),
    /// Replacement effect (carries ReplacementIr since Phase 49).
    Replacement(ReplacementIr),
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
    /// Pre-lowered trigger from pre-processors (saga, leveler, spacecraft, exert, etc.)
    /// that construct `TriggerDefinition` directly without going through branch IR parsers.
    PreLoweredTrigger(TriggerDefinition),
    /// Pre-lowered static from pre-processors (leveler, defiler, etc.)
    /// that construct `StaticDefinition` directly.
    PreLoweredStatic(StaticDefinition),
    /// Pre-lowered replacement from pre-processors (saga ETB replacement, etc.)
    /// that construct `ReplacementDefinition` directly.
    PreLoweredReplacement(ReplacementDefinition),
    /// Pre-lowered spell/activated ability from dispatch paths that construct
    /// `AbilityDefinition` directly with post-processing (equip, loyalty,
    /// activated abilities with manual cost/restriction, etc.).
    PreLoweredSpell(AbilityDefinition),
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
            diagnostics: vec![],
        };
        assert!(doc.items.is_empty());
        assert_eq!(doc.source_text, "Flying");
        assert_eq!(doc.card_name, "Serra Angel");
        assert!(doc.diagnostics.is_empty());
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
            diagnostics: vec![],
        };
        assert_eq!(doc.items.len(), 2);
    }
}
