//! Typed Oracle parse diagnostics (Phase 50, D-04).
//!
//! Replaces thread-local `push_warning` string accumulation with
//! machine-readable diagnostics carrying severity and source provenance.

use std::fmt;
use strum::IntoEnumIterator;

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

/// Which sub-grammar rejected an unparsed clause.
///
/// CR 608.2c: the controller of a spell or ability follows its instructions in the
/// order written — a clause the parser cannot read end to end is a gap, and this type
/// names WHICH sub-grammar refused it, determined by replaying the engine's own
/// combinators over the recorded text (never by a string heuristic on its first word).
///
/// The verdict is NOT stored on the gap node. `ClauseGapKind::unimplemented_name` is
/// written into `Effect::Unimplemented.name`, and the phrase is re-derived by running
/// `diagnose_clause_gap` over the same recorded `description`. That is what makes the
/// function's context-freedom load-bearing.
///
/// The serde tag is deliberately the **same** string `ClauseGapKind::unimplemented_name`
/// writes into `Effect::Unimplemented.name`, so one `ClauseGapKind` has exactly one
/// spelling everywhere in `card-data.json`. The `rename`s are a mirror of that table, not
/// a second authority: the `EnumIter`-driven test
/// `clause_gap_serde_tag_is_the_unimplemented_name` makes drift a test failure.
#[derive(
    Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, strum::EnumDiscriminants,
)]
#[strum_discriminants(name(ClauseGapKind), derive(strum::EnumIter))]
#[serde(tag = "kind")]
pub enum ClauseGap {
    /// CR 614.1 + CR 614.1a: an event antecedent ("would …", with or without
    /// "instead") that no replacement lowering owns.
    ///
    /// The span always NAMES the event, but it is not guaranteed to be the event clause
    /// alone. Two disjoint shapes carry something else along, both consequences of the bound
    /// in `gap_diagnosis::replacement_antecedent`:
    ///
    /// - **A leading granting preamble.** When the replacement is printed inside a quoted
    ///   granted ability — `it gains "if this creature would …`, `you get an emblem with "if
    ///   you would …` — the bound is not quote-aware, so it cuts at the first comma INSIDE
    ///   the quote (making the span a PREFIX of the granted ability), and the preamble ahead
    ///   of it is not at position 0 so no leading-prefix authority can strip it.
    /// - **A trailing substitution.** When the chunk carries no `", "` at all, the bound's
    ///   documented "a clause carrying no comma is its own antecedent" fallback returns the
    ///   clause whole — so the span holds the event AND what CR 614.1a says it is replaced
    ///   with, e.g. `damage that would reduce your life total to less than N reduces it to N
    ///   instead.`
    ///
    /// Both are self-identifying over a card-data export, across every `unparsed_replacement`
    /// gap: the first carries an ODD number of `"`; the second ends at a sentence terminator.
    /// Neither predicate subsumes the other — the second shape is even-quoted, which is how it
    /// stayed unnamed while only the quote residual was documented.
    #[serde(rename = "unparsed_replacement")]
    Replacement { antecedent: String },
    /// CR 608.2c: a guard the single condition authority (`lower_instead_condition`)
    /// rejected. CR 603.4 names the trigger-side intervening-"if"; elsewhere the word
    /// still gates its clause.
    #[serde(rename = "unparsed_condition")]
    Condition { guard: String },
    /// CR 608.2h: a dynamic amount — the answer is determined only once, when the
    /// effect is applied — whose operand the quantity authorities rejected.
    #[serde(rename = "unparsed_quantity")]
    Quantity { operand: String },
    /// A clause-head verb the imperative dispatcher recognises, whose argument grammar
    /// rejected the rest of the clause.
    #[serde(rename = "unparsed_verb_arguments")]
    VerbArguments { verb: String, arguments: String },
    /// A clause head that neither the verb vocabulary nor the subject grammar recognised.
    #[serde(rename = "unrecognized_clause_head")]
    UnrecognizedHead { head: String },
}

impl ClauseGap {
    /// The verdict's kind, i.e. the wire-name authority's key for this gap.
    pub fn kind(&self) -> ClauseGapKind {
        self.into()
    }

