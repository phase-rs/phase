//! Replacement effect IR types.
//!
//! `ReplacementIr` is a thin wrapper around `ReplacementDefinition` that
//! captures the source text and an optional `EffectChainIr` execute body.
//! Per D-06, the primary cross-branch reuse pattern is `EffectChainIr` —
//! no deeper IR decomposition is needed for replacements.

use serde::Serialize;

use super::effect_chain::EffectChainIr;
use crate::types::ability::ReplacementDefinition;

/// Replacement effect IR: wraps the parsed `ReplacementDefinition` with
/// provenance and an optional effect chain IR for the execute body.
///
/// Output of `parse_replacement_line_ir`. Consumed by `lower_replacement_ir`
/// to produce a `ReplacementDefinition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ReplacementIr {
    /// The parsed replacement definition.
    pub(crate) definition: ReplacementDefinition,
    /// Original oracle text for description/provenance.
    pub(crate) source_text: String,
    /// Optional effect chain IR for the execute body. The `EffectChainIr`
    /// capture happens inside `parse_effect_chain_ir` which is already called
    /// by replacement sub-parsers internally.
    pub(crate) execute_ir: Option<EffectChainIr>,
}
