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

use super::doc::{OracleItemId, OracleItemIr, OracleNodeIr, OracleSourceSpan};
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, strum::EnumIter)]
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

impl OracleSemanticFeature {
    /// The stable detector label this feature is reported under.
    ///
    /// These strings are the wire format of `OracleDiagnostic::SwallowedClause` and
    /// appear in exported `parse_warnings`, so they are byte-for-byte the labels the
    /// previous audit emitted.
    ///
    /// This IS the single authority: every one of the fourteen `SwallowedClause` emit
    /// sites in `swallow_check.rs` constructs its `detector` through this function, and
    /// none constructs a string literal. That is what makes the label test below a pin on
    /// the WIRE FORMAT rather than merely on this table — a claim worth stating precisely,
    /// because while the fourteen literals still existed the two were independent
    /// authorities and the test could stay green while the export silently changed.
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

/// One auditable unit: a block of Oracle source lines, and every item that starts in
/// it.
///
/// **The unit is a SOURCE BLOCK, not an item.** Two independent substrate facts force
/// this, and getting either wrong corrupts the audit in a different direction.
///
/// **(1) A line lowers to several items — false positives.** Visions of Ruin's
/// `"Flashback {8}{R}{R}. This spell costs {X} less to cast this way, where X is the
/// greatest mana value of a commander..."` emits a `Keyword` item *and* a `ModifyCost`
/// static item, and both carry the whole line as their fragment. Audited separately,
/// each sibling raises the expectations of the entire shared line while supplying
/// evidence for only its own clause, so the `Keyword` item reports a swallowed
/// `DynamicQty` for a quantity the static represents perfectly. Grouping the line's
/// items into one unit — evidence being the union of what they produced — is what
/// makes the expectation and the evidence describe the same text.
///
/// **(2) An item consumes lines its span never claims — false negatives, the
/// dangerous direction.** A modal item reports `first_line == last_line == 0` with the
/// bare header `"Choose one —"` as its fragment, while having actually consumed the
/// bullet lines beneath it. Drown in the Loch is three source lines; all three of its
/// items claim line 0 and carry `"Choose one —"`, so both bullets — which hold the
/// card's entire meaning — are claimed by nothing at all. An audit that trusted
/// fragments would never scan that text, raise no expectation for it, and silently
/// *drop* warnings the card-wide audit correctly raised (measured: 21 faces, all modal
/// blocks or d20 roll tables).
///
/// # What this granularity CANNOT witness — stated, not hidden
///
/// Grouping a line's items into one unit means a sibling's evidence can satisfy an
/// expectation raised by its neighbour's text. Where the two clauses on a line are
/// genuinely unrelated — clause A's semantic dropped, clause B independently carrying a
/// def that happens to answer it — the audit stays silent. **This residual false-green
/// is accepted BY CONSTRUCTION at line granularity, and is bounded by the width of one
/// line.**
///
/// It is not a choice so much as a ceiling: the substrate hands every item on a line the
/// *whole line* as its fragment, so line width is the finest addressing that exists.
/// Auditing below it does not buy precision — it manufactures the false positives case
/// (1) describes, which is strictly worse than the bounded silence it would trade them
/// for. When the recognizer bring-up gives items real sub-line spans, units subdivide on
/// their own and this ceiling lifts with no change here.
///
/// So a unit owns every source line from its own start up to the next unit's start.
/// Coverage is then **total by construction**: every line belongs to exactly one unit,
/// and no text can go unaudited. A warning that disappears because nothing claimed its
/// text is the one delta direction that hides a regression, and this is the invariant
/// that forecloses it.
///
/// It also degrades in the right direction: once the recognizer bring-up gives items
/// honest spans, units subdivide on their own and the audit gets finer with no change
/// here.
#[derive(Debug)]
pub(crate) struct AuditUnit<'a> {
    /// The Oracle source this unit is accountable for. Supplies the expectation half.
    ///
    /// Sliced from the document's raw `source_text` rather than read off
    /// `item.fragment()` — fragments under-report what an item consumed (see above)
    /// and are normalized (the card's own name is rewritten to `~`), while the
    /// detectors' marker phrases and the emitted `description` are raw-text concepts.
    pub(crate) text: String,
    /// The line this unit's diagnostics are attributed to.
    pub(crate) first_line: usize,
    /// The card-absolute byte range of the lines this unit OWNS — i.e. of `text`, the
    /// exact source the detectors were handed.
    ///
    /// `Exact` locates the **unit**, not a clause inside it: item spans are whole-line
    /// (`DocEmitter::exact_span`), so every clause on one line lands in this one unit and
    /// shares this one span. It is not `first_line`-derived by accident either — the
    /// first unit absorbs any leading lines no item claimed, so `span.first_line` can be
    /// SMALLER than `first_line`, and the span is the honest extent of the audited text
    /// while `first_line` remains the item-start line the diagnostics are attributed to.
    pub(crate) span: OracleSourceSpan,
    /// Every item starting in this block. Supplies the evidence half.
    items: Vec<&'a OracleItemIr>,
}