    /// The Oracle phrase this verdict rejected.
    ///
    /// Exhaustive with no wildcard, so a sixth variant must declare its phrase. For
    /// `VerbArguments` the phrase is the ARGUMENTS: the verb is the half the imperative
    /// dispatcher recognised, so it is not what failed.
    pub fn phrase(&self) -> &str {
        match self {
            Self::Replacement { antecedent } => antecedent,
            Self::Condition { guard } => guard,
            Self::Quantity { operand } => operand,
            Self::VerbArguments { arguments, .. } => arguments,
            Self::UnrecognizedHead { head } => head,
        }
    }
}

impl ClauseGapKind {
    /// The stable `Effect::Unimplemented.name` this verdict is recorded under.
    ///
    /// This IS the single authority, in the exact sense
    /// `OracleSemanticFeature::detector_label` is for swallow detectors: every clause
    /// gap the parser records goes through `gap_diagnosis::clause_gap_unimplemented`,
    /// which names it through this function and never as a string literal. That is what
    /// makes the pin test below a pin on the WIRE FORMAT rather than merely on this
    /// table — these strings reach `card-data.json`, the coverage report's
    /// `Effect:<name>` handler keys, and the in-game unimplemented-mechanics badge.
    ///
    /// The match is exhaustive with no wildcard, so a sixth variant cannot ship unnamed.
    pub fn unimplemented_name(self) -> &'static str {
        match self {
            Self::Replacement => "unparsed_replacement",
            Self::Condition => "unparsed_condition",
            Self::Quantity => "unparsed_quantity",
            Self::VerbArguments => "unparsed_verb_arguments",
            Self::UnrecognizedHead => "unrecognized_clause_head",
        }
    }

    /// The inverse of [`Self::unimplemented_name`]: decode a recorded gap name back to
    /// its verdict kind, or `None` when the name is a *category* key minted by some
    /// other producer (`instead_condition`, `prevent`, `choose`, `unknown`, …).
    ///
    /// This decodes the engine's own wire key. It never parses Oracle text — the phrase
    /// half is re-derived by running `diagnose_clause_gap` over the recorded description.
    pub fn from_unimplemented_name(name: &str) -> Option<Self> {
        Self::iter().find(|k| k.unimplemented_name() == name)
    }
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
        /// The phrase this finding's own axis rejected, when that axis names one.
        ///
        /// `None` is not "we did not look", but it is not one thing either. There are
        /// THREE cases, and which of them can occur depends on the detector:
        ///
        /// 1. **No axis wired** — the detector passes `None` unconditionally. This does NOT
        ///    mean no phrase is nameable: `Replacement` is such a detector, and its axis names
        ///    phrases perfectly well — `Replacement_Instead` runs that same
        ///    `SwallowedAxis::Replacement` and mints antecedents from it. A consumer that
        ///    reads case 1 as "nothing here was nameable" reads those records backwards. The
        ///    CORPUS-PRESENT detectors in this case are enumerated by `EXPECTED_NON_PHRASE` in
        ///    `swallow_check`'s tests. It is not the whole list of such detectors, and says so:
        ///    its own doc names the two more that pass `None` but raise no corpus warning, so
        ///    no fixture can reach them.
        ///    Read that constant's name as "no gap argument at the push site" and nothing
        ///    wider. `Replacement` is on the list, and it gates on the CR 614.1b "skip" and
        ///    CR 614.1c "enters" replacement templates. It is there because this phase wired
        ///    the gap argument at five push sites, the detectors in `PHRASE_BEARING`, and not
        ///    because its own axis is structural.
        /// 2. **Accepted** — a phrase-bearing axis whose authority found candidate phrases
        ///    and ACCEPTED all of them. The clause was dropped for want of a CARRIER rather
        ///    than for want of a grammar.
        /// 3. **No candidate** — a phrase-bearing axis that found NOTHING to judge, because
        ///    the detector's gate and the extractor's markers are not the same test. The
        ///    clause was dropped for want of a grammar the axis cannot even name.
        ///
        /// Cases 2 and 3 carry OPPOSITE triage actions, and a consumer that assumes 2 will
        /// read case 3 exactly backwards. They are not distinguished on the wire today —
        /// see the type note below.
        ///
        /// # Which case a given `None` belongs to is NOT determinable from this record
        ///
        /// Nothing serialized here distinguishes them, and the text predicates tried so far do
        /// not recover the distinction. Whether a candidate phrase existed depends on the
        /// EXTRACTOR's parse, not on the presence of a word: a detector gate and its
        /// extractor are two different tests over two different notions of "occurs", and the
        /// gap between them is not expressible as a search over `oracle_text`.
        ///
        /// Spelltwine is a worked counterexample to the obvious proxy, and it is not alone —
        /// Furygale Flocking, Illusionist's Gambit and Rotted Ones, Lay Siege share the shape,
        /// and that list is an illustration rather than a census. Spelltwine's report is
        /// `Condition_If` with no gap. Its only bare `if` is the `if able` in "Cast the copies
        /// if able without paying their mana costs", and `trailing_guard` tries `if able`
        /// BEFORE `if `, so the scan takes a `Skip` and never mints a `Guard(If)` — no
        /// candidate was judged, which is case 3. A proxy that asks whether `if ` occurs at a
        /// word start answers "yes" and concludes case 2. The proxy is not merely approximate,
        /// it is confidently wrong, and re-running it reproduces the same wrong answer.
        ///
        /// Nor is a smarter proxy the answer. Deciding the case requires replaying the
        /// extractor's scan, including that a `Skip` ADVANCES rather than ends it — a card
        /// whose first `if` is `if able` may still mint a `Guard(If)` later in the same text,
        /// so even "which arm matches first" gets it wrong. Khârn the Betrayer is that case
        /// in the corpus: its first bare `if` is the `if able` in "attacks or blocks each
        /// combat if able", and it still reports a `Condition_If` gap, minted from the "If
        /// damage would be dealt to Khârn the Betrayer" on a later line.
        ///
        /// So this doc deliberately ships no per-axis table and no regenerating predicate.
        /// Both would be claims about relationships between a detector and an extractor that
        /// nothing checks, in a comment that cannot fail when they drift.
        ///
        /// A consumer that must ACT differently on case 2 versus case 3 therefore cannot get
        /// what it needs from this field, and should not infer it — the distinction has to be
        /// made typed at the point where it is decided. That is deliberately not done here:
        /// it reaches the constructor, every swallow-detector call site, the wire shape and
        /// its back-compat default, and the coverage consumer. Folding it into the pending
        /// consolidation of this field with `game::coverage::GapDetail` pays that migration
        /// once instead of twice.
        ///
        /// Not re-derivable from `description`: that field is `truncate(original, 140)`,
        /// so on any unit longer than the bound the phrase can lie past the recorded text
        /// and no consumer can recover it. The affected share is a property of the corpus,
        /// not of this type, and is not quoted here — re-measure it over a card-data export,
        /// counted per `detector`, with the predicate "`description` is AT the truncation
        /// bound in BYTES". Classify against the audit unit, which is a LINE-block, not the
        /// whole card. That predicate is a LOWER BOUND, not an exact census, and the shape it
        /// misses is its own: `truncate` walks its end index down with `str::is_char_boundary`,
        /// so a cut landing inside a multi-byte character yields a string SHORT of the bound,
        /// by up to the three trailing bytes of a four-byte scalar — and such a row is not AT
        /// the bound, so this predicate does not find it. Chaos Channeler is exactly that
        /// missed row, and naming it is not the same as counting it: 138 bytes, cut
        /// immediately before the em-dash of "10—19". Widening the predicate to "within three
        /// bytes below the bound" is NOT the fix either: that band also holds rows that merely
        /// ran out of text or stopped at a line break, and those are not truncated at all.
        /// This is the load-bearing difference from the clause-gap path, where
        /// `diagnose_clause_gap`'s context-freedom lets a consumer re-derive the verdict from
        /// the description.
        ///
        /// `skip_serializing_if` is LOAD-BEARING, not decoration: the committed
        /// `combat_celebrant` snapshots embed a serialized `SwallowedClause` whose
        /// detector carries no phrase, and they must stay byte-identical.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        gap: Option<ClauseGap>,
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
    ///
    /// `gap` is passed by the detector rather than stamped afterwards, because unlike
    /// provenance the phrase is evidence the DETECTOR holds — `detect_condition_if`'s
    /// post-exemption text exists nowhere outside that function.
    pub(crate) fn swallowed_clause(
        detector: impl Into<String>,
        description: impl Into<String>,
        gap: Option<ClauseGap>,
    ) -> Self {
        Self::SwallowedClause {
            detector: detector.into(),
            description: description.into(),
            line_index: 0,
            unit_span: None,
            items: Vec::new(),
            gap,
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

    /// The rejected phrase, for a `SwallowedClause` whose axis named one. `None` for every
    /// other variant — they are parse-time diagnostics with no audited axis.
    pub fn gap(&self) -> Option<&ClauseGap> {
        match self {
            Self::SwallowedClause { gap, .. } => gap.as_ref(),
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
            gap: None,
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
        let diag = OracleDiagnostic::swallowed_clause("Optional_YouMay", "you may draw", None);
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
        // An axis that named no phrase must serialize to EXACTLY today's shape.
        // `skip_serializing_if` is what buys this; dropping it rewrites every committed
        // snapshot and every already-exported payload.
        assert!(
            !obj.contains_key("gap"),
            "absent gap must not be written; got {obj:?}"
        );
        assert_eq!(obj["line_index"], 0);
        // A serialization failure must not read as a pass: these two keys prove the
        // object really is the diagnostic and not an empty map.
        assert_eq!(obj["detector"], "Optional_YouMay");
        assert!(obj.contains_key("description"));
    }

    /// The gap-bearing shape survives a serde round-trip, and the nested
    /// internally-tagged enum lands under the key the wire-name authority dictates.
    #[test]
    fn gap_bearing_diagnostic_round_trips() {
        let diag = OracleDiagnostic::SwallowedClause {
            detector: "Condition_If".into(),
            description: "if you do, draw a card".into(),
            line_index: 1,
            unit_span: Some(OracleSourceSpan::exact(1, 2, 14, 61, 0)),
            items: vec![OracleItemId(3)],
            gap: Some(ClauseGap::Condition {
                guard: "you sang a song".to_string(),
            }),
        };

        let json = serde_json::to_string(&diag).expect("serialize");
        let back: OracleDiagnostic = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, diag);

        let value: serde_json::Value = serde_json::from_str(&json).expect("as value");
        let obj = value.as_object().expect("object");
        assert!(obj.contains_key("gap"), "gap key must be present: {obj:?}");
        assert_eq!(obj["gap"]["kind"], "unparsed_condition");
        assert_eq!(obj["gap"]["guard"], "you sang a song");
    }

    /// A payload written before this field existed still deserializes, and
    /// `gap` defaults to `None` rather than failing the parse. The record below is a real
    /// `SwallowedClause` from the phase-base export, keys and all.
    #[test]
    fn old_payload_without_gap_still_deserializes() {
        let old = r#"{
            "type": "SwallowedClause",
            "detector": "Condition_If",
            "description": "Whenever Aggressive Detective attacks, if all your commanders have been revealed",
            "line_index": 0,
            "unit_span": {
                "first_line": 0,
                "last_line": 0,
                "start_byte": 0,
                "end_byte": 134,
                "precision": "Exact",
                "ordinal_within_span": 0
            },
            "items": [0]
        }"#;
        let diag: OracleDiagnostic = serde_json::from_str(old).expect("pre-gap payload");

        assert!(diag.gap().is_none(), "absent gap must default to None");
        // A total parse failure must not be mistaken for the `None`: every sibling field
        // has to have survived.
        assert_eq!(diag.line_index(), 0);
        assert_eq!(diag.category_name(), "swallowed-clause");
        let span = diag.unit_span().expect("span survived");
        assert_eq!((span.first_line, span.last_line), (0, 0));
        assert_eq!(diag.evidence_items(), &[OracleItemId(0)]);
    }

    /// The serde tag and `unimplemented_name` are ONE spelling, for every variant.
    /// `EnumIter`-driven, so a sixth variant cannot slip past an array length.
    #[test]
    fn clause_gap_serde_tag_is_the_unimplemented_name() {
        use ClauseGapKind as K;

        // One constructed value per kind. The match is exhaustive with no wildcard, so a
        // new variant forces a new row here rather than silently skipping the check.
        let sample = |kind: K| -> ClauseGap {
            match kind {
                K::Replacement => ClauseGap::Replacement {
                    antecedent: "x would die".to_string(),
                },
                K::Condition => ClauseGap::Condition {
                    guard: "you control a dragon".to_string(),
                },
                K::Quantity => ClauseGap::Quantity {
                    operand: "the excess".to_string(),
                },
                K::VerbArguments => ClauseGap::VerbArguments {
                    verb: "deal".to_string(),
                    arguments: "that damage to it instead".to_string(),
                },
                K::UnrecognizedHead => ClauseGap::UnrecognizedHead {
                    head: "bolster".to_string(),
                },
            }
        };

        let mut tags: Vec<String> = Vec::new();
        for kind in K::iter() {
            let value: serde_json::Value =
                serde_json::to_value(sample(kind)).expect("serialize clause gap");
            let tag = value["kind"].as_str().expect("tag is a string").to_string();
            assert_eq!(
                tag,
                kind.unimplemented_name(),
                "{kind:?} serializes under a tag the wire-name authority does not name"
            );
            tags.push(tag);
        }

        // Per-kind coverage is STRUCTURAL, not asserted: `K::iter()` visits every
        // discriminant, and `sample`'s match is exhaustive with no wildcard, so a new
        // variant fails to compile until it has a row. A counter incremented once per
        // iteration and compared against the iterator's own length could not fail under any
        // code change, so it is not written here.
        let distinct: std::collections::BTreeSet<&str> = tags.iter().map(String::as_str).collect();
        assert_eq!(
            distinct.len(),
            tags.len(),
            "two kinds serialize under one tag: {tags:?}"
        );
    }

    /// V8 — the clause-gap wire format. These five strings are written into
    /// `Effect::Unimplemented.name`, exported in `card-data.json`, and read back by
    /// `from_unimplemented_name`; a rename silently reclassifies every gap node in the
    /// corpus. Modelled line-for-line on
    /// `feature::detector_labels_are_distinct_and_pin_the_exported_wire_format`.
    #[test]
    fn clause_gap_names_are_distinct_and_pin_the_exported_wire_format() {
        use ClauseGapKind as K;

        // Hand-written on purpose: it is the pin. A generated table would only ever
        // agree with whatever the code says.
        let expected = [
            (K::Replacement, "unparsed_replacement"),
            (K::Condition, "unparsed_condition"),
            (K::Quantity, "unparsed_quantity"),
            (K::VerbArguments, "unparsed_verb_arguments"),
            (K::UnrecognizedHead, "unrecognized_clause_head"),
        ];
        for (kind, name) in expected {
            assert_eq!(kind.unimplemented_name(), name);
            assert_eq!(K::from_unimplemented_name(name), Some(kind));
        }

        // Exhaustiveness comes from `EnumIter`, not from the array's length: a sixth
        // variant must declare a name (the match forces that) AND a DISTINCT one, which
        // only iterating the enum can check.
        let every: Vec<K> = K::iter().collect();
        assert_eq!(
            every.len(),
            expected.len(),
            "a variant was added without pinning its exported name above"
        );
        let distinct: std::collections::BTreeSet<&str> =
            every.iter().map(|k| k.unimplemented_name()).collect();
        assert_eq!(
            distinct.len(),
            every.len(),
            "every clause-gap kind must map to a DISTINCT name: a collapse rewrites the \
             wire format and destroys per-kind gap attribution"
        );

        // Negative decodes: a CATEGORY key minted by some other producer must not decode
        // as a clause-gap verdict. Measured at the phase base: no name in the exported
        // corpus collides with the five above (M6).
        for category_key in [
            "deal",
            "unknown",
            "instead_condition",
            "empty",
            "choose",
            "prevent",
        ] {
            assert_eq!(
                K::from_unimplemented_name(category_key),
                None,
                "{category_key} is a category key, not a clause-gap verdict"
            );
        }
    }

    /// The discriminant is derived from the payload, never stored beside it.
    #[test]
    fn clause_gap_kind_is_derived_from_the_payload() {
        assert_eq!(
            ClauseGap::Quantity {
                operand: "the excess".to_string(),
            }
            .kind(),
            ClauseGapKind::Quantity
        );
        assert_eq!(
            ClauseGap::VerbArguments {
                verb: "deal".to_string(),
                arguments: "that damage to it instead".to_string(),
            }
            .kind(),
            ClauseGapKind::VerbArguments
        );
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
