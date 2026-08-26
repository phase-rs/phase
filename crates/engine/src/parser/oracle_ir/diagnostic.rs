//! Typed Oracle parse diagnostics (Phase 50, D-04).
//!
//! Replaces thread-local `push_warning` string accumulation with
//! machine-readable diagnostics carrying severity and source provenance.

use std::fmt;

// Source-identity types live in `doc` (the document IR that mints them). They are
// re-exported here because they are part of this module's PUBLIC wire payload:
// `SwallowedClause` carries them into `CardFace::parse_warnings` → `card-data.json`,
// and `doc` is a crate-private module, so without this a consumer outside the crate
// could deserialize the diagnostic but could not name the types inside it.
pub use super::doc::{OracleItemId, OracleSourceSpan, SpanPrecision};

/// Severity level for parse diagnostics (D-05).
/// Derived from the variant — not stored as a field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    ///
    /// # Provenance is UNIT-scoped, and that is the honest scope — not a shortcut
    ///
    /// The swallow audit's question is per-**audit unit**: *does the Oracle text of this
    /// unit raise a semantic expectation that the parse of this same unit does not
    /// represent?* An audit unit is a block of source lines owning `0..N` items, and
    /// `scope_to_unit` pools **all** of those items as the evidence half — deliberately,
    /// because a sibling's evidence legitimately satisfies a neighbour's expectation
    /// (`Kicker {2}{G}` emits a `Keyword` item *and* an `AdditionalCost` item on one
    /// line; the latter answers the expectation the former's text raises).
    ///
    /// Two consequences the payload must not lie about:
    ///
    /// 1. **No single `OracleItemId`.** The audit cannot attribute a finding to one item
    ///    — the evidence is the union. `items` is therefore the unit's whole evidence
    ///    set, honestly `0..N`. It is legitimately EMPTY for a card that lowered to no
    ///    items at all (Chorus of the Conclave), which is the case the audit exists for.
    /// 2. **No `OracleUnitId`.** `OracleUnitId` is `{item, ordinal}` — item-scoped by
    ///    construction. An audit unit spanning `0..N` items has no such id, and minting
    ///    one from `items[0]` would name a line-block by one item's header unit: a
    ///    precise-looking wrong answer.
    ///
    /// `unit_span` locates the unit exactly; it is LINE-GRANULAR today (see
    /// `OracleSourceSpan`), so two clauses on one physical line SHARE it. That collapse
    /// is pinned by name in `swallow_check`'s tests and lifts when items gain sub-line
    /// spans.
    ///
    /// `line_index` is retained: it is the unit's first ITEM line, it predates this
    /// payload, and it is what every existing consumer reads. Invariant:
    /// `unit_span.first_line <= line_index <= unit_span.last_line` (they differ when a
    /// unit absorbs leading lines no item claimed).
    SwallowedClause {
        detector: String,
        description: String,
        line_index: usize,
        /// The audit unit's card-absolute byte range. `None` on a payload written before
        /// provenance existed — deliberately an `Option` rather than a defaulted span,
        /// because a zeroed span does not read as "unattributed", it reads as *line 1,
        /// bytes 0..0, exactly located*. It is also the state a detector emits in: the
        /// audit loop stamps provenance afterwards (see `stamp_provenance`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit_span: Option<OracleSourceSpan>,
        /// Every item the unit pooled as evidence. Empty on a pre-provenance payload AND
        /// on a genuinely item-less unit — `unit_span.is_none()` is the discriminator
        /// between the two.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        items: Vec<OracleItemId>,
    },

    /// Cascade-diff: a cascade slot was populated but did not land on the final def.
    CascadeLoss {
        slot: CascadeSlot,
        effect_name: String,
        line_index: usize,
    },
}

impl OracleDiagnostic {
    /// The single authority for constructing a swallow-audit finding.
    ///
    /// Emitted UNATTRIBUTED — `line_index: 0`, no span, no items — because a detector
    /// knows *evidence*, not provenance: it is handed one unit's text and one unit's
    /// definitions and cannot see which unit that was. The audit loop that scoped the
    /// unit is the only thing that can attribute the finding, and it does so via
    /// `stamp_provenance`. Routing every construction through here (rather than 28
    /// struct literals) means a future provenance field cannot be silently defaulted at
    /// one forgotten call site — the same reason `Effect::unimplemented` exists.
    pub(crate) fn swallowed_clause(
        detector: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        Self::SwallowedClause {
            detector: detector.into(),
            description: description.into(),
            line_index: 0,
            unit_span: None,
            items: Vec::new(),
        }
    }

    /// The audit unit's source span, for a `SwallowedClause` that has been stamped.
    ///
    /// `None` for every other variant (they are parse-time diagnostics that carry no unit
    /// — they never pass through the audit) and for a pre-provenance payload.
    pub fn unit_span(&self) -> Option<&OracleSourceSpan> {
        match self {
            Self::SwallowedClause { unit_span, .. } => unit_span.as_ref(),
            Self::TargetFallback { .. }
            | Self::IgnoredRemainder { .. }
            | Self::CascadeLoss { .. } => None,
        }
    }