impl AuditUnit<'_> {
    /// The unit's pooled evidence set, as ids — the provenance a diagnostic carries.
    /// Legitimately EMPTY for a card that lowered to no items at all.
    pub(crate) fn item_ids(&self) -> Vec<OracleItemId> {
        self.items.iter().map(|item| item.id).collect()
    }
}

/// Partition a document into audit units: one per source line that starts an item,
/// each owning the lines up to the next such line.
///
/// Every line of `source_text` lands in exactly one unit — including the continuation
/// lines (modal bullets, roll-table branches) that no item's span admits to consuming.
/// Leading lines before the first item join the first unit for the same reason: text
/// that belongs to no unit raises no expectation, and a warning that vanishes because
/// nobody claimed its text is indistinguishable from a warning that was fixed.
pub(crate) fn audit_units<'a>(
    items: &'a [OracleItemIr],
    source_text: &'a str,
) -> Vec<AuditUnit<'a>> {
    let lines: Vec<&str> = source_text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }

    // Byte offset of the start of each line, mirroring `DocEmitter::line_start` (one
    // '\n' per prior line). A unit's span must live in the SAME coordinate system as the
    // item spans it owns, or a consumer cannot relate the two.
    let mut line_start: Vec<usize> = Vec::with_capacity(lines.len());
    let mut acc = 0usize;
    for line in &lines {
        line_start.push(acc);
        acc += line.len() + 1;
    }
    // The exactly-located card-absolute span of the line block `[start, end)`.
    let span_of = |start: usize, end: usize| -> OracleSourceSpan {
        let last = end.max(start + 1) - 1;
        OracleSourceSpan::exact(
            start,
            last,
            line_start[start],
            line_start[last] + lines[last].len(),
            0,
        )
    };

    // One unit per distinct starting line, in source order. `ir.items` is already in
    // Oracle source order (the document is keyed by source position), so a single pass
    // suffices and the resulting starts are ascending. Collected as (line, items) pairs
    // rather than as `AuditUnit`s because a unit's `text` and `span` are both functions
    // of the NEXT unit's start, which is not known until the pass is done — and a
    // half-built `AuditUnit` carrying a placeholder span is exactly the fabrication the
    // span type exists to prevent.
    let mut starts: Vec<(usize, Vec<&'a OracleItemIr>)> = Vec::new();
    for item in items {
        let first_line = item.source.span().first_line;
        if first_line >= lines.len() {
            continue;
        }
        match starts.last_mut() {
            Some((line, unit_items)) if *line == first_line => unit_items.push(item),
            _ => starts.push((first_line, vec![item])),
        }
    }

    // A card that produced NO items at all is still accountable for its text. This is
    // not a curiosity — it is the worst case there is. Chorus of the Conclave lowers to
    // an entirely empty `ParsedAbilities` (no ability, no static, no additional cost, no
    // keyword) and does not even leave an `Effect::Unimplemented` behind: the parser
    // drops the whole card in silence. With no items there are no units, and a
    // unit-iterating audit would then say nothing at all — going quiet at exactly the
    // moment the parser failed hardest. That is the precise shape of a false green.
    //
    // So such a card becomes ONE unit owning all of its text, with no items and
    // therefore NO evidence. Every semantic its text raises is then correctly reported
    // as swallowed, because nothing represents any of it.
    if starts.is_empty() {
        return vec![AuditUnit {
            text: source_text.to_string(),
            first_line: 0,
            span: span_of(0, lines.len()),
            items: Vec::new(),
        }];
    }

    // Give each unit the lines it owns: its own start through the line before the next
    // unit's start. The first unit absorbs any leading lines, and the last unit runs to
    // the end of the card, so the partition covers every line.
    let mut units: Vec<AuditUnit<'a>> = Vec::with_capacity(starts.len());
    for i in 0..starts.len() {
        let first_line = starts[i].0;
        let start = if i == 0 { 0 } else { first_line };
        let end = starts
            .get(i + 1)
            .map_or(lines.len(), |(next, _)| (*next).min(lines.len()))
            .max(start + 1);
        units.push(AuditUnit {
            text: lines[start..end].join("\n"),
            first_line,
            span: span_of(start, end),
            items: std::mem::take(&mut starts[i].1),
        });
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
///
/// # The premise the "unit evidence ⊆ card evidence" argument rests on
///
/// That argument is what makes an evidence-side LOSS structurally impossible (fewer facts
/// can only yield more warnings), and it is how the delta's loss ledger is reasoned about.
/// It holds unconditionally for the four id-tracked categories and for the `Vec` ones.
///
/// It does NOT hold unconditionally for the four **`Option`-valued singletons** — `modal`,
/// `additional_cost`, `solve_condition`, `strive_cost`. `lower_oracle_ir` assigns those
/// last-write-wins across items, so on a card carrying two such items the card-wide view
/// shows only the LAST one, while the unit-scoped view shows each unit its OWN. A unit
/// could then hold a singleton the card-wide view had overwritten — evidence the card-wide
/// audit never had — and that is the one shape that can produce an evidence-side loss.
///
/// No card in the pool does this today (each singleton appears at most once per card), so
/// the impact is zero and the subset argument stands as used. The debug assertion below
/// makes the premise fail loudly rather than silently if a future card breaks it.
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
            // The four `Option`-valued singletons. `lower_oracle_ir` assigns these
            // last-write-wins card-wide, so a second one on a card would give this unit
            // evidence the card-wide view had overwritten — the one shape that breaks the
            // "unit evidence ⊆ card evidence" premise the loss ledger is reasoned on. No
            // card in the pool does this; these assertions make a future one fail loudly
            // rather than quietly turning a warning off.
            OracleNodeIr::Modal(modal) => {
                debug_assert!(scoped.modal.is_none(), "two Modal items in one unit");
                scoped.modal = Some(modal.clone());
            }
            OracleNodeIr::AdditionalCost(cost) => {
                debug_assert!(
                    scoped.additional_cost.is_none(),
                    "two AdditionalCost items in one unit"
                );
                scoped.additional_cost = Some(cost.clone());
            }
            OracleNodeIr::SolveCondition(c) => {
                debug_assert!(
                    scoped.solve_condition.is_none(),
                    "two SolveCondition items in one unit"
                );
                scoped.solve_condition = Some(c.clone());
            }
            OracleNodeIr::StriveCost(c) => {
                debug_assert!(
                    scoped.strive_cost.is_none(),
                    "two StriveCost items in one unit"
                );
                scoped.strive_cost = Some(c.clone());
            }
            OracleNodeIr::CastingRestriction(r) => scoped.casting_restrictions.push(r.clone()),
            OracleNodeIr::CastingOption(o) => scoped.casting_options.push(o.clone()),
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

        let units = audit_units(&items, "line one\nline two");
        assert_eq!(
            units.len(),
            2,
            "two distinct starting lines => two audit units"
        );

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
    fn detector_labels_are_distinct_and_pin_the_exported_wire_format() {
        use strum::IntoEnumIterator;
        use OracleSemanticFeature as F;

        // The expected table is deliberately hand-written — it is the pin, and a generated
        // one would only ever agree with whatever the code says.
        let expected = [
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
        for (feature, label) in expected {
            assert_eq!(feature.detector_label(), label);
        }

        // Exhaustiveness comes from `EnumIter`, not from the length of a hand-written array.
        // `detector_label`'s match already forces a 16th variant to declare a LABEL — but a
        // 16th variant declaring a DUPLICATE label would sail past a 15-entry array that
        // never mentions it, and a collapsed label is the exact failure this test exists to
        // catch. Iterating the enum closes that: a new variant enters both checks below on
        // its own, whether or not anyone remembers this file.
        let every: Vec<F> = F::iter().collect();
        assert_eq!(
            every.len(),
            expected.len(),
            "a variant was added without pinning its exported label above"
        );
        let distinct: std::collections::BTreeSet<&str> =
            every.iter().map(|f| f.detector_label()).collect();
        assert_eq!(
            distinct.len(),
            every.len(),
            "every feature must map to a DISTINCT label: a collapse silently rewrites the \
             wire format and destroys per-detector regression attribution"
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
        let units = audit_units(&items, "Flying\ndraw a card");
        let scoped = scope_to_unit(&result, &tracks, &units[0]);
        assert_eq!(scoped.extracted_keywords, vec![Keyword::Flying]);
        assert!(
            scoped.abilities.is_empty(),
            "the keyword item must not see the spell line's ability"
        );
    }
}

/// Item-structure census over a real card pool — the instrument that sizes the substrate
/// defects this module works around, and that Plan 05 inherits.
///
/// Kept (rather than deleted with the investigation that spawned it) because it is the only
/// thing in the tree that can measure them. Two of `audit_units`' guards were proven DEAD on
/// the real pool by this census — `first_line` out of range: 0, `fragment == None`: 0 — and
/// the whole-line-fragment defect it sizes is the reason units are span-blocks at all.
///
/// `#[ignore]`d: it parses the entire pool (~30 min debug) and needs a card pool, which a
/// checkout does not have. Point it at one and run it explicitly:
///
/// ```text
/// ORACLE_POOL_DIR=/path/to/data cargo test -p engine --lib pool_structure_census \
///     -- --ignored --nocapture
/// ```
#[cfg(test)]
mod pool_structure_census {
    use crate::parser::oracle::parse_oracle_ir;
    use std::collections::BTreeMap;

    #[test]
    #[ignore = "parses the whole card pool; needs ORACLE_POOL_DIR"]
    fn census() {
        let Ok(dir) = std::env::var("ORACLE_POOL_DIR") else {
            panic!("set ORACLE_POOL_DIR to a directory containing mtgjson/AtomicCards.json");
        };
        let raw = std::fs::read_to_string(format!("{dir}/mtgjson/AtomicCards.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let data = v.get("data").unwrap().as_object().unwrap();

        let (mut faces, mut items_total) = (0usize, 0usize);
        let (mut frag_none, mut out_of_range, mut zero_item_faces) = (0usize, 0usize, 0usize);
        let (mut multi_item_lines, mut faces_with_multi_item_line) = (0usize, 0usize);

        for printings in data.values() {
            for card in printings.as_array().unwrap() {
                let text = card.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.is_empty() {
                    continue;
                }
                let name = card.get("name").and_then(|t| t.as_str()).unwrap_or("");
                let strs = |k: &str| -> Vec<String> {
                    card.get(k)
                        .and_then(|x| x.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let ir = parse_oracle_ir(
                    text,
                    name,
                    &strs("keywords"),
                    &strs("types"),
                    &strs("subtypes"),
                );
                faces += 1;
                items_total += ir.items.len();
                if ir.items.is_empty() {
                    zero_item_faces += 1;
                }
                let nlines = ir.source_text.lines().count();
                let mut per_line: BTreeMap<usize, usize> = BTreeMap::new();
                for item in &ir.items {
                    if item.source.fragment().is_none() {
                        frag_none += 1;
                    }
                    if item.source.span().first_line >= nlines {
                        out_of_range += 1;
                    }
                    *per_line.entry(item.source.span().first_line).or_default() += 1;
                }
                let multi = per_line.values().filter(|c| **c > 1).count();
                multi_item_lines += multi;
                if multi > 0 {
                    faces_with_multi_item_line += 1;
                }
            }
        }
        println!("\n===== POOL ITEM-STRUCTURE CENSUS =====");
        println!("faces with text                     : {faces}");
        println!("items total                         : {items_total}");
        println!("faces with ZERO items               : {zero_item_faces}");
        println!("items with fragment == None         : {frag_none}");
        println!("items with first_line out of range  : {out_of_range}");
        println!("MULTI-ITEM LINES (>1 item on a line): {multi_item_lines}");
        println!("faces having >=1 multi-item line    : {faces_with_multi_item_line}");
    }
}
