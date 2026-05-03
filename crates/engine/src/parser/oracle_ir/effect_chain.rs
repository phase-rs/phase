//! Effect chain IR types.
//!
//! `EffectChainIr` represents the pre-assembly clause list produced by IR production.
//! `ClauseIr` captures each parsed chunk's effect plus all stripped context (conditions,
//! optionality, continuations, temporal markers). Lowering consumes this flat clause
//! list and performs all assembly operations (continuation patching, condition lifting,
//! delayed-trigger wrapping, sub_ability chain wiring).

use serde::Serialize;

use super::ast::{ClauseBoundary, ContinuationAst, ParsedEffectClause};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ControllerRef,
    DelayedTriggerCondition, MultiTargetSpec, OpponentMayScope, PlayerFilter, QuantityExpr,
    RoundingMode,
};

/// Chain-level IR: the complete parsed representation of an effect chain before assembly.
///
/// Output of `parse_effect_chain_ir` (Plan 02). Consumed by `lower_effect_chain_ir`
/// to produce an `AbilityDefinition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EffectChainIr {
    /// Parsed clauses in source order — each `ClauseIr` captures one parsed
    /// chunk's effect plus all stripped context (conditions, optionality,
    /// continuations, temporal markers). Lowering converts this flat list into
    /// `AbilityDefinition`s via def assembly, continuation patching, and
    /// sub_ability chaining.
    pub(crate) clauses: Vec<ClauseIr>,
    /// The ability kind (Spell, Activated, etc.).
    pub(crate) kind: AbilityKind,
    /// CR 107.1a: Chain-level rounding annotation ("Round down/up each time").
    pub(crate) chain_rounding: Option<RoundingMode>,
    /// CR 701.21a: Actor context threaded from ParseContext (per D-07).
    pub(crate) actor: Option<ControllerRef>,
}

/// Special-case clause actions that modify or attach to adjacent clauses during lowering.
///
/// The chunk loop's special-case handlers (otherwise, instead, alt-cost rider, etc.)
/// currently modify `defs: Vec<AbilityDefinition>` inline. In the IR split, these
/// become markers that lowering processes when building the def list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum SpecialClause {
    /// CR 118.9 + CR 119.4: Alternative-cost rider — fold cost onto previous CastFromZone.
    AltCostRider(AbilityCost),
    /// CR 608.2c: "Otherwise, [effect]" — attach as else_ability on previous conditional.
    Otherwise(Box<AbilityDefinition>),
    /// CR 608.2c: "Otherwise" fallback — no conditional found, emit as Unimplemented + def.
    OtherwiseFallback(Box<AbilityDefinition>),
    /// CR 614.1a + CR 514.2: Die-exile-rider — attach as sub_ability on previous def.
    DieExileRider(Box<AbilityDefinition>),
    /// CR 608.2c: Dig-instead alternative — replace previous Dig with conditional alternative.
    DigInsteadAlt(Box<AbilityDefinition>),
    /// CR 608.2e: Generic instead clause — attach to previous def as sub_ability.
    InsteadClause(Box<AbilityDefinition>),
    /// CR 508.4 / CR 614.1: Conditional enters-tapped-attacking modifier on previous clause.
    EntersTappedAttacking,
    /// CR 608.2e: TargetHasKeywordInstead — attach to previous def as sub_ability.
    KeywordInsteadOverride,
    /// CR 608.2e: AdditionalCostPaidInstead + SearchLibrary — fold else_ability from previous.
    AdditionalCostInsteadSearch,
}

