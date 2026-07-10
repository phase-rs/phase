//! Document-level Oracle IR types.
//!
//! `OracleDocIr` represents the complete parsed output of a card's Oracle text
//! as a list of `OracleItemIr`. Each item carries a stable `OracleItemId`, an
//! `OracleUnitSource` (byte + line span), and a typed `OracleNodeIr` payload.
//!
//! Items are produced through `OracleDocBuilder`, which is the single authority
//! for item identity, ordering, and span invariants. Nothing constructs an
//! `OracleItemIr` directly.
//!
//! **Unit-3a status.** The builder orders items by `(first_line, ordinal)` and is
//! ready to receive exact spans, but its only current producer is
//! `parsed_abilities_to_doc_ir`, which runs after lowering and can supply only a
//! whole-document containing span (see `OracleSourceSpan::whole_document`). So
//! today's item order still reproduces the category order of `ParsedAbilities`.
//! Unit 3b moves emission into the dispatch loop, at which point items become
//! genuinely source-ordered with exact spans and this note is deleted.

// NOTE ON `dead_code`: suppression in this module is **per item**, never
// module-wide. A module-wide `#![allow(dead_code)]` also silences code not yet
// written, and it is precisely what hid two silent defects here during review
// (an `emit` counter that skipped every `PreLowered*` payload, and a
// `validate_child_span` that failed open). Each suppression below names the unit
// that gives the item a production caller, and dies when that unit lands.

use std::collections::BTreeMap;

use super::diagnostic::OracleDiagnostic;
use super::effect_chain::EffectChainIr;
use super::replacement::ReplacementIr;
use super::static_ir::StaticIr;
use super::trigger::TriggerIr;
use crate::types::ability::{
    AbilityDefinition, AdditionalCost, CastingRestriction, ModalChoice, ReplacementDefinition,
    SolveCondition, SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;

// ---------------------------------------------------------------------------
// Source identity
// ---------------------------------------------------------------------------

/// Stable document-local identity for one parsed item.
///
/// Reserved by `OracleDocBuilder::begin_item` *before* branch parsing, so a
/// nested parser can name its owning item without the item existing yet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub(crate) struct OracleItemId(pub(crate) u32);

/// Document-global identity for one independently auditable unit: an item, a
/// clause, a modal mode, a trigger/static/replacement execute body, a granted
/// ability body, or an explicit unsupported fallback.
///
/// `ordinal` is scoped to `item`; `ordinal == 0` is the item's own header unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub(crate) struct OracleUnitId {
    pub(crate) item: OracleItemId,
    pub(crate) ordinal: u32,
}

/// How precisely a span locates its unit.
///
/// A span is always *true* — it always contains its unit — but it is not always
/// *minimal*. A consumer that renders a position (Plan 02's per-unit
/// diagnostics) must be able to tell the difference, because `first_line == 0`
/// on a whole-document span is not the claim "this unit is on line 0"; it is the
/// claim "we do not yet know which line". Rendering the former as the latter is
/// a precise-looking wrong answer, which is worse than an admittedly coarse one.
///
/// Typed rather than a `bool` per CLAUDE.md: a future `LineOnly` precision (line
/// known, byte range not) is an enum value, not a second flag.
// No `Ord`: `Exact < WholeDocument` would be a meaningless magnitude claim on a
// qualifier this enum's own docs call orthogonal to containment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
pub(crate) enum SpanPrecision {
    /// The span is the unit's exact byte/line extent. Safe to render.
    Exact,
    /// The span is the whole Oracle document: a true containing range whose
    /// bounds carry no per-unit information. A renderer must NOT print
    /// `first_line`/`start_byte` as this unit's position.
    ///
    /// UNIT-3B DEBT — emitted only by `OracleSourceSpan::whole_document`.
    WholeDocument,
}

/// Position of a unit within the original Oracle text, plus how precisely that
/// position is known (`precision`).
///
/// Byte ranges are required (not just line ranges) so two clauses on one
/// physical line remain distinguishable. `ordinal_within_span` disambiguates
/// units that share a byte span — one Oracle line may legitimately yield two
/// items (e.g. `Kicker {2}{G}` emits a `Keyword` and an `AdditionalCost`).
// No `PartialOrd`/`Ord`: derived lexicographic order would compare `precision`
// ahead of `ordinal_within_span`, which is nonsense, and there is no consumer —
// the item map is keyed by an explicit `(usize, u32)` tuple, not by this type.
// If a future unit needs ordering, hand-implement it over
// `(start_byte, end_byte, ordinal_within_span)` and never over `precision`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
pub(crate) struct OracleSourceSpan {
    pub(crate) first_line: usize,
    pub(crate) last_line: usize,
    pub(crate) start_byte: usize,
    pub(crate) end_byte: usize,
    /// Whether the bounds above locate this unit exactly. See `SpanPrecision`.
    pub(crate) precision: SpanPrecision,
    pub(crate) ordinal_within_span: u32,
}

impl OracleSourceSpan {
    /// The whole Oracle document, distinguished only by `ordinal_within_span`.
    ///
    /// UNIT-3B DEBT, and deliberately coarse rather than fabricated. Items are
    /// currently emitted by `parsed_abilities_to_doc_ir`, which runs *after*
    /// lowering and never sees the line cursor, so no exact position is
    /// recoverable there. Recovering one by searching `source_text` for a
    /// lowered definition's `description` would be precisely the lowered-shape
    /// scan this plan exists to delete.
    ///
    /// This span is *true* — the document does contain the item — merely not
    /// minimal. Unit 3b moves emission into the dispatch loop, where the exact
    /// line/byte range is in hand, and this constructor disappears.
    ///
    /// Invariants still hold: `is_contained_by` is satisfied by construction,
    /// and `conflicts_with` cannot fire because ordinals are distinct.
    ///
    /// Carries `SpanPrecision::WholeDocument` so a consumer cannot mistake
    /// `first_line == 0` for "this unit is on line 0".
    pub(crate) fn whole_document(source_text: &str, ordinal_within_span: u32) -> Self {
        Self {
            first_line: 0,
            last_line: source_text.lines().count().saturating_sub(1),
            start_byte: 0,
            end_byte: source_text.len(),
            precision: SpanPrecision::WholeDocument,
            ordinal_within_span,
        }
    }

    /// An exactly-located span. The constructor unit 3b uses once emission moves
    /// into the dispatch loop and the real line/byte range is in hand.
    #[allow(dead_code)] // production caller lands in unit 3b.
    pub(crate) fn exact(
        first_line: usize,
        last_line: usize,
        start_byte: usize,
        end_byte: usize,
        ordinal_within_span: u32,
    ) -> Self {
        Self {
            first_line,
            last_line,
            start_byte,
            end_byte,
            precision: SpanPrecision::Exact,
            ordinal_within_span,
        }
    }

