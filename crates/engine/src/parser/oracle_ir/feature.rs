//! Typed per-unit semantic feature vocabulary for the swallow audit.
//!
//! The swallow audit asks one question per source unit: *does the Oracle text of
//! this unit raise a semantic expectation that the parsed output for **this same
//! unit** does not represent?* This module owns the vocabulary that question is
//! asked in — the closed feature set, the per-unit audit record, and the
//! item-scoped view of the lowered definitions that supplies the evidence.
//!
//! # Why the evidence is a *lowered* definition rather than sourced IR
//!
//! Plan 02 step 1 was written expecting to visit `OracleNodeIr::{Spell, Trigger,
//! Static, Replacement}` — the IR-carrying node variants — before lowering
//! discards their nested provenance. Those four variants have **no production
//! constructors**: every item the dispatch loop emits is a `PreLowered*` node
//! carrying an already-lowered engine definition (`DocEmitter` in `oracle.rs` is
//! the only emitter, and it emits nothing else). There is no sourced IR to visit,
//! so the audit visits the lowered definition that the item actually produced.
//!
//! # Why the audit runs *after* the relation passes
//!
//! Pre-lowering auditing is blind to relation-synthesized semantics; the
//! false-positive wave U1 bounded to 31 faces would be caused, not avoided.
//! `apply_linked_choice_etb_counter` **synthesizes a replacement**, so an audit
//! running ahead of it would report that replacement as swallowed on exactly the
//! cross-item cards the relations exist to model. The audit therefore runs at its
//! pinned post-relation position, and resolves each definition back to its owning
//! item through the parallel `_ids` tracks `lower_oracle_ir` already maintains —
//! tracks that are kept in sync *across* relation synthesis (see the `&mut
//! replacement_ids` parameter on `apply_linked_choice_etb_counter`).
//!
//! # Granularity
//!
//! Every unit key minted here is an item's **header unit** (`ordinal == 0`), which
//! is document-unique and carries an `Exact` span. Sub-item units (two clauses on
//! one line; mode A vs mode B inside one modal item) are **not** expressible
//! today: `ClauseIrBuilder` mints its clause ids against a fresh, throwaway
//! `OracleDocBuilder`, so every chain restarts at `OracleItemId(0)` and clause
//! `OracleUnitId`s are not document-unique. Restoring sub-item granularity is the
//! recognizer bring-up plan's job. This module is keyed by `OracleUnitId` rather
//! than `OracleItemId` precisely so that landing it requires no API change here.

// NOTE ON `dead_code`: suppression here is **per item**, never module-wide — the
// convention `doc.rs` established after a blanket `#![allow(dead_code)]` hid two
// silent defects during review. Each allow below names the commit that gives the
// item a production caller, and dies with it. The consumer is the `swallow_check`
// cutover (this plan's next commit), which re-scopes the audit from the card to
// the item and emits through this vocabulary.
use std::collections::BTreeSet;

use super::doc::OracleUnitSource;
use crate::types::ability::{
    AbilityDefinition, ModalChoice, ReplacementDefinition, StaticDefinition, TriggerDefinition,
};
use crate::types::keywords::Keyword;

/// A semantic that Oracle text can raise an expectation for, and that the parsed
/// output can be checked to represent.
///
/// Closed and parameter-free on purpose: a stringly feature name would put the
/// audit back on the substring channel this module exists to remove. The three
/// duration detectors and the two optionality detectors of the previous
/// text-and-JSON audit collapse into `Duration` and `Optional` respectively;
/// `Replacement` is net-new (nothing in the previous audit emitted it standalone).
///
/// `Unimplemented` is deliberately **not** a feature. An explicit unsupported node
/// is not a semantic the text asked for — it is the parser admitting it dropped
/// one — so it is recorded separately as an [`UnsupportedObservation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
pub(crate) enum OracleSemanticFeature {
    /// CR 614: an event-modifying effect exists.
    Replacement,
    /// CR 614.1: the modification is an *instead* substitution, not an addition.
    ReplacementInstead,
    /// CR 602.5: activation is restricted to a timing window.
    ActivationTimingRestriction,
    /// CR 602.5: activation is capped by a usage count.
    ActivationLimit,
    /// CR 611.2: the effect has a temporal scope.
    Duration,
    /// CR 603.5 / CR 609.3: the effect (or a nested one) is optional.
    Optional,
    /// CR 107.3: a quantity is computed from game state rather than fixed.
    DynamicQuantity,
    /// CR 608.2c: a conditional guard ("if", "only if").
    Condition,
    /// CR 608.2c: an *unless* guard — distinct from a plain condition because it
    /// inverts, and because it usually carries a payment.
    UnlessCondition,
    /// CR 611.3: an *as long as* continuous gate.
    AsLongAsCondition,
    /// CR 101.4 + CR 800.4: an explicit turn-order start for a multiplayer
    /// iteration. Note that a bare player scope is **not** an ordering fact.
    ApnapOrdering,
    /// CR 700.2: a modal choice whose maximum is dynamic.
    ModalDynamicMaximum,
}

