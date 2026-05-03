//! Typed Oracle parse diagnostics (Phase 50, D-04).
//!
//! Replaces thread-local `push_warning` string accumulation with
//! machine-readable diagnostics carrying severity and source provenance.

use std::fmt;

/// Severity level for parse diagnostics (D-05).
/// Derived from the variant — not stored as a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)] // Used by future consumers in Plan 3.
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

/// Which cascade slot was lost in a cascade-diff diagnostic.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum CascadeSlot {
    Optional,
    OpponentMay,
    Condition,
    RepeatFor,
    PlayerScope,
    Duration,
}

/// Typed Oracle parse diagnostic (D-04).
///
/// Every variant carries `line_index` for source provenance (D-06).
/// Severity is determined by variant via `severity()` method (D-05).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum OracleDiagnostic {
    /// Parser fell back to a degraded target filter (TargetFilter::Any or similar).
    /// Covers both target-fallback and bare-filter-fallback categories.
    TargetFallback {
        context: String,
        text: String,
        line_index: usize,
    },

    /// Text remained after a successful parse that was silently discarded.
    IgnoredRemainder {
        text: String,
        parser: String,
        line_index: usize,
    },

    /// Swallow-check detector found Oracle text not represented in parsed output.
    SwallowedClause {
        detector: String,
        description: String,
        line_index: usize,
    },

    /// Cascade-diff: a cascade slot was populated but did not land on the final def.
    CascadeLoss {
        slot: CascadeSlot,
        effect_name: String,
        line_index: usize,
    },

    /// Legacy string warning not yet migrated to a typed variant.
    /// Used during dual-emit transition (D-11) for swallow_check warnings
    /// that are deferred to Plan 3.
    Legacy { message: String, line_index: usize },
}

impl OracleDiagnostic {
    /// Severity level, determined by variant (D-05).
    #[allow(dead_code)] // Used by future consumers in Plan 3.
    pub fn severity(&self) -> DiagnosticSeverity {
        match self {
            Self::TargetFallback { .. } => DiagnosticSeverity::Warning,
            Self::IgnoredRemainder { .. } => DiagnosticSeverity::Info,
            Self::SwallowedClause { .. } => DiagnosticSeverity::Warning,
            Self::CascadeLoss { .. } => DiagnosticSeverity::Warning,
            Self::Legacy { .. } => DiagnosticSeverity::Warning,
        }
    }

    /// Oracle text line index (D-06 provenance).
    #[allow(dead_code)] // Used by future consumers in Plan 3.
    pub fn line_index(&self) -> usize {
        match self {
            Self::TargetFallback { line_index, .. }
            | Self::IgnoredRemainder { line_index, .. }
            | Self::SwallowedClause { line_index, .. }
            | Self::CascadeLoss { line_index, .. }
            | Self::Legacy { line_index, .. } => *line_index,
        }
    }
}

/// Display impl produces backward-compatible string format matching
/// the legacy `push_warning` format strings, so dual-emit parity
/// verification (D-11) can compare stringified typed diagnostics to
/// the thread-local output.
impl fmt::Display for OracleDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TargetFallback { context, text, .. } => {
                write!(f, "target-fallback: {context} '{text}'")
            }
            Self::IgnoredRemainder { text, parser, .. } => {
                write!(f, "ignored-remainder({parser}): '{text}'")
            }
            Self::SwallowedClause {
                detector,
                description,
                ..
            } => {
                write!(f, "Swallow:{detector} — {description}")
            }
            Self::CascadeLoss {
                slot, effect_name, ..
            } => {
                let slot_name = match slot {
                    CascadeSlot::Optional => "CascadeOptional",
                    CascadeSlot::OpponentMay => "CascadeOpponentMay",
                    CascadeSlot::Condition => "CascadeCondition",
                    CascadeSlot::RepeatFor => "CascadeRepeatFor",
                    CascadeSlot::PlayerScope => "CascadePlayerScope",
                    CascadeSlot::Duration => "CascadeDuration",
                };
                write!(
                    f,
                    "Swallow:{slot_name} — cascade slot lost (effect={effect_name})"
                )
            }
            Self::Legacy { message, .. } => write!(f, "{message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_mapping() {
        let diag = OracleDiagnostic::TargetFallback {
            context: "test".into(),
            text: "foo".into(),
            line_index: 0,
        };
        assert_eq!(diag.severity(), DiagnosticSeverity::Warning);

        let diag = OracleDiagnostic::IgnoredRemainder {
            text: "bar".into(),
            parser: "test".into(),
            line_index: 0,
        };
        assert_eq!(diag.severity(), DiagnosticSeverity::Info);
    }

    #[test]
    fn display_backward_compat() {
        let diag = OracleDiagnostic::TargetFallback {
            context: "parse_target could not classify".into(),
            text: "some creature".into(),
            line_index: 2,
        };
        assert_eq!(
            diag.to_string(),
            "target-fallback: parse_target could not classify 'some creature'"
        );

        let diag = OracleDiagnostic::IgnoredRemainder {
            text: "extra stuff".into(),
            parser: "must-block".into(),
            line_index: 0,
        };
        assert_eq!(
            diag.to_string(),
            "ignored-remainder(must-block): 'extra stuff'"
        );

        let diag = OracleDiagnostic::Legacy {
            message: "Swallow:DynamicQty — some issue".into(),
            line_index: 0,
        };
        assert_eq!(diag.to_string(), "Swallow:DynamicQty — some issue");
    }

    #[test]
    fn line_index_accessor() {
        let diag = OracleDiagnostic::CascadeLoss {
            slot: CascadeSlot::Condition,
            effect_name: "DealDamage".into(),
            line_index: 5,
        };
        assert_eq!(diag.line_index(), 5);
    }
}