    /// True when this span's bounds locate the unit exactly. A position renderer
    /// must consult this before printing a line number or byte offset.
    ///
    /// Live in production today: `check_fragment_precision` <- `emit`.
    pub(crate) fn is_exact(&self) -> bool {
        matches!(self.precision, SpanPrecision::Exact)
    }

    /// True when `self` and `other` cover overlapping bytes *and* claim the same
    /// `ordinal_within_span`. Co-located siblings with distinct ordinals are
    /// legal; that is what `ordinal_within_span` exists for.
    pub(crate) fn conflicts_with(&self, other: &Self) -> bool {
        self.ordinal_within_span == other.ordinal_within_span
            && self.start_byte < other.end_byte
            && other.start_byte < self.end_byte
    }

    /// True when `self` lies entirely within `other`'s byte range.
    #[allow(dead_code)] // production caller lands in unit 3b.
    pub(crate) fn is_contained_by(&self, other: &Self) -> bool {
        self.start_byte >= other.start_byte && self.end_byte <= other.end_byte
    }
}

/// A unit's identity, its source position, and — when that position is exact —
/// the verbatim fragment it covers.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OracleUnitSource {
    id: OracleUnitId,
    span: OracleSourceSpan,
    /// The verbatim Oracle text this unit covers — **only** when `span.precision`
    /// is `Exact`.
    ///
    /// `None` under `WholeDocument`, because the only "fragment" such a span could
    /// honestly report is the entire card. Handing a diagnostic renderer the whole
    /// card as "the offending clause" is a precise-looking wrong answer, exactly
    /// what `SpanPrecision` exists to prevent — guarding the span while leaving the
    /// fragment lying would move the lie, not remove it.
    ///
    /// `emit` rejects any unit whose fragment presence disagrees with its precision.
    #[serde(skip_serializing_if = "Option::is_none")]
    fragment: Option<String>,
}

impl OracleUnitSource {
    #[allow(dead_code)] // consumed by Plan 02's per-unit diagnostics.
    pub(crate) fn id(&self) -> OracleUnitId {
        self.id
    }

    #[allow(dead_code)] // consumed by Plan 02's per-unit diagnostics.
    pub(crate) fn span(&self) -> &OracleSourceSpan {
        &self.span
    }

    /// `Some` iff the span is `Exact`. See the field docs.
    #[allow(dead_code)] // consumed by Plan 02's per-unit diagnostics.
    pub(crate) fn fragment(&self) -> Option<&str> {
        self.fragment.as_deref()
    }

    /// Fields are private so a nested parser holding a `UnitAllocator` cannot
    /// forge a source with an arbitrary span. The only constructors are
    /// `OracleDocBuilder::begin_item` and `UnitAllocator::allocate_with_span`,
    /// both of which validate.
    fn new(id: OracleUnitId, span: OracleSourceSpan, fragment: Option<&str>) -> Self {
        Self {
            id,
            span,
            fragment: fragment.map(str::to_owned),
        }
    }

    /// A unit whose fragment presence contradicts its span precision is a parser
    /// contract violation, not an Oracle-text problem. Fail closed.
    fn check_fragment_precision(&self) -> Result<(), DocBuilderError> {
        if self.fragment.is_some() == self.span.is_exact() {
            Ok(())
        } else {
            Err(DocBuilderError::FragmentPrecisionMismatch {
                unit: self.id,
                precision: self.span.precision,
            })
        }
    }
}

/// One source-addressed parsed item.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OracleItemIr {
    pub(crate) id: OracleItemId,
    pub(crate) source: OracleUnitSource,
    pub(crate) node: OracleNodeIr,
}

/// The typed payload of a document item. Identity and provenance live on
/// `OracleItemIr`; this enum carries only the parsed category.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[allow(clippy::large_enum_variant)] // Intentional: variants carry parser IR directly.
pub(crate) enum OracleNodeIr {
    /// Spell or activated ability effect chain.
    ///
    /// Unit 3b replaces this payload with `AbilityIr { source, body, shell }` so
    /// the activation metadata the router currently applies around the chain
    /// becomes typed IR rather than a pre-lowered `AbilityDefinition`.
    #[allow(dead_code)] // constructed by ordinary dispatch in unit 3b.
    Spell(EffectChainIr),
    /// Triggered ability.
    #[allow(dead_code)] // constructed by ordinary dispatch in unit 3b.
    Trigger(TriggerIr),
    /// Static ability.
    #[allow(dead_code)] // constructed by ordinary dispatch in unit 3b.
    Static(StaticIr),
    /// Replacement effect.
    #[allow(dead_code)] // constructed by ordinary dispatch in unit 3b.
    Replacement(ReplacementIr),
    /// Keyword ability from a keyword line.
    Keyword(Keyword),
    /// Modal spell block (Choose one/two/etc.).
    Modal(ModalChoice),
    /// Additional casting cost.
    AdditionalCost(AdditionalCost),
    /// Casting restriction.
    CastingRestriction(CastingRestriction),
    /// Casting option (alternative/additional modes).
    CastingOption(SpellCastingOption),
    /// Case enchantment solve condition.
    SolveCondition(SolveCondition),
    /// Strive per-target surcharge.
    StriveCost(ManaCost),

    // -----------------------------------------------------------------------
    // UNIT-4 DEBT — pre-lowered escape hatches.
    //
    // These four variants carry already-assembled engine definitions rather
    // than IR. They exist for exactly one reason: the five preprocessors
    // (`parse_saga_chapters`, `parse_attraction_visit_triggers`,
    // `parse_level_blocks`, `parse_spacecraft_threshold_lines`,
    // `parse_class_oracle_text`) return lowered engine types and have nowhere
    // else to go. Converting them is unit 4, whose file set is disjoint from
    // unit 3's.
    //
    // Until unit 4 lands, ordinary dispatch ALSO routes through these (unit 3b
    // converts ordinary dispatch to `Spell`/`Trigger`/`Static`/`Replacement`).
    //
    // Removal gate 4 ("grep finds no `PreLowered*`") is satisfied only after
    // unit 4. Do not add a new producer of these variants.
    // -----------------------------------------------------------------------
    /// Pre-lowered trigger from a preprocessor. UNIT-4 DEBT.
    PreLoweredTrigger(TriggerDefinition),
    /// Pre-lowered static from a preprocessor. UNIT-4 DEBT.
    PreLoweredStatic(StaticDefinition),
    /// Pre-lowered replacement from a preprocessor. UNIT-4 DEBT.
    PreLoweredReplacement(ReplacementDefinition),
    /// Pre-lowered spell/activated ability from a preprocessor or dispatch path
    /// that constructs an `AbilityDefinition` directly. UNIT-4 DEBT.
    PreLoweredSpell(AbilityDefinition),
}

