//! Unified parsing context for pronoun and reference resolution.
//!
//! Flat superset of the former effect-chain and nom ParseContext structs.
//! All parser branches import from this single location (Phase 50, D-01).

use super::diagnostic::OracleDiagnostic;
use crate::types::ability::{ControllerRef, QuantityRef, TargetFilter, TargetSelectionMode};

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
    /// CR 109.4 + CR 115.1: Relative-player scope for "that player controls"
    /// resolution inside trigger effects. Replaces thread-local oracle_target_scope.
    pub relative_player_scope: Option<ControllerRef>,
    /// CR 115.1 + CR 701.9b: Target selection mode for the most recent target
    /// phrase parsed via `parse_target_with_ctx`. The chunk loop in
    /// `parse_effect_chain_ir` snapshots this into the produced `ClauseIr` and
    /// resets it to `Chosen` for the next chunk so the marker is per-clause.
    pub target_selection_mode: TargetSelectionMode,
}

impl ParseContext {
    /// Resolve third-person player pronouns ("they", "their") against the
    /// nearest parser context that introduced a player referent.
    pub fn third_person_player_controller_ref(&self) -> Option<ControllerRef> {
        self.relative_player_scope
            .clone()
            .or_else(|| self.actor.clone())
    }

    /// Push a diagnostic (replaces oracle_warnings::push_diagnostic).
    pub fn push_diagnostic(&mut self, d: OracleDiagnostic) {
        if matches!(d, OracleDiagnostic::TargetFallback { .. })
            && self.diagnostics.iter().any(|existing| existing == &d)
        {
            return;
        }
        self.diagnostics.push(d);
    }

    /// Execute `f` with a temporary relative-player scope, restoring the prior
    /// value on return. Replaces thread-local ScopeGuard RAII pattern.
    #[allow(dead_code)] // Available for nested-scope uses (e.g., nested triggers).
    pub fn with_player_scope<R>(
        &mut self,
        scope: ControllerRef,
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let prev = self.relative_player_scope.take();
        self.relative_player_scope = Some(scope);
        let result = f(self);
        self.relative_player_scope = prev;
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_fallback_diagnostics_are_idempotent() {
        let mut ctx = ParseContext::default();
        let diagnostic = OracleDiagnostic::TargetFallback {
            context: "search-filter-suffix unmatched".into(),
            text: "with an unsupported clause".into(),
            line_index: 0,
        };

        ctx.push_diagnostic(diagnostic.clone());
        ctx.push_diagnostic(diagnostic);

        assert_eq!(ctx.diagnostics.len(), 1);
    }

    #[test]
    fn distinct_target_fallback_diagnostics_are_preserved() {
        let mut ctx = ParseContext::default();

        ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
            context: "search-filter-suffix unmatched".into(),
            text: "first clause".into(),
            line_index: 0,
        });
        ctx.push_diagnostic(OracleDiagnostic::TargetFallback {
            context: "search-filter-suffix unmatched".into(),
            text: "second clause".into(),
            line_index: 0,
        });

        assert_eq!(ctx.diagnostics.len(), 2);
    }
}