#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
impl OracleSemanticFeature {
    /// The stable detector label this feature is reported under.
    ///
    /// These strings are the wire format of `OracleDiagnostic::SwallowedClause`
    /// and appear in exported `parse_warnings`, so they are deliberately unchanged
    /// from the previous audit's labels — a rename here is a silent breaking
    /// change to every downstream consumer of the coverage report.
    ///
    /// The mapping is many-to-one on the *source* side (three duration phrasings,
    /// two optionality phrasings) but one-to-one here: the label names the
    /// feature, not the phrase that raised it.
    pub(crate) fn detector_label(self) -> &'static str {
        match self {
            Self::Replacement => "Replacement",
            Self::ReplacementInstead => "Replacement_Instead",
            Self::ActivationTimingRestriction => "ActivateOnlyDuring",
            Self::ActivationLimit => "ActivateLimit",
            Self::Duration => "Duration",
            Self::Optional => "Optional_YouMay",
            Self::DynamicQuantity => "DynamicQty",
            Self::Condition => "Condition_If",
            Self::UnlessCondition => "Condition_Unless",
            Self::AsLongAsCondition => "Condition_AsLongAs",
            Self::ApnapOrdering => "APNAP",
            Self::ModalDynamicMaximum => "Modal_DynamicMaxDropped",
        }
    }
}

/// An explicit `Effect::Unimplemented` owned by one source unit.
///
/// Recorded separately from the feature sets because it is not evidence *for* a
/// semantic — it is a declaration that this unit's text was not represented at
/// all. It suppresses duplicate swallowed-clause reporting for **its own unit
/// only**; a sibling unit's expectations are still audited.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
pub(crate) struct UnsupportedObservation {
    pub(crate) source: OracleUnitSource,
    pub(crate) fragment: String,
}

/// One source unit's audit: what its text asked for, what its parse delivered,
/// and whether it explicitly gave up.
///
/// Expectations, evidence, and unsupported declarations are three separate fields
/// and are never conflated. In particular there is **no card-wide or item-wide
/// `has_unimplemented` boolean**: the previous audit's card-wide suppression gate
/// silenced every detector on 2,563 faces, and its hand-written walker leaked
/// through a `_ => false` wildcard besides. Suppression is now a property of the
/// unit that owns the unsupported node, and of nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
pub(crate) struct UnitSemanticAudit {
    pub(crate) source: OracleUnitSource,
    pub(crate) expected: BTreeSet<OracleSemanticFeature>,
    pub(crate) observed: BTreeSet<OracleSemanticFeature>,
    pub(crate) unsupported: Vec<UnsupportedObservation>,
}

#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
impl UnitSemanticAudit {
    pub(crate) fn new(source: OracleUnitSource) -> Self {
        Self {
            source,
            expected: BTreeSet::new(),
            observed: BTreeSet::new(),
            unsupported: Vec::new(),
        }
    }

    /// Features this unit's text asked for and its parse did not deliver.
    ///
    /// Empty when the unit owns an `Effect::Unimplemented`: the gap is already
    /// reported explicitly (and fails coverage on its own), so reporting it a
    /// second time as a swallowed clause would double-count one defect. This is
    /// the *entire* suppression rule — it is scoped to this unit and cannot reach
    /// a sibling.
    pub(crate) fn swallowed(&self) -> Vec<OracleSemanticFeature> {
        if !self.unsupported.is_empty() {
            return Vec::new();
        }
        self.expected.difference(&self.observed).copied().collect()
    }
}