/// Per-clause IR: captures everything about a single parsed chunk before chain assembly.
///
/// Each field corresponds to a local variable extracted during the chunk loop's
/// "strip cascade" in `parse_effect_chain_ir`. All assembly logic (continuation
/// patching, condition lifting, sub_ability wiring) is deferred to lowering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ClauseIr {
    /// The parsed effect clause (effect, duration, sub_ability from parse_effect_clause).
    pub(crate) parsed: ParsedEffectClause,
    /// Clause boundary from split_clause_sequence.
    pub(crate) boundary: Option<ClauseBoundary>,
    /// CR 608.2c: Leading or suffix conditional guard.
    pub(crate) condition: Option<AbilityCondition>,
    /// CR 609.3: "You may" optional effect.
    pub(crate) is_optional: bool,
    /// CR 608.2d: Opponent-may scope.
    pub(crate) opponent_may_scope: Option<OpponentMayScope>,
    /// CR 609.3: "for each" / "N times" repeat quantity.
    pub(crate) repeat_for: Option<QuantityExpr>,
    /// Player scope iteration ("each opponent", "each player").
    pub(crate) player_scope: Option<PlayerFilter>,
    /// CR 603.7: Temporal suffix delayed trigger condition.
    pub(crate) delayed_condition: Option<DelayedTriggerCondition>,
    /// CR 603.7a: Temporal prefix delayed trigger condition.
    pub(crate) prefix_delayed_condition: Option<DelayedTriggerCondition>,
    /// Intrinsic continuation marker (parsed from this chunk's text, applies to self).
    pub(crate) intrinsic_continuation: Option<ContinuationAst>,
    /// Followup continuation marker (parsed from this chunk's text, applies to previous clause).
    pub(crate) followup_continuation: Option<ContinuationAst>,
    /// Whether this clause was absorbed by a followup continuation.
    pub(crate) absorbed_by_followup: bool,
    /// CR 115.1d: Multi-target spec.
    pub(crate) multi_target: Option<MultiTargetSpec>,
    /// CR 107.3i: "where X is <expr>" binding.
    pub(crate) where_x_expression: Option<String>,
    /// Special-case: "otherwise" clause that attaches to prior conditional.
    pub(crate) is_otherwise: bool,
    /// Special-case action that modifies adjacent clauses during lowering.
    pub(crate) special: Option<SpecialClause>,
    /// The raw normalized text (for debug/diagnostic purposes).
    pub(crate) source_text: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::ast::parsed_clause;
    use crate::types::ability::{Effect, TargetFilter};

    #[test]
    fn effect_chain_ir_empty_construction() {
        let ir = EffectChainIr {
            clauses: vec![],
            kind: AbilityKind::Spell,
            chain_rounding: None,
            actor: None,
        };
        assert!(ir.clauses.is_empty());
    }

    #[test]
    fn clause_ir_default_fields() {
        let clause = ClauseIr {
            parsed: parsed_clause(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }),
            boundary: None,
            condition: None,
            is_optional: false,
            opponent_may_scope: None,
            repeat_for: None,
            player_scope: None,
            delayed_condition: None,
            prefix_delayed_condition: None,
            intrinsic_continuation: None,
            followup_continuation: None,
            absorbed_by_followup: false,
            multi_target: None,
            where_x_expression: None,
            is_otherwise: false,
            special: None,
            source_text: "draw a card".to_string(),
        };
        assert_eq!(clause.source_text, "draw a card");
        assert!(!clause.is_optional);
        assert!(!clause.is_otherwise);
        assert!(!clause.absorbed_by_followup);
    }

    #[test]
    fn effect_chain_ir_with_single_clause() {
        let ir = EffectChainIr {
            clauses: vec![ClauseIr {
                parsed: parsed_clause(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Controller,
                }),
                boundary: Some(ClauseBoundary::Sentence),
                condition: None,
                is_optional: false,
                opponent_may_scope: None,
                repeat_for: None,
                player_scope: None,
                delayed_condition: None,
                prefix_delayed_condition: None,
                intrinsic_continuation: None,
                followup_continuation: None,
                absorbed_by_followup: false,
                multi_target: None,
                where_x_expression: None,
                is_otherwise: false,
                special: None,
                source_text: "draw two cards".to_string(),
            }],
            kind: AbilityKind::Spell,
            chain_rounding: None,
            actor: None,
        };
        assert_eq!(ir.clauses.len(), 1);
        assert_eq!(ir.kind, AbilityKind::Spell);
    }
}