// ---------------------------------------------------------------------------
// CR 707.9a printed-slot indices
// ---------------------------------------------------------------------------

/// The printed slot the next emitted ability will occupy.
///
/// CR 707.9a: "Some copy effects cause the copy to gain an ability as part of the
/// copying process. This ability becomes part of the copiable values for the
/// copy, along with any other abilities that were copied." An "…except it has
/// this ability" clause must resolve to the correct *printed* slot, so the index
/// threaded into the parser has to be the emission count, not a vector length.
///
/// A newtype rather than `usize` so a category-vector length cannot be passed by
/// accident. The only unrestricted producer is `OracleDocBuilder::ability_index`;
/// the one escape hatch is loudly named and deleted in unit 3b.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PrintedAbilityIndex(usize);

/// The printed slot the next emitted trigger will occupy. See
/// `PrintedAbilityIndex`; same rule, same reasoning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct PrintedTriggerIndex(usize);

macro_rules! printed_index_impl {
    ($t:ty) => {
        impl $t {
            /// The raw slot, for the parser internals that still store `usize`.
            pub(crate) fn get(self) -> usize {
                self.0
            }

            /// The slot `n` positions after this one. Compound splitting emits
            /// several definitions from one line and needs each one's own slot.
            #[allow(dead_code)] // ability side gains a caller in unit 3b.
            pub(crate) fn offset(self, n: usize) -> Self {
                Self(self.0 + n)
            }

            /// **UNIT-3B DEBT — the sole legal way to build a printed index from
            /// a category-vector length.**
            ///
            /// In unit 3a the builder is constructed inside
            /// `parsed_abilities_to_doc_ir`, i.e. *after* the dispatch loop has
            /// run, so the loop's index reads have no builder in scope and must
            /// still derive the slot from `Vec::len()`. That is correct today and
            /// only today: emission is category-ordered, so `len()` *is* the
            /// printed slot. Unit 3b emits in source order and the equality dies.
            ///
            /// Rather than trust a comment, the newtype forces every such read to
            /// name this function, so `rg from_category_vector_len` enumerates every
            /// site that derives an **absolute** printed slot from a category vector.
            /// Unit 3b deletes this constructor; afterwards
            /// `Some(result.triggers.len())` does not compile and
            /// `OracleDocBuilder` is the only producer.
            ///
            /// **That grep is complete for absolute derivations and for nothing else.**
            /// `offset(n)` displaces an already-correct base and is NOT part of this
            /// cutover, but its argument can carry debt of its own:
            ///
            /// * `oracle_trigger.rs` (`base.offset(i)`, two sites) — `i` indexes the
            ///   compound halves of ONE trigger line. Structural; correct under any
            ///   emission order; not debt.
            /// * `oracle_spacecraft.rs` (`base.offset(output.triggers.len())`) — a
            ///   `len()` read of the preprocessor's OWN local vector, so it is
            ///   relative and survives document reordering, but unit 4 replaces it
            ///   with an emission counter when that preprocessor emits through the
            ///   builder.
            ///
            /// So: `rg from_category_vector_len` for the 3b cutover; `rg '\.offset\('`
            /// to review the relative sites, one of which is unit-4 debt. Claiming a
            /// single grep is exhaustive would be a false safety promise, which is the
            /// defect class this whole type exists to remove.
            ///
            /// A `debug_assert_eq!(counter, vec.len())` was considered and rejected:
            /// it holds only while emission is category-ordered, so it would fire on
            /// *correct* code the moment 3b reorders, and it is compiled out of the
            /// release profile that generates `card-data.json`.
            pub(crate) fn from_category_vector_len(len: usize) -> Self {
                Self(len)
            }
        }
    };
}

printed_index_impl!(PrintedAbilityIndex);
printed_index_impl!(PrintedTriggerIndex);

/// Test-only constructors. Kept behind `cfg(test)` so production code cannot
/// mint a printed index out of a bare integer: the only production producers are
/// `OracleDocBuilder::{ability_index, trigger_index}` and the loudly-named
/// `from_category_vector_len` escape hatch that unit 3b deletes.
#[cfg(test)]
impl PrintedTriggerIndex {
    pub(crate) fn from_slot_for_test(slot: usize) -> Self {
        Self(slot)
    }
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

/// Document-level IR: the complete parsed representation of a card's Oracle text.
///
/// Produced by `parse_oracle_ir`, consumed by `lower_oracle_ir`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct OracleDocIr {
    /// Parsed items in Oracle source order.
    pub(crate) items: Vec<OracleItemIr>,
    /// Original Oracle text (provenance).
    pub(crate) source_text: String,
    /// Card name for self-reference context.
    pub(crate) card_name: String,
    /// Typed diagnostics accumulated during parsing (D-07).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) diagnostics: Vec<OracleDiagnostic>,
}

