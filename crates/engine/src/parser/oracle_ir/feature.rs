//! Typed per-item semantic feature vocabulary for the swallow audit.
//!
//! The swallow audit asks one question per source unit: *does the Oracle text of
//! this unit raise a semantic expectation that the parsed output for **this same
//! unit** does not represent?* This module owns the vocabulary that question is
//! asked in, and the item-scoped view of the lowered definitions that supplies
//! the evidence half of the answer.
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
//! item through the parallel `_ids` tracks `lower_oracle_ir` already maintains.
//!
//! Those tracks stay index-aligned with their category vectors across the relation
//! passes: of the four passes that run before the audit, three are
//! length-preserving and the fourth (`apply_linked_choice_etb_counter`) removes
//! from `result.replacements` and `replacement_ids` at the same index. So
//! `result.<category>[k]` ↔ `<category>_ids[k]` is a sound zip at the audit point,
//! which is what [`scope_to_item`] relies on.
//!
//! # Granularity
//!
//! Every unit audited here is an item's **header unit** (`ordinal == 0`), which is
//! document-unique and carries an `Exact` span. Sub-item units (two clauses on one
//! line; mode A vs mode B inside one modal item) are **not** expressible today:
//! `ClauseIrBuilder` mints its clause ids against a fresh, throwaway
//! `OracleDocBuilder`, so every chain restarts at `OracleItemId(0)` and clause
//! `OracleUnitId`s are not document-unique. Restoring sub-item granularity is the
//! recognizer bring-up plan's job.

use super::doc::{OracleItemId, OracleItemIr, OracleNodeIr};
use crate::parser::oracle::ParsedAbilities;

/// A semantic that Oracle text can raise an expectation for, and that the parsed
/// output can be checked to represent.
///
/// Closed and parameter-free on purpose: a stringly feature name would put the
/// audit back on the substring channel this module exists to remove.
///
/// **One variant per emitted detector label.** The tempting collapse — folding the
/// three duration detectors into one `Duration` and the two optionality detectors
/// into one `Optional` — is wrong twice over. `detector` is the **wire format** of
/// `OracleDiagnostic::SwallowedClause` and is exported in `parse_warnings`, so a
/// collapse is a silent breaking change to every downstream consumer of the
/// coverage report; and it would make per-detector regression attribution
/// impossible, because three distinct detectors would report under one name. The
/// semantic *kinship* of the three durations is real, but it is not the label.
///
/// `Effect::Unimplemented` is deliberately **not** a feature. An explicit
/// unsupported node is not a semantic the text asked for — it is the parser
/// admitting it dropped one — so it suppresses its own item's expectations rather
/// than satisfying them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
pub(crate) enum OracleSemanticFeature {
    /// CR 614: an event-modifying effect exists. Net-new in this plan — no
    /// detector in the previous audit emitted it standalone.
    Replacement,
    /// CR 614.1a: effects that use the word "instead" are replacement effects.
    ReplacementInstead,
    /// CR 602.5d + CR 602.5e: activation is confined to a timing window
    /// ("activate only as a sorcery" / "only as an instant" / "only during ...").
    ActivateOnlyDuring,
    /// CR 602.5b: an activated ability carries a restriction on its use — the
    /// rule's own example is "Activate only once each turn".
    ActivateLimit,
    /// CR 611.2a: a continuous effect lasts as long as stated — "until end of
    /// turn".
    DurationUntilEndOfTurn,
    /// CR 611.2a: a stated duration bounded by the current turn.
    DurationThisTurn,
    /// CR 611.2a: a stated duration extending into the next turn.
    DurationNextTurn,
    /// CR 603.5: the effect is optional — it contains "may".
    OptionalYouMay,
    /// CR 603.5: an optional grant phrased as "may have"/"may be".
    OptionalMayHave,
    /// CR 608.2h + CR 107.3: the amount is read from the game ("the number of
    /// creatures on the battlefield") or is the placeholder X, rather than being a
    /// fixed integer.
    DynamicQty,
    /// CR 603.4: a conditional guard. For a triggered ability an "if" immediately
    /// following the trigger event is the intervening-"if" clause; elsewhere the
    /// word has its normal English meaning and still gates the effect.
    ConditionIf,
    /// CR 603.5: an "unless" guard — 603.5 names it alongside "may", because both
    /// are choices resolved as the ability resolves. Distinct from a plain
    /// condition because it inverts and usually carries a payment.
    ConditionUnless,
    /// CR 611.3: an "as long as" gate on a static ability's continuous effect.
    ConditionAsLongAs,
    /// CR 101.4: an explicit turn-order start for a multiplayer iteration (APNAP).
    /// Note that a bare player scope is **not** an ordering fact.
    Apnap,
    /// CR 700.2: a modal choice whose maximum number of modes is dynamic.
    ModalDynamicMaxDropped,
}

