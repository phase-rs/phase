//! Unified parsing context for pronoun and reference resolution.
//!
//! Flat superset of the former effect-chain and nom ParseContext structs.
//! All parser branches import from this single location (Phase 50, D-01).

use super::diagnostic::OracleDiagnostic;
use crate::types::ability::{ControllerRef, QuantityRef, TargetFilter};

/// Unified parsing context — threaded through all parser branches for
/// pronoun/reference resolution ("it", "that creature", "that many").
///
/// Callers set only the fields they need; all fields are Default-able (D-02).
#[derive(Debug, Clone, Default)]
pub(crate) struct ParseContext {
    /// The current subject (resolved target — "it", "that creature").
    pub subject: Option<TargetFilter>,
    /// Card name for self-reference (~) normalization.
    pub card_name: Option<String>,
    /// CR 707.9a + CR 603.1: Index of the printed trigger whose body is being
    /// parsed. Consumed by BecomeCopy "has this ability" arm.
    pub current_trigger_index: Option<usize>,
    /// CR 701.21a + CR 608.2k: The actor performing the effect ("you", "an opponent").
    pub actor: Option<ControllerRef>,
    /// Resolved quantity reference ("that many", "that much").
    #[allow(dead_code)] // Retained for future nom combinator consumers (D-02).
    pub quantity_ref: Option<QuantityRef>,
    /// Whether we are inside a trigger effect (enables event context refs).
    #[allow(dead_code)] // Retained for future nom combinator consumers (D-02).
    pub in_trigger: bool,
    /// Whether we are inside a replacement effect.
    #[allow(dead_code)] // Retained for future nom combinator consumers (D-02).
    pub in_replacement: bool,
    /// Accumulated diagnostics for the current card parse (Phase 52, D-07).
    /// Replaces thread-local oracle_warnings accumulator.
    pub diagnostics: Vec<OracleDiagnostic>,
}

impl ParseContext {
    /// Push a diagnostic (replaces oracle_warnings::push_diagnostic).
    pub fn push_diagnostic(&mut self, d: OracleDiagnostic) {
        self.diagnostics.push(d);
    }
}