impl OracleDocIr {
    /// Look up an item by its stable id. Cross-item lowering binds through this,
    /// never by scanning category vectors for a matching shape.
    #[allow(dead_code)] // production caller lands in unit 3b.
    pub(crate) fn item(&self, id: OracleItemId) -> Option<&OracleItemIr> {
        self.items.iter().find(|item| item.id == id)
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

/// Reason an item or unit was rejected by the builder. These are contract
/// violations in the parser, not Oracle-text problems.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // production caller lands in unit 3b.
pub(crate) enum DocBuilderError {
    DuplicateItemPosition {
        span: OracleSourceSpan,
    },
    OverlappingSiblingSpans {
        first: OracleSourceSpan,
        second: OracleSourceSpan,
    },
    ChildSpanOutsideItem {
        item: OracleItemId,
        child: OracleSourceSpan,
    },
    /// A unit carries a fragment without an `Exact` span, or vice versa.
    FragmentPrecisionMismatch {
        unit: OracleUnitId,
        precision: SpanPrecision,
    },
}

/// Item-scoped allocator for child `OracleUnitId`s.
///
/// Handed to nested clause/mode/body parsers through `ParseContext`. A nested
/// parser may allocate child units but can never invent an `OracleItemId`.
#[derive(Debug, Clone)]
#[allow(dead_code)] // production caller lands in unit 3b.
pub(crate) struct UnitAllocator {
    item: OracleItemId,
    /// The owning item's span. Held here, not on `ItemSlot`, because the
    /// allocator is what nested parsers actually receive (through `ParseContext`)
    /// — see `allocate_with_span`.
    parent_span: OracleSourceSpan,
    next_ordinal: u32,
    issued: Vec<u32>,
}

#[allow(dead_code)] // production caller lands in unit 3b.
impl UnitAllocator {
    fn new(item: OracleItemId, parent_span: OracleSourceSpan) -> Self {
        // Ordinal 0 is reserved for the item's own header unit.
        Self {
            item,
            parent_span,
            next_ordinal: 1,
            issued: vec![0],
        }
    }

    /// Allocate a child unit **with** its source, validating containment.
    ///
    /// This is the only way to obtain an `OracleUnitSource` for a nested clause,
    /// mode, or execute body, and it is why `OracleUnitSource`'s fields are
    /// private. A validator that merely *offered* to check a child span would be
    /// advisory: nested parsers hold this allocator, detached from the `ItemSlot`,
    /// so a `validate_child_span` living on the slot is not reachable from the
    /// code that needs it. Putting the parent span here makes the check
    /// unavoidable rather than available.
    ///
    /// Fails closed on an out-of-range child and on a fragment/precision mismatch.
    pub(crate) fn allocate_with_span(
        &mut self,
        span: OracleSourceSpan,
        fragment: Option<&str>,
    ) -> Result<OracleUnitSource, DocBuilderError> {
        if !span.is_contained_by(&self.parent_span) {
            return Err(DocBuilderError::ChildSpanOutsideItem {
                item: self.item,
                child: span,
            });
        }
        let source = OracleUnitSource::new(self.allocate(), span, fragment);
        source.check_fragment_precision()?;
        Ok(source)
    }

    pub(crate) fn item(&self) -> OracleItemId {
        self.item
    }

    /// Allocate the next child unit id. **Private on purpose**: an id handed to a
    /// nested parser without a validated span is exactly the forge path
    /// `allocate_with_span` exists to close. Making `OracleUnitSource`'s fields
    /// private is the load-bearing half; leaving a span-less allocator beside the
    /// new constructor would be theatre.
    fn allocate(&mut self) -> OracleUnitId {
        let ordinal = self.next_ordinal;
        self.next_ordinal += 1;
        self.issued.push(ordinal);
        OracleUnitId {
            item: self.item,
            ordinal,
        }
    }
}

/// Source-positioned document collector.
///
/// Keyed by `(first_line, ordinal_within_span)` so emission order is irrelevant
/// and the final `Vec` is always in Oracle source order. This is what makes the
/// document IR source-ordered rather than category-ordered.
#[derive(Debug, Default)]
pub(crate) struct OracleDocBuilder {
    items: BTreeMap<(usize, u32), OracleItemIr>,
    next_item_id: u32,
    /// CR 707.9a printed-**trigger** index: the slot the next emitted trigger
    /// occupies.
    ///
    /// CR 707.9a: "Some copy effects cause the copy to gain an ability as part of
    /// the copying process. This ability becomes part of the copiable values for
    /// the copy, along with any other abilities that were copied."
    ///
    /// **The rule is not "never derive an index from a vector length."** It is
    /// *never derive an ABSOLUTE printed slot from a DOCUMENT category vector*,
    /// whose order unit 3b changes. Deriving from an **emission-ordered id stack**
    /// is exactly right — that is why `ability_index()` is `spells_emitted.len()`
    /// and has no counter of its own.
    ///
    /// **UNIT-4 PRECONDITION.** This is a bare counter rather than
    /// `triggers_emitted.len()` only because no `triggers.pop()` exists anywhere in
    /// the parser (control-verified against the `result.abilities.pop()` that does).
    /// Triggers are insert-only, so the counter cannot disagree with `items`.
    ///
    /// Unit 4 makes preprocessors emit through this builder. The first
    /// `take_last_trigger` — or any API handing out a `&mut OracleNodeIr` able to
    /// change an emitted item's variant — lets this counter disagree with `items`
    /// and reintroduces the CR 707.9a two-triggers-one-slot collision. Convert it
    /// to a symmetric `triggers_emitted: Vec<OracleItemId>` **before** either lands.
    ///
    /// Do **not** "derive" it by filtering `items` for trigger variants. That
    /// couples the printed-slot index to variant mutation (flip a `Trigger` to a
    /// `Keyword` and the derived index silently decrements) and adds a third
    /// representation of a fact `spells_emitted` already models correctly with a
    /// stack.
    trigger_index: usize,
    /// Ids of emitted spells, in **emission** order — and the sole authority for
    /// the CR 707.9a ability index, which is simply `spells_emitted.len()`.
    ///
    /// There is deliberately no separate `ability_index: usize`. A counter beside
    /// this vector could disagree with it (push without increment, pop without
    /// decrement, or a rejected `emit` mutating one but not the other); deriving
    /// the index from the vector's length makes disagreement unrepresentable
    /// rather than merely tested for. `take_last_spell` pops here, not from the
    /// source-ordered item map, because slots are assigned at *emission* time.
    spells_emitted: Vec<OracleItemId>,
}

/// A reserved item slot: the id exists, the payload does not yet.
#[derive(Debug)]
pub(crate) struct ItemSlot {
    id: OracleItemId,
    source: OracleUnitSource,
    allocator: UnitAllocator,
}

#[allow(dead_code)] // production caller lands in unit 3b.
impl ItemSlot {
    pub(crate) fn id(&self) -> OracleItemId {
        self.id
    }

    pub(crate) fn source(&self) -> &OracleUnitSource {
        &self.source
    }

    pub(crate) fn allocator(&mut self) -> &mut UnitAllocator {
        &mut self.allocator
    }
}

impl OracleDocBuilder {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// The printed slot the next emitted ability will occupy (CR 707.9a). Read
    /// before emission. This — not `Vec::len()` — is the authority in unit 3b.
    ///
    /// **UNIT-3B PRECONDITION, do not lose this.** An ability's *real* printed slot
    /// is its source rank among spells: `lower_oracle_ir` fills `result.abilities`
    /// by iterating `ir.items`, which is key-ordered by source position. This
    /// counter instead reports the *emission* rank. The two agree only while `emit`
    /// is called in nondecreasing `first_line` order — which unit 3a satisfies by
    /// accident (every span is `WholeDocument`, so the key degenerates to the
    /// emission ordinal) and which unit 3b's dispatch-loop emission must either
    /// guarantee or replace.
    ///
    /// So unit 3b must do one of: (a) assert `emit` is called in nondecreasing
    /// `first_line` order, or (b) derive both counters at `finish()` from the
    /// source-ordered item vector rather than at `emit()`. Option (b) is the honest
    /// one; (a) merely re-states today's accident as a rule.
    #[allow(dead_code)] // becomes the sole index authority in unit 3b.
    pub(crate) fn ability_index(&self) -> PrintedAbilityIndex {
        PrintedAbilityIndex(self.spells_emitted.len())
    }

    /// The next trigger's printed index (CR 707.9a). Read before emission.
    #[allow(dead_code)] // becomes the sole index authority in unit 3b.
    pub(crate) fn trigger_index(&self) -> PrintedTriggerIndex {
        PrintedTriggerIndex(self.trigger_index)
    }

    /// Reserve an `OracleItemId` and its header unit **before** branch parsing,
    /// returning an item-scoped allocator for nested units.
    pub(crate) fn begin_item(
        &mut self,
        span: OracleSourceSpan,
        fragment: Option<&str>,
    ) -> ItemSlot {
        let id = OracleItemId(self.next_item_id);
        self.next_item_id += 1;
        let source = OracleUnitSource::new(
            OracleUnitId {
                item: id,
                ordinal: 0,
            },
            span.clone(),
            fragment,
        );
        ItemSlot {
            id,
            source,
            allocator: UnitAllocator::new(id, span),
        }
    }

    /// Commit a reserved slot with its parsed payload.
    ///
    /// Rejects a duplicate `(first_line, ordinal_within_span)` position, any
    /// sibling whose byte span conflicts (overlapping bytes at the same ordinal),
    /// and any item whose fragment presence contradicts its span precision.
    pub(crate) fn emit(
        &mut self,
        slot: ItemSlot,
        node: OracleNodeIr,
    ) -> Result<OracleItemId, DocBuilderError> {
        slot.source.check_fragment_precision()?;
        let span = slot.source.span.clone();
        let key = (span.first_line, span.ordinal_within_span);
        if self.items.contains_key(&key) {
            return Err(DocBuilderError::DuplicateItemPosition { span });
        }
        for existing in self.items.values() {
            if existing.source.span.conflicts_with(&span) {
                return Err(DocBuilderError::OverlappingSiblingSpans {
                    first: existing.source.span.clone(),
                    second: span,
                });
            }
        }
        // CR 707.9a printed-slot counters. EXHAUSTIVE ON PURPOSE — no `_` arm.
        // A pre-lowered spell/trigger occupies a printed slot exactly like a core
        // one, so it must advance the counter. A wildcard here would silently
        // mis-index every card that reaches a preprocessor. When a variant is
        // added, the compiler must ask whether it consumes a printed slot.
        match node {
            // Pushing IS the increment: `ability_index()` reads `spells_emitted.len()`.
            // Reached only after the duplicate/conflict/fragment early-returns above,
            // so a rejected `emit` mutates neither counter.
            OracleNodeIr::Spell(_) | OracleNodeIr::PreLoweredSpell(_) => {
                self.spells_emitted.push(slot.id);
            }
            OracleNodeIr::Trigger(_) | OracleNodeIr::PreLoweredTrigger(_) => {
                self.trigger_index += 1
            }
            OracleNodeIr::Static(_)
            | OracleNodeIr::PreLoweredStatic(_)
            | OracleNodeIr::Replacement(_)
            | OracleNodeIr::PreLoweredReplacement(_)
            | OracleNodeIr::Keyword(_)
            | OracleNodeIr::Modal(_)
            | OracleNodeIr::AdditionalCost(_)
            | OracleNodeIr::CastingRestriction(_)
            | OracleNodeIr::CastingOption(_)
            | OracleNodeIr::SolveCondition(_)
            | OracleNodeIr::StriveCost(_) => {}
        }
        let id = slot.id;
        self.items.insert(
            key,
            OracleItemIr {
                id,
                source: slot.source,
                node,
            },
        );
        Ok(id)
    }

    /// Remove the most recently **emitted** spell item, returning it. The typed
    /// replacement for `result.abilities.pop()` (`oracle.rs`), used by cross-line
    /// "instead" composition, which folds the previous ability into the new one.
    ///
    /// **Emission order, not source order — this is the whole correctness argument.**
    /// `emit` assigns a printed slot from `ability_index` at *emission* time, while
    /// `items` is a `BTreeMap` keyed by *source* position and this builder
    /// explicitly supports out-of-order emission (see
    /// `builder_returns_items_in_source_order_regardless_of_emission_order`). Popping
    /// the greatest source key would therefore free the wrong slot: emit spell A at
    /// line 5 (slot 0), then spell B at line 1 (slot 1); the max key is A, so taking
    /// it decrements `ability_index` to 1 while B still holds slot 1 — and the next
    /// emitted spell is issued slot 1 as well. Two abilities, one CR 707.9a printed
    /// slot, and a copy's "except it has this ability" binds the wrong one, silently.
    /// Popping `spells_emitted` mirrors `Vec::pop()` on the emission-ordered
    /// `result.abilities` exactly, which is what this method replaces.
    ///
    /// **Takes no id, on purpose.** An id-taking `take_item` would accept *any* item
    /// while implementing `pop()` semantics, expressing that same slot collision.
    /// Restricting the taker to the last emitted spell makes the call
    /// unrepresentable rather than merely discouraged.
    ///
    /// It cannot remove a trigger, so the trigger counter can never drift. There is
    /// no counter to underflow: the ability index *is* `spells_emitted.len()`, so
    /// popping the vector and freeing the printed slot are the same operation. An
    /// empty builder yields `None` from the first `?`.
    ///
    /// Exactly mirrors production, verified: `result.abilities.pop()` is the only
    /// category `pop()` in the parser — no `triggers.pop()`, `statics.pop()`, or
    /// `replacements.pop()` exists.
    // Production caller lands in unit 3b: the `result.abilities.pop()` in
    // `oracle.rs`. Named by symbol, not by line — a line number in a comment is an
    // unchecked claim, and the one that stood here had already rotted inside this
    // very diff, moved by lines the diff itself added above it.
    #[allow(dead_code)]
    pub(crate) fn take_last_spell(&mut self) -> Option<OracleItemIr> {
        let id = self.spells_emitted.pop()?;
        let key = *self
            .items
            .iter()
            .find(|(_, i)| i.id == id)
            .map(|(k, _)| k)
            .expect("spells_emitted holds only ids that `emit` inserted");
        // Popping IS the decrement: the freed printed slot is the popped spell's.
        // `spells_emitted` can never name an id absent from `items` — `emit` inserts
        // both together, and this is the only removal path.
        self.items.remove(&key)
    }

    /// Finish, producing items already in Oracle source order.
    pub(crate) fn finish(
        self,
        source_text: &str,
        card_name: &str,
        diagnostics: Vec<OracleDiagnostic>,
    ) -> OracleDocIr {
        OracleDocIr {
            items: self.items.into_values().collect(),
            source_text: source_text.to_string(),
            card_name: card_name.to_string(),
            diagnostics,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(first_line: usize, start: usize, end: usize, ordinal: u32) -> OracleSourceSpan {
        OracleSourceSpan::exact(first_line, first_line, start, end, ordinal)
    }

    #[test]
    fn builder_returns_items_in_source_order_regardless_of_emission_order() {
        let mut b = OracleDocBuilder::new();
        // Emit line 2 before line 0 — the builder must still order by source.
        let s2 = b.begin_item(span(2, 20, 30, 0), Some("Creatures you control get +1/+1."));
        b.emit(s2, OracleNodeIr::Keyword(Keyword::Flying)).unwrap();
        let s0 = b.begin_item(span(0, 0, 10, 0), Some("Flying"));
        b.emit(s0, OracleNodeIr::Keyword(Keyword::Vigilance))
            .unwrap();

        let doc = b.finish(
            "Flying\n\nCreatures you control get +1/+1.",
            "Probe",
            vec![],
        );
        let lines: Vec<usize> = doc
            .items
            .iter()
            .map(|i| i.source.span().first_line)
            .collect();
        assert_eq!(
            lines,
            vec![0, 2],
            "items must be ordered by source position"
        );
    }

    /// One physical line may yield two items (`Kicker {2}{G}` → `Keyword` +
    /// `AdditionalCost`). They share a byte span and are separated by
    /// `ordinal_within_span`, so the overlap rule must not reject them.
    #[test]
    fn kicker_two_items_from_one_line_do_not_trip_overlap_rejection() {
        let mut b = OracleDocBuilder::new();
        let kw = b.begin_item(span(0, 0, 14, 0), Some("Kicker {2}{G}"));
        b.emit(kw, OracleNodeIr::Keyword(Keyword::Flying)).unwrap();

        let cost = b.begin_item(span(0, 0, 14, 1), Some("Kicker {2}{G}"));
        let emitted = b.emit(cost, OracleNodeIr::Keyword(Keyword::Vigilance));
        assert!(
            emitted.is_ok(),
            "co-located items with distinct ordinals must be accepted: {emitted:?}"
        );
        assert_eq!(b.finish("Kicker {2}{G}", "Probe", vec![]).items.len(), 2);
    }

    #[test]
    fn overlapping_sibling_spans_at_same_ordinal_are_rejected() {
        let mut b = OracleDocBuilder::new();
        let a = b.begin_item(span(0, 0, 10, 0), Some("abc"));
        b.emit(a, OracleNodeIr::Keyword(Keyword::Flying)).unwrap();
        let c = b.begin_item(span(1, 5, 15, 0), Some("def"));
        let err = b.emit(c, OracleNodeIr::Keyword(Keyword::Vigilance));
        assert!(
            matches!(err, Err(DocBuilderError::OverlappingSiblingSpans { .. })),
            "overlapping bytes at the same ordinal must be rejected, got {err:?}"
        );
    }

    #[test]
    fn duplicate_item_position_is_rejected() {
        let mut b = OracleDocBuilder::new();
        let a = b.begin_item(span(0, 0, 5, 0), Some("abc"));
        b.emit(a, OracleNodeIr::Keyword(Keyword::Flying)).unwrap();
        let c = b.begin_item(span(0, 0, 5, 0), Some("abc"));
        assert!(matches!(
            b.emit(c, OracleNodeIr::Keyword(Keyword::Vigilance)),
            Err(DocBuilderError::DuplicateItemPosition { .. })
        ));
    }

    /// DISCRIMINATING. Nested parsers hold the `UnitAllocator`, handed out by
    /// `ItemSlot::allocator()` and threaded through `ParseContext` — NOT the
    /// `ItemSlot` itself. So the containment check has to live where the allocator
    /// is, or the intended caller cannot reach it.
    ///
    /// Two earlier shapes both failed:
    ///   * builder-side `validate_child_span(id, ..)` — the item is not in the map
    ///     between `begin_item` and `emit`, which is exactly when nested parsers
    ///     run, so it returned `Ok(())` for every child. Fail-open.
    ///   * slot-side `validate_child_span(..)` — unreachable from a detached
    ///     allocator, and `OracleUnitSource`'s fields were `pub(crate)`, so a nested
    ///     parser could forge one anyway. Advisory, not enforced.
    ///
    /// `allocate_with_span` is the only way to mint a child `OracleUnitSource`, and
    /// it validates. This test allocates BEFORE `emit`.
    #[test]
    fn allocator_rejects_child_span_outside_item_before_emit() {
        let mut b = OracleDocBuilder::new();
        let mut slot = b.begin_item(span(0, 0, 10, 0), Some("abcdefghij"));
        let alloc = slot.allocator();

        let ok = alloc.allocate_with_span(span(0, 2, 8, 1), Some("cdefgh"));
        assert!(
            ok.is_ok(),
            "a contained child must be accepted pre-emit: {ok:?}"
        );

        let bad = alloc.allocate_with_span(span(0, 2, 30, 2), Some("x"));
        assert!(
            matches!(bad, Err(DocBuilderError::ChildSpanOutsideItem { .. })),
            "an out-of-range child must be REJECTED pre-emit, got {bad:?}"
        );

        // Fragment/precision coupling is enforced on children too. This span IS
        // contained (0..10 of a 10-byte document), so only the precision mismatch
        // can reject it — the assertion cannot pass for the wrong reason.
        let mism = alloc.allocate_with_span(
            OracleSourceSpan::whole_document("abcdefghij", 3),
            Some("abc"),
        );
        assert!(
            matches!(mism, Err(DocBuilderError::FragmentPrecisionMismatch { .. })),
            "a WholeDocument child carrying a fragment must be rejected, got {mism:?}"
        );

        // ...and the converse: an Exact span with no fragment is equally rejected.
        let missing = alloc.allocate_with_span(span(0, 2, 6, 4), None);
        assert!(
            matches!(
                missing,
                Err(DocBuilderError::FragmentPrecisionMismatch { .. })
            ),
            "an Exact child without a fragment must be rejected, got {missing:?}"
        );

        b.emit(slot, OracleNodeIr::Keyword(Keyword::Flying))
            .unwrap();
    }

    /// DISCRIMINATING, and the reason `take_last_spell` pops `spells_emitted`
    /// rather than the source-ordered map.
    ///
    /// `emit` assigns the printed slot at EMISSION time; `items` is keyed by SOURCE
    /// position and out-of-order emission is a supported contract. Taking the
    /// greatest source key would free the wrong slot: A@line5 holds slot 0, B@line1
    /// holds slot 1; popping A leaves B on slot 1 while `ability_index` says the
    /// next spell also gets slot 1. Two abilities, one CR 707.9a printed slot.
    #[test]
    fn take_last_spell_pops_emission_order_not_source_order() {
        let mut b = OracleDocBuilder::new();

        // Emit LATER source line first — the builder's advertised contract.
        let a = b.begin_item(span(5, 50, 60, 0), Some("{T}: Add {G}."));
        let a_id = a.id();
        b.emit(a, spell_node()).unwrap(); // printed slot 0
        let bb = b.begin_item(span(1, 10, 20, 0), Some("{T}: Add {R}."));
        let b_id = bb.id();
        b.emit(bb, spell_node()).unwrap(); // printed slot 1
        assert_eq!(b.ability_index().get(), 2);

        let taken = b.take_last_spell().expect("a spell is present");
        assert_eq!(
            taken.id, b_id,
            "must pop the LAST EMITTED spell (slot 1), not the greatest source key (line 5)"
        );
        assert_eq!(
            b.ability_index().get(),
            1,
            "the freed slot must be the one the popped spell held"
        );

        // The survivor is the earlier-emitted spell, still holding slot 0.
        let doc = b.finish("x", "Probe", vec![]);
        assert_eq!(doc.items.len(), 1);
        assert_eq!(doc.items[0].id, a_id);
    }

    /// FIX G: nothing asserted this after `last_spell_id`'s removal. Three of us
    /// reasoned it was trivially `None`; none of us had run it.
    #[test]
    fn take_last_spell_on_an_empty_builder_is_none() {
        let mut b = OracleDocBuilder::new();
        assert!(b.take_last_spell().is_none(), "no spell has been emitted");
        assert_eq!(
            b.ability_index().get(),
            0,
            "and no printed slot was consumed"
        );
    }

    /// DISCRIMINATING. A take followed by a re-emit is where an off-by-one hides:
    /// `spells_emitted` must be a true stack, and `ability_index` must track it.
    ///
    /// It cannot drift, because there is no separate counter — `ability_index()` IS
    /// `spells_emitted.len()`. This test pins that: cross-line "instead" composition
    /// takes the previous ability and emits the composed one in its place, so the
    /// take → emit → take cycle is the production path, not a synthetic one.
    #[test]
    fn take_then_reemit_then_take_returns_the_reemitted_spell() {
        let mut b = OracleDocBuilder::new();

        let a = b.begin_item(span(0, 0, 10, 0), Some("{T}: Add {G}."));
        let a_id = a.id();
        b.emit(a, spell_node()).unwrap();
        let c = b.begin_item(span(1, 11, 21, 0), Some("{T}: Add {R}."));
        let c_id = c.id();
        b.emit(c, spell_node()).unwrap();
        assert_eq!(b.ability_index().get(), 2);

        // Take the last emitted spell (C), as the "instead" composition does.
        assert_eq!(b.take_last_spell().unwrap().id, c_id);
        assert_eq!(b.ability_index().get(), 1, "C's slot is freed");

        // Re-emit a composed ability in its place.
        let composed = b.begin_item(span(1, 11, 21, 0), Some("{T}: Add {R}."));
        let composed_id = composed.id();
        b.emit(composed, spell_node()).unwrap();
        assert_eq!(
            b.ability_index().get(),
            2,
            "the composed ability retakes slot 1"
        );
        assert_ne!(composed_id, c_id, "a fresh item id, not a resurrected one");

        // The next take must return the RE-EMITTED spell, not A and not stale C.
        let second = b.take_last_spell().expect("a spell is present");
        assert_eq!(
            second.id, composed_id,
            "the stack must return the most recently emitted spell after a re-emit"
        );
        assert_eq!(b.ability_index().get(), 1);

        // A survives, still on slot 0, and is the last one left.
        assert_eq!(b.take_last_spell().unwrap().id, a_id);
        assert_eq!(b.ability_index().get(), 0);
        assert!(b.take_last_spell().is_none(), "drained");
    }

    /// The allocator's only public way out is `allocate_with_span`, which
    /// validates. `allocate()` is private: an id handed to a nested parser without
    /// a checked span is the forge path this design closes. Ordinal 0 is the item's
    /// own header unit, so children start at 1 and never collide.
    #[test]
    fn allocator_issues_distinct_child_ordinals_starting_after_the_header() {
        let mut b = OracleDocBuilder::new();
        let mut slot = b.begin_item(span(0, 0, 10, 0), Some("abcdefghij"));
        let header = slot.source().id();
        assert_eq!(header.ordinal, 0, "the item's own unit is ordinal 0");

        let alloc = slot.allocator();
        let u1 = alloc
            .allocate_with_span(span(0, 0, 4, 1), Some("abcd"))
            .expect("contained child");
        let u2 = alloc
            .allocate_with_span(span(0, 4, 8, 2), Some("efgh"))
            .expect("contained child");

        assert_ne!(
            u1.id().ordinal,
            u2.id().ordinal,
            "ordinals must be distinct"
        );
        assert_ne!(
            u1.id().ordinal,
            0,
            "children never reuse the header ordinal"
        );
        assert_eq!(u1.id().item, u2.id().item, "both belong to the same item");
    }

    fn spell_node() -> OracleNodeIr {
        OracleNodeIr::PreLoweredSpell(AbilityDefinition::new(
            crate::types::ability::AbilityKind::Spell,
            crate::types::ability::Effect::NoOp,
        ))
    }

    fn trigger_node() -> OracleNodeIr {
        OracleNodeIr::PreLoweredTrigger(TriggerDefinition::new(
            crate::types::triggers::TriggerMode::ChangesZone,
        ))
    }

    /// CR 707.9a: the printed index must be an explicit emission counter, not a
    /// vector length. A source-ordered builder interleaves categories, so a
    /// trigger emitted between two abilities must not shift the ability index.
    ///
    /// This asserts the *positive* direction too: a spell/trigger item MUST
    /// advance its counter. An earlier draft only checked that a keyword item
    /// advanced neither, which passes vacuously even if `emit` never counts.
    #[test]
    fn printed_indices_are_explicit_counters_not_vector_lengths() {
        let mut b = OracleDocBuilder::new();
        assert_eq!(b.ability_index().get(), 0);
        assert_eq!(b.trigger_index().get(), 0);

        // Keyword advances neither.
        let kw = b.begin_item(span(0, 0, 5, 0), Some("Flying"));
        b.emit(kw, OracleNodeIr::Keyword(Keyword::Flying)).unwrap();
        assert_eq!(
            b.ability_index().get(),
            0,
            "keyword must not consume an ability slot"
        );
        assert_eq!(
            b.trigger_index().get(),
            0,
            "keyword must not consume a trigger slot"
        );

        // Spell advances only the ability index.
        let a0 = b.begin_item(span(1, 10, 20, 0), Some("{T}: Add {G}."));
        b.emit(a0, spell_node()).unwrap();
        assert_eq!(b.ability_index().get(), 1);
        assert_eq!(b.trigger_index().get(), 0);

        // A trigger emitted BETWEEN two abilities must not shift the ability index.
        let t0 = b.begin_item(span(2, 21, 40, 0), Some("When this enters, draw a card."));
        b.emit(t0, trigger_node()).unwrap();
        assert_eq!(
            b.ability_index().get(),
            1,
            "trigger must not consume an ability slot"
        );
        assert_eq!(b.trigger_index().get(), 1);

        let a1 = b.begin_item(span(3, 41, 55, 0), Some("{T}: Add {R}."));
        b.emit(a1, spell_node()).unwrap();
        assert_eq!(
            b.ability_index().get(),
            2,
            "second ability must occupy slot 1, i.e. next index 2, despite the interleaved trigger"
        );
        assert_eq!(b.trigger_index().get(), 1);

        // `take_last_spell` frees the printed slot again (cross-line "instead"
        // composition, oracle.rs `result.abilities.pop()`).
        b.take_last_spell()
            .expect("a spell was emitted, so one must be takeable");
        assert_eq!(
            b.ability_index().get(),
            1,
            "removing a spell must free its printed slot"
        );
        assert_eq!(b.trigger_index().get(), 1);
    }

    /// Discriminating guard for the `emit` counter match: pre-lowered payloads
    /// occupy printed slots exactly like core ones. A `_ => {}` wildcard would
    /// silently skip them and mis-index every preprocessor-routed card.
    #[test]
    fn prelowered_payloads_consume_printed_slots() {
        let mut b = OracleDocBuilder::new();
        let s = b.begin_item(span(0, 0, 5, 0), Some("a"));
        b.emit(s, spell_node()).unwrap();
        let t = b.begin_item(span(1, 6, 10, 0), Some("b"));
        b.emit(t, trigger_node()).unwrap();
        assert_eq!((b.ability_index().get(), b.trigger_index().get()), (1, 1));
    }

    /// DISCRIMINATING. `take_last_spell` must remove the LAST spell, never an
    /// earlier one, and must leave the trigger counter alone.
    ///
    /// The replaced `take_item(id)` accepted any id while implementing `pop()`
    /// counter semantics: taking `a0` here would decrement `ability_index` to 1
    /// while `a1` still occupies printed slot 1, so the next emitted spell would
    /// be issued slot 1 as well — two abilities, one CR 707.9a slot. `take_item`
    /// let a caller express that; `take_last_spell` cannot.
    #[test]
    fn take_last_spell_removes_the_last_spell_and_leaves_triggers_alone() {
        let mut b = OracleDocBuilder::new();
        let a0 = b.begin_item(span(0, 0, 10, 0), Some("{T}: Add {G}."));
        let a0_id = a0.id();
        b.emit(a0, spell_node()).unwrap();

        let t0 = b.begin_item(span(1, 11, 30, 0), Some("When this enters, draw a card."));
        b.emit(t0, trigger_node()).unwrap();

        let a1 = b.begin_item(span(2, 31, 45, 0), Some("{T}: Add {R}."));
        let a1_id = a1.id();
        b.emit(a1, spell_node()).unwrap();
        assert_eq!((b.ability_index().get(), b.trigger_index().get()), (2, 1));

        let taken = b.take_last_spell().expect("a spell is present");
        assert_eq!(taken.id, a1_id, "must take the LAST spell, not the first");
        assert_eq!(
            b.ability_index().get(),
            1,
            "the freed slot is the last one; a0 keeps slot 0"
        );
        assert_eq!(
            b.trigger_index().get(),
            1,
            "taking a spell must never move the trigger counter"
        );

        // a0 survives, and the interleaved trigger is untouched.
        let doc = b.finish("x", "Probe", vec![]);
        assert_eq!(doc.items.len(), 2);
        assert_eq!(doc.items[0].id, a0_id);

        // Exhausted: no spell left to take.
        let mut b2 = OracleDocBuilder::new();
        let t = b2.begin_item(span(0, 0, 5, 0), Some("t"));
        b2.emit(t, trigger_node()).unwrap();
        assert!(
            b2.take_last_spell().is_none(),
            "a trigger is not a spell and must not be taken"
        );
        assert_eq!(b2.trigger_index().get(), 1);

        // A keyword is not a spell either. (Also guards the pre-lowered matcher:
        // `spell_node()` is a `PreLoweredSpell`, so a `Spell(_)`-only match in
        // `take_last_spell` would make every assertion above fail.)
        let mut b3 = OracleDocBuilder::new();
        let kw = b3.begin_item(span(0, 0, 6, 0), Some("Flying"));
        b3.emit(kw, OracleNodeIr::Keyword(Keyword::Flying)).unwrap();
        assert!(b3.take_last_spell().is_none(), "a keyword is not a spell");
        assert_eq!(b3.ability_index().get(), 0);
    }

    /// A whole-document span must be self-describing: `first_line == 0` is "we
    /// don't know the line", not "line 0". `SpanPrecision` carries that.
    #[test]
    fn span_precision_distinguishes_coarse_from_exact() {
        let coarse = OracleSourceSpan::whole_document("Flying\n{T}: Add {G}.", 0);
        assert_eq!(coarse.precision, SpanPrecision::WholeDocument);
        assert!(
            !coarse.is_exact(),
            "a renderer must be able to refuse to print line 0 for this span"
        );

        let exact = OracleSourceSpan::exact(1, 1, 7, 20, 0);
        assert_eq!(exact.precision, SpanPrecision::Exact);
        assert!(exact.is_exact());

        // Both are true containing spans; precision is orthogonal to containment.
        assert!(exact.is_contained_by(&coarse));
    }

    /// The 3a document span is coarse but true: it contains the item, and
    /// distinct ordinals keep co-located siblings from tripping the overlap rule.
    #[test]
    fn whole_document_span_contains_items_and_never_conflicts() {
        let text = "Flying\n{T}: Add {G}.";
        let first = OracleSourceSpan::whole_document(text, 0);
        let second = OracleSourceSpan::whole_document(text, 1);
        assert!(first.is_contained_by(&second) && second.is_contained_by(&first));
        assert!(
            !first.conflicts_with(&second),
            "distinct ordinals must not conflict even with identical byte ranges"
        );
        assert!(
            first.conflicts_with(&first.clone()),
            "same ordinal + overlapping bytes must still conflict (rule is live)"
        );
        assert_eq!(first.last_line, 1);
        assert_eq!(first.end_byte, text.len());
    }
}
