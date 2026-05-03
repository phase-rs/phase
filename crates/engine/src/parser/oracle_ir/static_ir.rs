//! Static ability IR types.
//!
//! `StaticIr` is a thin wrapper around `StaticDefinition` that captures the
//! source text and an optional `EffectChainIr` body. Per D-06, the primary
//! cross-branch reuse pattern is `EffectChainIr` — no deeper IR decomposition
//! is needed for statics.

use serde::Serialize;

use super::effect_chain::EffectChainIr;
use crate::types::ability::StaticDefinition;

/// Static ability IR: wraps the parsed `StaticDefinition` with provenance
/// and an optional effect chain IR body.
///
/// Output of `parse_static_line_ir`. Consumed by `lower_static_ir` to produce
/// a `StaticDefinition` (applying post-parse transforms like active zone inference).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct StaticIr {
    /// The parsed static definition (pre-lowering — active_zones not yet populated).
    pub(crate) definition: StaticDefinition,
    /// Original oracle text for description/provenance.
    pub(crate) source_text: String,
    /// Optional effect chain IR body (e.g., from granted activated abilities).
    /// Most statics have `None` — the `EffectChainIr` capture happens inside
    /// `parse_effect_chain_ir` which is already called by internal sub-parsers.
    pub(crate) body_ir: Option<EffectChainIr>,
}