/// The lowered definitions owned by exactly one document item.
///
/// This is the evidence side of one unit's audit. It is a borrowed *view*, not a
/// copy: the definitions live in the `ParsedAbilities` the fold produced, and are
/// resolved back to their owning item through `lower_oracle_ir`'s parallel `_ids`
/// tracks — which is what lets the audit run after the relation passes have
/// mutated and synthesized into those same vectors.
///
/// Every existing typed evidence walker (`def_tree_has_duration`,
/// `static_definition_has_optional`, …) is reused verbatim against this view. The
/// walkers were never the defect; their **card-wide scope** was. Scoping them to
/// the owning item is what makes the audit honest, and it is why the three
/// previously-silent detectors can fire: a card that drops an activation limit on
/// line 3 is no longer excused by an unrelated restriction on line 1.
#[derive(Debug, Default)]
#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
pub(crate) struct ItemDefs<'a> {
    pub(crate) abilities: Vec<&'a AbilityDefinition>,
    pub(crate) triggers: Vec<&'a TriggerDefinition>,
    pub(crate) statics: Vec<&'a StaticDefinition>,
    pub(crate) replacements: Vec<&'a ReplacementDefinition>,
    pub(crate) keywords: Vec<&'a Keyword>,
    pub(crate) modal: Option<&'a ModalChoice>,
}

#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
impl ItemDefs<'_> {
    /// True when this item produced no lowered definition at all.
    ///
    /// Such an item raises expectations from its text but can never satisfy them,
    /// so it would warn on every marker it contains. That is correct for a line
    /// the parser silently dropped, and wrong for a line that legitimately
    /// contributes no definition (a keyword-only line folded into a cost, say) —
    /// which is why the caller checks this rather than the audit assuming.
    pub(crate) fn is_empty(&self) -> bool {
        self.abilities.is_empty()
            && self.triggers.is_empty()
            && self.statics.is_empty()
            && self.replacements.is_empty()
            && self.keywords.is_empty()
            && self.modal.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::doc::{OracleDocBuilder, OracleSourceSpan};

    fn unit_source() -> OracleUnitSource {
        let mut b = OracleDocBuilder::new();
        let slot = b.begin_item(OracleSourceSpan::exact(0, 0, 0, 10, 0), Some("Flying"));
        slot.source().clone()
    }

    #[test]
    fn swallowed_is_expected_minus_observed() {
        let mut audit = UnitSemanticAudit::new(unit_source());
        audit.expected.insert(OracleSemanticFeature::Duration);
        audit.expected.insert(OracleSemanticFeature::Optional);
        audit.observed.insert(OracleSemanticFeature::Optional);
        assert_eq!(audit.swallowed(), vec![OracleSemanticFeature::Duration]);
    }

    /// The whole point of the per-unit model: an unsupported node silences ONLY
    /// the unit that owns it. This asserts the suppression is scoped, not global —
    /// the card-wide gate this replaces silenced every detector on 2,563 faces.
    #[test]
    fn unsupported_suppresses_only_its_own_unit() {
        let source = unit_source();
        let mut owner = UnitSemanticAudit::new(source.clone());
        owner.expected.insert(OracleSemanticFeature::Duration);
        owner.unsupported.push(UnsupportedObservation {
            source,
            fragment: "some unparsed text".into(),
        });
        assert!(
            owner.swallowed().is_empty(),
            "the unit owning the Unimplemented must not double-report its gap"
        );

        let mut sibling = UnitSemanticAudit::new(unit_source());
        sibling.expected.insert(OracleSemanticFeature::Duration);
        assert_eq!(
            sibling.swallowed(),
            vec![OracleSemanticFeature::Duration],
            "a sibling unit must still be audited when another unit is unsupported"
        );
    }

    /// Detector labels are the exported wire format; a rename silently breaks every
    /// consumer of `parse_warnings`. Pin the three that were previously silent, so
    /// their labels cannot drift while they are being brought to life.
    #[test]
    fn detector_labels_are_the_exported_wire_format() {
        assert_eq!(
            OracleSemanticFeature::ActivationLimit.detector_label(),
            "ActivateLimit"
        );
        assert_eq!(
            OracleSemanticFeature::ApnapOrdering.detector_label(),
            "APNAP"
        );
        assert_eq!(
            OracleSemanticFeature::ModalDynamicMaximum.detector_label(),
            "Modal_DynamicMaxDropped"
        );
    }
}