#[allow(dead_code)] // production caller lands in the swallow_check cutover commit.
impl OracleSemanticFeature {
    /// The stable detector label this feature is reported under.
    ///
    /// These strings are the wire format of `OracleDiagnostic::SwallowedClause` and
    /// appear in exported `parse_warnings`, so they are byte-for-byte the labels the
    /// previous audit emitted. This function exists to make that mapping a typed,
    /// exhaustive, single-authority fact instead of fourteen string literals spread
    /// across fourteen detectors.
    pub(crate) fn detector_label(self) -> &'static str {
        match self {
            Self::Replacement => "Replacement",
            Self::ReplacementInstead => "Replacement_Instead",
            Self::ActivateOnlyDuring => "ActivateOnlyDuring",
            Self::ActivateLimit => "ActivateLimit",
            Self::DurationUntilEndOfTurn => "Duration_UntilEndOfTurn",
            Self::DurationThisTurn => "Duration_ThisTurn",
            Self::DurationNextTurn => "Duration_NextTurn",
            Self::OptionalYouMay => "Optional_YouMay",
            Self::OptionalMayHave => "Optional_MayHave",
            Self::DynamicQty => "DynamicQty",
            Self::ConditionIf => "Condition_If",
            Self::ConditionUnless => "Condition_Unless",
            Self::ConditionAsLongAs => "Condition_AsLongAs",
            Self::Apnap => "APNAP",
            Self::ModalDynamicMaxDropped => "Modal_DynamicMaxDropped",
        }
    }
}

/// The parallel `OracleItemId` tracks `lower_oracle_ir` maintains, borrowed for the
/// duration of the audit.
///
/// `abilities[k]` is the id of the item whose parse produced `result.abilities[k]`,
/// and likewise for the other three recursive categories. These are the only
/// categories a relation pass can reorder or resynthesize, which is why they need a
/// track at all; every other category is read straight off the owning item's node.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ItemIdTracks<'a> {
    pub(crate) abilities: &'a [OracleItemId],
    pub(crate) triggers: &'a [OracleItemId],
    pub(crate) statics: &'a [OracleItemId],
    pub(crate) replacements: &'a [OracleItemId],
}

/// One auditable unit: a distinct span of Oracle source, and every item that claims
/// that span.
///
/// **The unit is the SPAN, not the item.** A single line routinely lowers to more
/// than one item — Visions of Ruin's `"Flashback {8}{R}{R}. This spell costs {X}
/// less to cast this way, where X is the greatest mana value of a commander..."`
/// emits a `Keyword` item *and* a `ModifyCost` static item — and today **both carry
/// the whole line as their fragment**, because the substrate cannot yet hand an item
/// a sub-line fragment.
///
/// Auditing such items separately is not merely coarse, it is *wrong*: each sibling
/// would raise the expectations of the entire shared line while being able to supply
/// evidence for only its own clause, so the Keyword item would report a swallowed
/// `DynamicQty` for a quantity the static item represents perfectly. That is a
/// manufactured false positive, and it would fire on every multi-item line in the
/// pool.
///
/// Grouping by span is therefore the honest granularity given the substrate: the
/// expectation comes from a piece of text, so the evidence must be everything that
/// piece of text produced. It also degrades in exactly the right direction — when
/// the recognizer bring-up gives items real sub-line spans, items stop sharing a
/// span, the groups split automatically, and the audit gets finer with no change
/// here.
#[derive(Debug)]
pub(crate) struct AuditUnit<'a> {
    /// The source range this unit occupies: `(first_line, start_byte, end_byte)`.
    /// Identity of the unit — two items with the same range are the same unit.
    key: (usize, usize, usize),
    /// The Oracle text this unit is accountable for. Supplies the expectation half.
    pub(crate) fragment: &'a str,
    /// The line this unit's diagnostics are attributed to.
    pub(crate) first_line: usize,
    /// Every item claiming this span. Supplies the evidence half.
    items: Vec<&'a OracleItemIr>,
}