    /// The items the audit unit pooled as evidence. Empty for every other variant.
    pub fn evidence_items(&self) -> &[OracleItemId] {
        match self {
            Self::SwallowedClause { items, .. } => items,
            Self::TargetFallback { .. }
            | Self::IgnoredRemainder { .. }
            | Self::CascadeLoss { .. } => &[],
        }
    }

    /// Severity level, determined by variant (D-05).
    pub fn severity(&self) -> DiagnosticSeverity {
        match self {
            Self::TargetFallback { .. } => DiagnosticSeverity::Warning,
            Self::IgnoredRemainder { .. } => DiagnosticSeverity::Info,
            Self::SwallowedClause { .. } => DiagnosticSeverity::Warning,
            Self::CascadeLoss { .. } => DiagnosticSeverity::Warning,
        }
    }

    /// Oracle text line index (D-06 provenance).
    pub fn line_index(&self) -> usize {
        match self {
            Self::TargetFallback { line_index, .. }
            | Self::IgnoredRemainder { line_index, .. }
            | Self::SwallowedClause { line_index, .. }
            | Self::CascadeLoss { line_index, .. } => *line_index,
        }
    }

    /// Diagnostic category name for regression tracking (D-08).
    pub fn category_name(&self) -> &'static str {
        match self {
            Self::TargetFallback { .. } => "target-fallback",
            Self::IgnoredRemainder { .. } => "ignored-remainder",
            Self::SwallowedClause { .. } => "swallowed-clause",
            Self::CascadeLoss { .. } => "cascade-loss",
        }
    }
}

/// Display impl uses structured [severity:category] prefix format (D-11).
impl fmt::Display for OracleDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity() {
            DiagnosticSeverity::Error => "error",
            DiagnosticSeverity::Warning => "warning",
            DiagnosticSeverity::Info => "info",
        };
        let category = self.category_name();
        match self {
            Self::TargetFallback { context, text, .. } => {
                write!(f, "[{severity}:{category}] {context} '{text}'")
            }
            Self::IgnoredRemainder { text, parser, .. } => {
                write!(f, "[{severity}:{category}] ({parser}) '{text}'")
            }
            Self::SwallowedClause {
                detector,
                description,
                ..
            } => {
                write!(f, "[{severity}:{category}] {detector} — {description}")
            }
            Self::CascadeLoss {
                slot, effect_name, ..
            } => {
                write!(
                    f,
                    "[{severity}:{category}] {slot:?} lost (effect={effect_name})"
                )
            }
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

    /// A stamped diagnostic, as the audit emits one.
    fn stamped() -> OracleDiagnostic {
        OracleDiagnostic::SwallowedClause {
            detector: "Condition_If".into(),
            description: "if you do, draw a card".into(),
            line_index: 1,
            unit_span: Some(OracleSourceSpan::exact(1, 2, 14, 61, 0)),
            items: vec![OracleItemId(3), OracleItemId(4)],
        }
    }

    /// Plan 02 step 6, half 1: the NEW shape survives a serde round-trip intact.
    #[test]
    fn new_provenance_shape_round_trips() {
        let diag = stamped();
        let json = serde_json::to_string(&diag).expect("serialize");
        let back: OracleDiagnostic = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, diag);

        let span = back
            .unit_span()
            .expect("stamped diagnostic carries a unit span");
        assert_eq!((span.first_line, span.last_line), (1, 2));
        assert_eq!((span.start_byte, span.end_byte), (14, 61));
        assert_eq!(span.precision, SpanPrecision::Exact);
        assert_eq!(back.evidence_items(), &[OracleItemId(3), OracleItemId(4)]);
    }

    /// Plan 02 step 6, half 2: the OLD `{detector, description, line_index}` payload —
    /// the shape sitting in every already-exported `card-data.json` — still deserializes.
    /// The new fields default to "unattributed" rather than to a zeroed span.
    #[test]
    fn old_payload_without_provenance_still_deserializes() {
        let old = r#"{
            "type": "SwallowedClause",
            "detector": "Replacement",
            "description": "as ~ enters, choose a creature type",
            "line_index": 2
        }"#;
        let diag: OracleDiagnostic = serde_json::from_str(old).expect("legacy payload");

        assert_eq!(diag.line_index(), 2);
        assert_eq!(diag.category_name(), "swallowed-clause");
        // Absent, NOT zeroed: a pre-provenance payload must not claim line 1, bytes 0..0.
        assert!(diag.unit_span().is_none());
        assert!(diag.evidence_items().is_empty());
    }

    /// The other compatibility direction: an UNATTRIBUTED diagnostic serializes to
    /// exactly the old three-field shape, so new code writing one an old reader can
    /// still parse is possible. (`skip_serializing_if` is what buys this.)
    #[test]
    fn unattributed_diagnostic_serializes_to_the_old_shape() {
        let diag = OracleDiagnostic::swallowed_clause("Optional_YouMay", "you may draw");
        let json: serde_json::Value = serde_json::to_value(&diag).expect("serialize unattributed");

        let obj = json.as_object().expect("object");
        assert!(
            !obj.contains_key("unit_span"),
            "absent span must not be written"
        );
        assert!(
            !obj.contains_key("items"),
            "empty evidence must not be written"
        );
        assert_eq!(obj["line_index"], 0);
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