/// Split a document's items into audit units, one per distinct source span.
///
/// Items with no recorded fragment are skipped: there is no text, so there is no
/// expectation to raise. Inventing one would fabricate a warning.
pub(crate) fn audit_units(items: &[OracleItemIr]) -> Vec<AuditUnit<'_>> {
    let mut units: Vec<AuditUnit<'_>> = Vec::new();
    for item in items {
        let Some(fragment) = item.source.fragment() else {
            continue;
        };
        let span = item.source.span();
        let key = (span.first_line, span.start_byte, span.end_byte);
        match units.iter_mut().find(|unit| unit.key == key) {
            Some(unit) => unit.items.push(item),
            None => units.push(AuditUnit {
                key,
                fragment,
                first_line: span.first_line,
                items: vec![item],
            }),
        }
    }
    units
}

/// Clone out the slice of `result` that **this unit alone** produced.
///
/// This is the evidence side of one unit's audit, and it is the whole cutover. The
/// previous audit handed every detector the *card-wide* `ParsedAbilities`, so a
/// card that dropped an activation limit on line 3 was excused by an unrelated
/// restriction on line 1 — the evidence never had to come from the clause that
/// raised the expectation. Handing the same detectors a unit-scoped
/// `ParsedAbilities` makes all ~40 `any_*` / `def_tree_has_*` walkers unit-scoped
/// without touching one of them: the walkers were never the defect, their scope was.
///
/// The four recursive categories are resolved through `tracks` because the relation
/// passes may have synthesized into or removed from them. Everything else is read
/// straight off each item's node: relations never touch those categories, so the
/// node is already the authority.
///
/// Returning an owned `ParsedAbilities` rather than a borrowed view is deliberate —
/// it is what lets the existing detectors be reused verbatim, and it costs one clone
/// of the definitions a single unit produced, at parse time only.
pub(crate) fn scope_to_unit(
    result: &ParsedAbilities,
    tracks: &ItemIdTracks<'_>,
    unit: &AuditUnit<'_>,
) -> ParsedAbilities {
    let owns = |id: OracleItemId| unit.items.iter().any(|item| item.id == id);
    let pick = |ids: &[OracleItemId], len: usize| -> Vec<usize> {
        (0..len)
            .filter(|k| ids.get(*k).is_some_and(|id| owns(*id)))
            .collect()
    };

    let abilities = pick(tracks.abilities, result.abilities.len())
        .into_iter()
        .map(|k| result.abilities[k].clone())
        .collect();
    let triggers = pick(tracks.triggers, result.triggers.len())
        .into_iter()
        .map(|k| result.triggers[k].clone())
        .collect();
    let statics = pick(tracks.statics, result.statics.len())
        .into_iter()
        .map(|k| result.statics[k].clone())
        .collect();
    let replacements = pick(tracks.replacements, result.replacements.len())
        .into_iter()
        .map(|k| result.replacements[k].clone())
        .collect();

    let mut scoped = ParsedAbilities {
        abilities,
        triggers,
        statics,
        replacements,
        extracted_keywords: Vec::new(),
        modal: None,
        additional_cost: None,
        casting_restrictions: Vec::new(),
        casting_options: Vec::new(),
        solve_condition: None,
        strive_cost: None,
        parse_warnings: Vec::new(),
    };

    // Non-recursive categories: no relation pass mutates them, so each item's node IS
    // the authority and no id track is needed. Folded over every item in the unit —
    // this is precisely what stops one clause of a shared line from raising an
    // expectation that its sibling clause already satisfies.
    //
    // Exhaustive on purpose — a new `OracleNodeIr` variant must make a deliberate
    // attribution decision here rather than defaulting into invisibility behind a `_`
    // arm. The four IR variants and the four `PreLowered*` variants contribute
    // through the id tracks above, so they add nothing further here.
    for item in &unit.items {
        match &item.node {
            OracleNodeIr::Keyword(kw) => scoped.extracted_keywords.push(kw.clone()),
            OracleNodeIr::Modal(modal) => scoped.modal = Some(modal.clone()),
            OracleNodeIr::AdditionalCost(cost) => scoped.additional_cost = Some(cost.clone()),
            OracleNodeIr::CastingRestriction(r) => scoped.casting_restrictions.push(r.clone()),
            OracleNodeIr::CastingOption(o) => scoped.casting_options.push(o.clone()),
            OracleNodeIr::SolveCondition(c) => scoped.solve_condition = Some(c.clone()),
            OracleNodeIr::StriveCost(c) => scoped.strive_cost = Some(c.clone()),
            OracleNodeIr::Spell(_)
            | OracleNodeIr::Trigger(_)
            | OracleNodeIr::Static(_)
            | OracleNodeIr::Replacement(_)
            | OracleNodeIr::PreLoweredSpell(_)
            | OracleNodeIr::PreLoweredTrigger(_)
            | OracleNodeIr::PreLoweredStatic(_)
            | OracleNodeIr::PreLoweredReplacement(_) => {}
        }
    }
    scoped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::doc::{OracleDocBuilder, OracleSourceSpan};
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect};

    /// An empty card-wide `ParsedAbilities` to fill in per test. Spelled out rather
    /// than defaulted because `ParsedAbilities` has no `Default` — and that is a
    /// feature: a new category cannot be added without every construction site
    /// making a deliberate decision about it.
    fn empty_parsed() -> ParsedAbilities {
        ParsedAbilities {
            abilities: Vec::new(),
            triggers: Vec::new(),
            statics: Vec::new(),
            replacements: Vec::new(),
            extracted_keywords: Vec::new(),
            modal: None,
            additional_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
            solve_condition: None,
            strive_cost: None,
            parse_warnings: Vec::new(),
        }
    }

    /// One document item at `line`, carrying `node`, sourced exactly at its fragment.
    fn item(
        b: &mut OracleDocBuilder,
        line: usize,
        fragment: &str,
        node: OracleNodeIr,
    ) -> OracleItemIr {
        let span = OracleSourceSpan::exact(line, line, 0, fragment.len(), 0);
        let slot = b.begin_item(span, Some(fragment));
        OracleItemIr {
            id: slot.id(),
            source: slot.source().clone(),
            node,
        }
    }

    fn def(fragment: &str) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Spell, Effect::unimplemented("x", fragment))
    }

    /// Build a two-item document and the id tracks a fold would have produced, with
    /// one ability per item, so scoping can be observed to separate them.
    fn two_item_doc() -> (ParsedAbilities, Vec<OracleItemId>, Vec<OracleItemIr>) {
        let mut b = OracleDocBuilder::new();
        let (def_a, def_b) = (def("line one"), def("line two"));
        let item_a = item(
            &mut b,
            0,
            "line one",
            OracleNodeIr::PreLoweredSpell(def_a.clone()),
        );
        let item_b = item(
            &mut b,
            1,
            "line two",
            OracleNodeIr::PreLoweredSpell(def_b.clone()),
        );

        let mut result = empty_parsed();
        result.abilities = vec![def_a, def_b];
        let ids = vec![item_a.id, item_b.id];
        (result, ids, vec![item_a, item_b])
    }

    /// The entire point of the cutover: each item sees ONLY the definitions it
    /// produced. Under the card-wide scope this replaces, both items would have seen
    /// both abilities — which is precisely how a line-1 fact came to excuse a line-3
    /// expectation.
    #[test]
    fn scoping_separates_two_items_definitions() {
        let (result, ability_ids, items) = two_item_doc();
        let tracks = ItemIdTracks {
            abilities: &ability_ids,
            triggers: &[],
            statics: &[],
            replacements: &[],
        };

        let units = audit_units(&items);
        assert_eq!(units.len(), 2, "two distinct spans => two audit units");

        let scoped_a = scope_to_unit(&result, &tracks, &units[0]);
        assert_eq!(scoped_a.abilities.len(), 1);
        assert_eq!(scoped_a.abilities[0], result.abilities[0]);

        let scoped_b = scope_to_unit(&result, &tracks, &units[1]);
        assert_eq!(scoped_b.abilities.len(), 1);
        assert_eq!(scoped_b.abilities[0], result.abilities[1]);
    }

    /// Detector labels are the exported wire format; a rename silently breaks every
    /// consumer of `parse_warnings`. Pin every label, and in particular pin the three
    /// durations and two optionalities as DISTINCT — collapsing them to one semantic
    /// name is the tempting refactor that would rewrite the wire format and destroy
    /// per-detector regression attribution.
    #[test]
    fn detector_labels_are_the_exported_wire_format() {
        use OracleSemanticFeature as F;
        let all = [
            (F::Replacement, "Replacement"),
            (F::ReplacementInstead, "Replacement_Instead"),
            (F::ActivateOnlyDuring, "ActivateOnlyDuring"),
            (F::ActivateLimit, "ActivateLimit"),
            (F::DurationUntilEndOfTurn, "Duration_UntilEndOfTurn"),
            (F::DurationThisTurn, "Duration_ThisTurn"),
            (F::DurationNextTurn, "Duration_NextTurn"),
            (F::OptionalYouMay, "Optional_YouMay"),
            (F::OptionalMayHave, "Optional_MayHave"),
            (F::DynamicQty, "DynamicQty"),
            (F::ConditionIf, "Condition_If"),
            (F::ConditionUnless, "Condition_Unless"),
            (F::ConditionAsLongAs, "Condition_AsLongAs"),
            (F::Apnap, "APNAP"),
            (F::ModalDynamicMaxDropped, "Modal_DynamicMaxDropped"),
        ];
        for (feature, label) in all {
            assert_eq!(feature.detector_label(), label);
        }
        let distinct: std::collections::BTreeSet<&str> =
            all.iter().map(|(f, _)| f.detector_label()).collect();
        assert_eq!(
            distinct.len(),
            all.len(),
            "every feature must map to a distinct label: a collapse rewrites the wire format"
        );
    }

    /// A non-recursive category is attributed from the item's own node, not from an
    /// id track. Keywords are the case that matters: the activation-limit detector
    /// reads them, and a keyword folded into a cost produces no ability at all.
    #[test]
    fn non_recursive_categories_come_from_the_items_own_node() {
        use crate::types::keywords::Keyword;
        let mut b = OracleDocBuilder::new();
        let kw_item = item(&mut b, 0, "Flying", OracleNodeIr::Keyword(Keyword::Flying));

        // The card also has a spell line, whose ability must NOT leak into the
        // keyword item's scope — that leak is the card-wide scope this replaces.
        let spell_def = def("draw a card");
        let spell_item = item(
            &mut b,
            1,
            "draw a card",
            OracleNodeIr::PreLoweredSpell(spell_def.clone()),
        );
        let mut result = empty_parsed();
        result.abilities = vec![spell_def];
        result.extracted_keywords = vec![Keyword::Flying];
        let ability_ids = vec![spell_item.id];

        let tracks = ItemIdTracks {
            abilities: &ability_ids,
            triggers: &[],
            statics: &[],
            replacements: &[],
        };
        let items = vec![kw_item, spell_item];
        let units = audit_units(&items);
        let scoped = scope_to_unit(&result, &tracks, &units[0]);
        assert_eq!(scoped.extracted_keywords, vec![Keyword::Flying]);
        assert!(
            scoped.abilities.is_empty(),
            "the keyword item must not see the spell line's ability"
        );
    }
}
