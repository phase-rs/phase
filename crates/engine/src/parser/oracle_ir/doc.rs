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
//! The builder orders items by `(first_line, ordinal)`, and every producer —
//! the dispatch loop and every preprocessor, Class included — emits through
//! `DocEmitter` at the item's printed source line. Items are therefore genuinely
//! source-ordered, and every span is `SpanPrecision::Exact` or the honest
//! chain-local `SpanPrecision::ChainRelative`.

// NOTE ON `dead_code`: suppression in this module is **per item**, never
// module-wide. A module-wide `#![allow(dead_code)]` also silences code not yet
// written, and it is precisely what hid two silent defects here during review
// (an `emit` counter that skipped every `PreLowered*` payload, and a
// `validate_child_span` that failed open). Each suppression below names the unit
// that gives the item a production caller, and dies when that unit lands.

use std::collections::BTreeMap;

use super::diagnostic::OracleDiagnostic;
use super::effect_chain::EffectChainIr;
use super::relation::DocumentRelationIr;
use super::replacement::ReplacementIr;
use super::static_ir::StaticIr;
use super::trigger::TriggerIr;
use crate::types::ability::{
    AbilityDefinition, AdditionalCost, CastingPermission, CastingRestriction,
    ContinuousModification, Effect, ModalChoice, ReplacementDefinition, SolveCondition,
    SpellCastingOption, StaticDefinition, TriggerDefinition, VoteSubject,
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
///
/// `Deserialize` + `pub`: this id is part of the `OracleDiagnostic::SwallowedClause`
/// wire payload (`CardFace::parse_warnings` → `card-data.json`), so it must survive a
/// round-trip through the card DB loaders. Re-exported from `oracle_ir::diagnostic`
/// so consumers outside this crate-private module can name it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct OracleItemId(pub u32);

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
/// *card-absolute*. A consumer that renders a position (Plan 02's per-unit
/// diagnostics) must be able to tell the difference: printing a chain-local byte
/// offset as a card position is a precise-looking wrong answer, which is worse
/// than an admittedly coarse one.
///
/// Typed rather than a `bool` per CLAUDE.md: a future `LineOnly` precision (line
/// known, byte range not) is an enum value, not a second flag.
// No `Ord`: `Exact < ChainRelative` would be a meaningless magnitude claim on a
// qualifier this enum's own docs call orthogonal to containment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SpanPrecision {
    /// The span is the unit's exact byte/line extent. Safe to render.
    Exact,
    /// The span's byte range is exact **within the effect chain** it was parsed
    /// from, but NOT card-absolute: it is an offset into a single
    /// `parse_effect_chain_ir` invocation's text, not into the whole Oracle
    /// document. Minted by the item-scoped `ClauseIrBuilder`, whose local
    /// allocator is seeded over the chain text because the document allocator is
    /// not yet threaded through `ParseContext` (same wall `AbilityIr` documents).
    ///
    /// It is HONEST, not fabricated: the offset is truthful relative to the
    /// chain, and the verbatim `fragment` is retained so a later
    /// allocator-threading unit can upgrade it to a card-absolute
    /// `SpanPrecision::Exact` by adding the chain's base offset — or, if
    /// preprocessing was not offset-linear, by re-locating the fragment in the
    /// card text. A renderer must NOT print `first_line`/`start_byte` as a
    /// card-absolute position.
    ///
    /// U5 DEBT — upgrades to `Exact` when the allocator is threaded.
    ChainRelative,
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
///
/// `Deserialize` + `pub`: a span is part of the `OracleDiagnostic::SwallowedClause` wire
/// payload (`CardFace::parse_warnings` → `card-data.json`), so it must survive a
/// round-trip through the card DB loaders. Re-exported from `oracle_ir::diagnostic`.
///
/// # The span an audit diagnostic carries is a UNIT span, and it is LINE-GRANULAR
///
/// `Exact` on a diagnostic's `unit_span` means the bounds locate **the audit unit**
/// exactly — not a clause within it. No producer mints a sub-line ITEM span:
/// `DocEmitter::exact_span` hands every item on a line the whole line's byte range
/// (`byte_range(line)`), so two clauses on one physical line are addressed by the same
/// bytes and are distinguished only by `ordinal_within_span`. `audit_units` therefore
/// groups them into ONE unit, and both share one `unit_span` — the line-granularity
/// ceiling `feature.rs` documents. A renderer must not present a `unit_span` as a
/// *clause* position. When the recognizer bring-up gives items real sub-line spans, the
/// units subdivide on their own and this narrows with no change here.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct OracleSourceSpan {
    pub first_line: usize,
    pub last_line: usize,
    pub start_byte: usize,
    pub end_byte: usize,
    /// Whether the bounds above locate this unit exactly. See `SpanPrecision`.
    pub precision: SpanPrecision,
    pub ordinal_within_span: u32,
}

impl OracleSourceSpan {
    /// An exactly-located span. The constructor unit 3b uses once emission moves
    /// into the dispatch loop and the real line/byte range is in hand.
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

    /// A span whose byte range is exact **within one effect chain** but not
    /// card-absolute. See `SpanPrecision::ChainRelative`. Minted by
    /// `ClauseIrBuilder` for per-clause provenance; carries a verbatim fragment
    /// (the guard in `check_fragment_precision` requires it) so the eventual
    /// allocator-threading unit can upgrade it to `Exact`.
    pub(crate) fn chain_relative(
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
            precision: SpanPrecision::ChainRelative,
            ordinal_within_span,
        }
    }

    /// True when this span's bounds locate the unit exactly **card-absolutely**.
    /// A position renderer must consult this before printing a line number or
    /// byte offset as a card position. `ChainRelative` is deliberately NOT exact:
    /// its offsets are truthful only within a single chain.
    ///
    /// The fragment-precision guard now consults the widened `carries_fragment`
    /// (which admits `ChainRelative`), so this exact-only accessor has no
    /// production caller in M1 — it is retained solely as the card-absolute
    /// distinction the precision-tier tests assert on, hence `#[cfg(test)]`
    /// rather than a dead-code allow.
    #[cfg(test)]
    pub(crate) fn is_exact(&self) -> bool {
        matches!(self.precision, SpanPrecision::Exact)
    }

    /// True when this precision tier is allowed to carry a verbatim `fragment`.
    ///
    /// Both live tiers — `Exact` (card-absolute) and `ChainRelative` (chain-local
    /// but honest) — address a concrete byte range and therefore have a real
    /// verbatim fragment, so this is currently true for every span. That is not a
    /// tautology to be collapsed into `true`: it is the single authority the
    /// fail-closed `check_fragment_precision` guard consults, and because the
    /// match is exhaustive, a future non-locating tier (the `LineOnly` this
    /// enum's docs anticipate) cannot be added without deciding here whether it
    /// may carry a fragment. The retired `WholeDocument` tier was the `false`
    /// case: its sole honest "fragment" would have been the entire card.
    pub(crate) fn carries_fragment(&self) -> bool {
        matches!(
            self.precision,
            SpanPrecision::Exact | SpanPrecision::ChainRelative
        )
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
    /// The verbatim Oracle text this unit covers — present exactly when
    /// `span.precision.carries_fragment()`, which today is every tier.
    ///
    /// A tier that cannot locate the unit must NOT report a fragment: handing a
    /// diagnostic renderer the whole card as "the offending clause" is a
    /// precise-looking wrong answer, exactly what `SpanPrecision` exists to
    /// prevent — guarding the span while leaving the fragment lying would move
    /// the lie, not remove it. (This was the retired `WholeDocument` tier's case.)
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
        if self.fragment.is_some() == self.span.carries_fragment() {
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
    Spell(EffectChainIr),
    /// Triggered ability.
    // PLAN-05 DEBT (2026-07-17, post-U2): constructed only by the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
    Trigger(TriggerIr),
    /// Static ability.
    // PLAN-05 DEBT (2026-07-17, post-U2): constructed only by the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
    Static(StaticIr),
    /// Replacement effect.
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
    // PLAN-05 DEBT — pre-lowered escape hatches.
    //
    // These four variants carry already-assembled engine definitions rather
    // than typed IR. Unit 4 wired the preprocessors through the document
    // builder, but it did not remove these variants; ordinary dispatch also
    // still emits them. The IR-native `Spell`/`Trigger`/`Static`/`Replacement`
    // siblings are dead-coded pending Plan 05 U2's document-seam hoist.
    //
    // Plan 05, not unit 4, removes these variants after U2--U4 have made every
    // producer IR-native. Do not add a new producer of these variants.
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

            /// A parse-time placeholder printed slot (always `0`).
            ///
            /// CR 707.9a: an "…except it has this ability" clause must resolve to
            /// the ability's **printed** slot, which is its final position in the
            /// source-ordered document. That position is not known while the
            /// dispatch loop is still running, so the loop bakes this placeholder
            /// into `RetainPrinted{Trigger,Ability}FromSource` and `finish()`
            /// overwrites it via `stamp_retained_printed_slot` from the item's
            /// resolved slot.
            ///
            /// This replaces the former `from_category_vector_len` constructor.
            /// Deriving the slot from a category-vector length was correct only
            /// while emission was category-ordered; the source-ordered document
            /// builder makes that equality false, so the late-bind at `finish()`
            /// is the single authority and the length constructor is gone.
            pub(crate) fn placeholder() -> Self {
                Self(0)
            }
        }
    };
}

printed_index_impl!(PrintedAbilityIndex);
printed_index_impl!(PrintedTriggerIndex);

impl PrintedAbilityIndex {
    /// The slot `n` positions after this one. Compound splitting emits several
    /// definitions from one line and needs each one's own slot.
    // PLAN-05 DEBT (2026-07-17, post-U2): used only by the printed-index consumers in the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
    pub(crate) fn offset(self, n: usize) -> Self {
        Self(self.0 + n)
    }
}

impl PrintedTriggerIndex {
    /// The slot `n` positions after this one. Compound splitting emits several
    /// definitions from one line and needs each one's own slot.
    pub(crate) fn offset(self, n: usize) -> Self {
        Self(self.0 + n)
    }
}

/// Test-only constructors. Kept behind `cfg(test)` so production code cannot
/// mint a printed index out of a bare integer: the only production producers are
/// `OracleDocBuilder::{ability_index, trigger_index}` and the const-zero
/// `placeholder()` that the dispatch loop bakes in and `finish()` overwrites.
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
    /// CR 607.2d: Cross-item document relations recovered at parse time by
    /// pairing producer/consumer items by `OracleItemId`, applied by id in
    /// `lower_oracle_ir`. Empty for the vast majority of cards. See `relation`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) relations: Vec<DocumentRelationIr>,
}

impl OracleDocIr {
    /// Look up an item by its stable id. Cross-item lowering binds through this,
    /// never by scanning category vectors for a matching shape.
    // PLAN-05 DEBT (2026-07-17, post-U2): used only by the source-context consumers in the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
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
pub(crate) struct UnitAllocator {
    item: OracleItemId,
    /// The owning item's span. Held here, not on `ItemSlot`, because the
    /// allocator is what nested parsers actually receive (through `ParseContext`)
    /// — see `allocate_with_span`.
    parent_span: OracleSourceSpan,
    next_ordinal: u32,
    issued: Vec<u32>,
}

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

    // PLAN-05 DEBT (2026-07-17, post-U2): used only by the source-context consumers in the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
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
/// Emission order is irrelevant: items are keyed by their source position (see
/// the `items` field for the exact key and why each component is load-bearing)
/// so the final `Vec` is always in Oracle source order. This is what makes the
/// document IR source-ordered rather than category-ordered.
#[derive(Debug, Default)]
pub(crate) struct OracleDocBuilder {
    /// Keyed by `(first_line, start_byte, ordinal_within_span)`.
    ///
    /// `start_byte` is load-bearing even though it is `0` for every item today.
    /// `OracleSourceSpan`'s contract says byte ranges exist "so two clauses on one
    /// physical line remain distinguishable" — but `conflicts_with` only rejects
    /// siblings whose bytes OVERLAP. Two exact, non-overlapping clauses on one
    /// line with the same `ordinal_within_span` therefore pass the conflict check
    /// and then collide on the map key, and a `(first_line, ordinal)` key would
    /// also order same-line siblings by ordinal rather than by position — so an
    /// emitter assigning ordinals out of byte order would have `finish()` return
    /// them reversed, and `lower_oracle_ir` would lower them reversed. Silently.
    ///
    /// Inert until unit 3b emits exact spans; fixed here, before it can fire.
    items: BTreeMap<(usize, usize, u32), OracleItemIr>,
    next_item_id: u32,
    /// Per-line `ordinal_within_span` allocator, keyed on `first_line` ONLY —
    /// deliberately category-BLIND (unit 4).
    ///
    /// The item map key `(first_line, start_byte, ordinal_within_span)` carries no
    /// category, so any two items on the SAME line with the SAME `start_byte` and
    /// ordinal `0` collide as `DuplicateItemPosition` regardless of category. Same-
    /// line multi-category emissions are common (casting_option+trigger,
    /// static+replacement, `push_same_is_true_*` static+ability, multi-keyword
    /// lines, a Saga ETB replacement on the chapter-1 line, a multi-numeral Saga
    /// chapter line — CR 714.2c). A per-site counter cannot see cross-category or
    /// cross-stage siblings and would make those collisions a latent parse failure,
    /// so every emitter that holds `&mut self` draws its ordinal from this ONE map
    /// via `next_ordinal_for_line`: pre-loop scans (strive, the Saga ETB
    /// replacement), the preprocessors, the dispatch loop, and any post-loop
    /// emission.
    ///
    /// Allocation order across different `start_byte`s on one line may hand a
    /// later-byte item a smaller ordinal, but the map key sorts by `start_byte`
    /// BEFORE `ordinal_within_span`, so source order is preserved regardless of the
    /// order in which ordinals are drawn. A `take_last_spell` re-emit does NOT draw
    /// a new ordinal from here — it reuses the popped item's ORIGINAL span+ordinal
    /// (the key it just freed), which is position-preserving; so this allocator only
    /// ever hands out fresh ordinals to genuinely new emissions.
    next_ordinal_by_line: BTreeMap<usize, u32>,
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

impl ItemSlot {
    // PLAN-05 DEBT (2026-07-17, post-U2): used only by the source-context consumers in the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
    pub(crate) fn id(&self) -> OracleItemId {
        self.id
    }

    // PLAN-05 DEBT (2026-07-17, post-U2): used only by the source-context consumers in the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
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

    /// Allocate the next distinct `ordinal_within_span` for `first_line`.
    ///
    /// Single cross-category authority (unit 4): see `next_ordinal_by_line`. Every
    /// emitter holding `&mut self` — pre-loop scans, preprocessors, the dispatch
    /// loop, post-loop emission — draws its ordinal from here so no two items on
    /// one line can collide on the map key. The counter never decrements. A
    /// `take_last_spell` re-emit does NOT call this — it reuses the popped item's
    /// ORIGINAL span+ordinal (the freed key), which is position-preserving — so this
    /// allocator only ever advances for genuinely new emissions.
    pub(crate) fn next_ordinal_for_line(&mut self, first_line: usize) -> u32 {
        let slot = self.next_ordinal_by_line.entry(first_line).or_insert(0);
        let ordinal = *slot;
        *slot += 1;
        ordinal
    }

    /// The printed slot the next emitted ability will occupy (CR 707.9a). Read
    /// before emission. This — not `Vec::len()` — is the authority in unit 3b.
    ///
    /// **PRECONDITION, do not lose this.** An ability's *real* printed slot is its
    /// source rank among spells: `lower_oracle_ir` fills `result.abilities` by
    /// iterating `ir.items`, which is key-ordered by source position. This counter
    /// instead reports the *emission* rank. The two agree only while `emit` is
    /// called in nondecreasing `first_line` order, which no producer is required
    /// to guarantee — a preprocessor may legitimately emit a later line before the
    /// dispatch loop reaches an earlier one.
    ///
    /// That is why the live CR 707.9a authority is NOT this counter but the
    /// per-category stamping walk in `finish()`, which derives each slot from the
    /// source-ordered item map after all emission is done. These accessors remain
    /// only as the read-before-emission form; anything that needs a *true* printed
    /// slot must go through `finish()`.
    // PLAN-05 DEBT (2026-07-17, post-U2): used only by the printed-index consumers in the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
    pub(crate) fn ability_index(&self) -> PrintedAbilityIndex {
        PrintedAbilityIndex(self.spells_emitted.len())
    }

    /// The next trigger's printed index (CR 707.9a). Read before emission.
    // PLAN-05 DEBT (2026-07-17, post-U2): used only by the printed-index consumers in the Class-B bring-up (unit 3); retire this allow there.
    #[allow(dead_code)]
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
        let key = (span.first_line, span.start_byte, span.ordinal_within_span);
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

    /// Peek the most-recently-EMITTED spell node without removing it — insertion
    /// recency, via the `spells_emitted` stack. This is the pop-aware source of
    /// truth for the old `result.abilities.last()` read: `take_last_spell` pops this
    /// stack and the "instead"/min_x re-emit re-pushes, so a peek derived from it is
    /// correct across any pop/re-emit interleave — unlike a clone-on-emit mirror,
    /// which a pop would not revert. Deliberately NOT a `self.items` span maximum:
    /// a preprocessor may emit a higher-line spell before the dispatch loop emits a
    /// lower-line one, and the reader wants the just-emitted loop spell.
    pub(crate) fn peek_last_spell_node(&self) -> Option<&OracleNodeIr> {
        let id = *self.spells_emitted.last()?;
        self.items.values().find(|i| i.id == id).map(|i| &i.node)
    }

    /// Finish, producing items already in Oracle source order.
    pub(crate) fn finish(
        mut self,
        source_text: &str,
        card_name: &str,
        diagnostics: Vec<OracleDiagnostic>,
    ) -> OracleDocIr {
        // CR 707.9a: resolve every "…except it has this ability" printed slot now.
        //
        // The load-bearing invariant is PER-CATEGORY COUNTING, not source order.
        // `values_mut()` visits in source order now that every producer emits at a
        // real line, but the walk never depended on that: it counts each category
        // SEPARATELY (`trigger_slot` among triggers, `ability_slot` among
        // abilities), which is exactly the position `lower_oracle_ir` will give the
        // definition when it re-buckets items into the per-category vectors of
        // `ParsedAbilities`. That is why retiring the category-ordered Class façade
        // — the last producer that visited out of source order — left every stamped
        // slot unchanged. Each `RetainPrinted{Trigger,Ability}FromSource` is a
        // self-reference to its enclosing item (CR 603.1 / CR 602.1), stamped with
        // that item's per-category slot, replacing the `placeholder()` (= 0) the
        // dispatch loop baked in.
        //
        // Match is EXHAUSTIVE over `OracleNodeIr` (no `_`), mirroring `emit`'s
        // printed-slot match above: a future node variant — or the currently
        // never-constructed `Trigger`/`Spell` IR variants once a later commit emits
        // them — must fail to compile here until its slot behavior is decided,
        // rather than being silently skipped (which would mis-index every later
        // trigger/ability).
        let mut trigger_slot = 0usize;
        let mut ability_slot = 0usize;
        for item in self.items.values_mut() {
            match &mut item.node {
                OracleNodeIr::PreLoweredTrigger(trigger) => {
                    stamp_trigger_printed_slot(trigger, trigger_slot, PrintedItemKind::Trigger);
                    trigger_slot += 1;
                }
                OracleNodeIr::PreLoweredSpell(def) => {
                    stamp_retained_printed_slot(def, ability_slot, PrintedItemKind::Ability);
                    ability_slot += 1;
                }
                // No retain modification can reach these today: the `*` IR variants
                // are never constructed in unit 3a, and the remaining categories do
                // not carry a copy-except body. Left explicit (not `_`) so a new
                // slot-bearing node is a compile error, per the note above.
                OracleNodeIr::Trigger(_)
                | OracleNodeIr::Spell(_)
                | OracleNodeIr::Static(_)
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
        }
        OracleDocIr {
            items: self.items.into_values().collect(),
            source_text: source_text.to_string(),
            card_name: card_name.to_string(),
            diagnostics,
            // Relations are recovered in `parse_oracle_ir` after the document is
            // assembled (both the main path and the Class path converge there),
            // where the full source-ordered item list and card types are in hand.
            relations: Vec::new(),
        }
    }
}

/// Which printed category a finish()-time slot stamp targets.
///
/// A walk parameter local to this module — deliberately NOT a `ParseContext`
/// field. `ParseContext`'s `current_trigger_index`/`current_ability_index` carry
/// the parse-time placeholder; this enum only selects which
/// `RetainPrinted*FromSource` variant `finish()` rewrites for a given item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrintedItemKind {
    Trigger,
    Ability,
}

/// Stamp the resolved printed slot into a pre-lowered trigger's body.
///
/// The retain modification lives inside the trigger's `execute` ability
/// (CR 603.1: "this ability" refers to the triggered ability itself), so the
/// walk descends into it.
fn stamp_trigger_printed_slot(trigger: &mut TriggerDefinition, slot: usize, kind: PrintedItemKind) {
    if let Some(execute) = &mut trigger.execute {
        stamp_retained_printed_slot(execute, slot, kind);
    }
}

/// CR 707.9a: rewrite every `RetainPrinted{Trigger,Ability}FromSource` reachable
/// from `def` with the enclosing item's resolved printed slot.
///
/// Mirrors the immutable `collect_effects`/`collect_effects_in_effect`
/// (`analysis/ability_graph.rs`) as a `&mut` walk: the head effect plus the
/// chained `sub_ability`/`else_ability`/`mode_abilities` and the nested-`Effect`
/// ability payloads.
fn stamp_retained_printed_slot(def: &mut AbilityDefinition, slot: usize, kind: PrintedItemKind) {
    stamp_effect_printed_slot(&mut def.effect, slot, kind);
    if let Some(sub) = &mut def.sub_ability {
        stamp_retained_printed_slot(sub, slot, kind);
    }
    if let Some(els) = &mut def.else_ability {
        stamp_retained_printed_slot(els, slot, kind);
    }
    for m in &mut def.mode_abilities {
        stamp_retained_printed_slot(m, slot, kind);
    }
}

/// Stamp the printed slot into a static ability's continuous modifications.
fn stamp_static_printed_slot(sd: &mut StaticDefinition, slot: usize, kind: PrintedItemKind) {
    stamp_retained_mods(&mut sd.modifications, slot, kind);
}

/// Stamp the printed slot into a granted casting permission's modifications.
///
/// Exhaustive over `CastingPermission` (no `_`): a future permission variant that
/// carries continuous modifications must fail to compile until its arm visits
/// them, so a CR 707.9a stamp can never silently miss a carrier here either.
fn stamp_permission_printed_slot(
    permission: &mut CastingPermission,
    slot: usize,
    kind: PrintedItemKind,
) {
    match permission {
        CastingPermission::ExileWithAltCost {
            enters_with_modifications,
            ..
        } => stamp_retained_mods(enters_with_modifications, slot, kind),
        CastingPermission::AdventureCreature | CastingPermission::ExileWithEnergyCost => {}
        CastingPermission::PlayFromExile { .. }
        | CastingPermission::ExileWithAltAbilityCost { .. }
        | CastingPermission::WarpExile { .. }
        | CastingPermission::Plotted { .. }
        | CastingPermission::Foretold { .. } => {}
    }
}

/// Rewrite the printed slot on the retain modifications for `kind`.
///
/// A trigger item only ever holds `RetainPrintedTriggerFromSource`, an ability
/// item only `RetainPrintedAbilityFromSource` — `parse_has_this_ability` selects
/// the variant from whichever `ParseContext` index is set — so the stamp is
/// keyed on `kind`. The modification-variant match keeps a `_` arm on purpose:
/// only two of `ContinuousModification`'s many variants carry a printed slot.
fn stamp_retained_mods(mods: &mut [ContinuousModification], slot: usize, kind: PrintedItemKind) {
    for m in mods.iter_mut() {
        match (kind, m) {
            (
                PrintedItemKind::Trigger,
                ContinuousModification::RetainPrintedTriggerFromSource {
                    source_trigger_index,
                },
            ) => *source_trigger_index = slot,
            (
                PrintedItemKind::Ability,
                ContinuousModification::RetainPrintedAbilityFromSource {
                    source_ability_index,
                },
            ) => *source_ability_index = slot,
            _ => {}
        }
    }
}

/// CR 707.9a: stamp every retain modification reachable from a single `Effect`.
///
/// The match is EXHAUSTIVE ON PURPOSE — no `_` arm. Every `Effect` variant that
/// carries a `Vec<ContinuousModification>` (directly, or via a nested
/// `StaticDefinition`/`TriggerDefinition`/`CastingPermission`), and every variant
/// that carries a nested `AbilityDefinition`, has an explicit arm. A future
/// variant that adds a modification carrier must fail to compile until its arm
/// visits that carrier, so the "…except it has this ability" stamp can never
/// silently miss a slot (the exact defect class this walk closes). Leaf variants
/// carry no printed-slot self-reference and resolve to `=> {}`.
fn stamp_effect_printed_slot(effect: &mut Effect, slot: usize, kind: PrintedItemKind) {
    match effect {
        // ---- Direct Vec<ContinuousModification> carriers ---------------------
        // The "…except it has this ability" copy-except clause (CR 707.9a) lands
        // in one of these on the enclosing trigger/ability.
        Effect::CopySpell {
            additional_modifications,
            ..
        }
        | Effect::CopyTokenOf {
            additional_modifications,
            ..
        }
        | Effect::BecomeCopy {
            additional_modifications,
            ..
        } => stamp_retained_mods(additional_modifications, slot, kind),
        Effect::ReturnAsAura { grants, .. } => stamp_retained_mods(grants, slot, kind),
        Effect::AddPendingEntersModifications { modifications, .. } => {
            stamp_retained_mods(modifications, slot, kind)
        }
        Effect::EachPlayerCopyChosen {
            copy_modifications, ..
        } => stamp_retained_mods(copy_modifications, slot, kind),
        Effect::GrantCastingPermission { permission, .. } => {
            stamp_permission_printed_slot(permission, slot, kind)
        }
        // ---- StaticDefinition / TriggerDefinition carriers -------------------
        Effect::Token {
            static_abilities, ..
        }
        | Effect::GenericEffect {
            static_abilities, ..
        } => {
            for sd in static_abilities {
                stamp_static_printed_slot(sd, slot, kind);
            }
        }
        Effect::CreateEmblem {
            statics, triggers, ..
        } => {
            for sd in statics {
                stamp_static_printed_slot(sd, slot, kind);
            }
            for td in triggers {
                stamp_trigger_printed_slot(td, slot, kind);
            }
        }
        // ---- Nested AbilityDefinition payloads (mirror collect_effects) ------
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            for d in per_choice_effect {
                stamp_retained_printed_slot(d, slot, kind);
            }
            if let VoteSubject::Objects {
                outcome_template, ..
            } = subject
            {
                stamp_retained_printed_slot(outcome_template, slot, kind);
            }
        }
        Effect::SeparateIntoPiles {
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            stamp_retained_printed_slot(chosen_pile_effect, slot, kind);
            if let Some(unchosen) = unchosen_pile_effect {
                stamp_retained_printed_slot(unchosen, slot, kind);
            }
        }
        Effect::RevealFromHand { on_decline, .. } => {
            if let Some(d) = on_decline {
                stamp_retained_printed_slot(d, slot, kind);
            }
        }
        Effect::CreateDelayedTrigger { effect, .. } => {
            stamp_retained_printed_slot(effect, slot, kind)
        }
        Effect::RollDie { results, .. } => {
            for branch in results {
                stamp_retained_printed_slot(&mut branch.effect, slot, kind);
            }
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            ..
        }
        | Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => {
            if let Some(d) = win_effect {
                stamp_retained_printed_slot(d, slot, kind);
            }
            if let Some(d) = lose_effect {
                stamp_retained_printed_slot(d, slot, kind);
            }
        }
        Effect::FlipCoinUntilLose { win_effect } => {
            stamp_retained_printed_slot(win_effect, slot, kind)
        }
        Effect::ChooseOneOf { branches, .. } => {
            for d in branches {
                stamp_retained_printed_slot(d, slot, kind);
            }
        }
        // ---- Leaf variants: no printed-slot self-reference -------------------
        Effect::StartYourEngines { .. } => {}
        Effect::ChangeSpeed { .. } => {}
        Effect::DealDamage { .. } => {}
        Effect::ApplyPostReplacementDamage { .. } => {}
        Effect::EachDealsDamageEqualToPower { .. } => {}
        Effect::EachSourceDealsDamage { .. } => {}
        Effect::Draw { .. } => {}
        Effect::Pump { .. } => {}
        Effect::PairWith { .. } => {}
        Effect::Destroy { .. } => {}
        Effect::Regenerate { .. } => {}
        Effect::RemoveAllDamage { .. } => {}
        Effect::Counter { .. } => {}
        Effect::CounterAll { .. } => {}
        Effect::GainLife { .. } => {}
        Effect::LoseLife { .. } => {}
        Effect::SetTapState { .. } => {}
        Effect::RemoveCounter { .. } => {}
        Effect::Sacrifice { .. } => {}
        Effect::DiscardCard { .. } => {}
        Effect::Mill { .. } => {}
        Effect::Scry { .. } => {}
        Effect::PumpAll { .. } => {}
        Effect::DamageAll { .. } => {}
        Effect::DamageEachPlayer { .. } => {}
        Effect::DestroyAll { .. } => {}
        Effect::ChangeZone { .. } => {}
        Effect::ChangeZoneAll { .. } => {}
        Effect::Dig { .. } => {}
        Effect::GainControl { .. } => {}
        Effect::GainControlAll { .. } => {}
        Effect::ControlNextTurn { .. } => {}
        Effect::Attach { .. } => {}
        Effect::UnattachAll { .. } => {}
        Effect::Surveil { .. } => {}
        Effect::Fight { .. } => {}
        Effect::Bounce { .. } => {}
        Effect::BounceAll { .. } => {}
        Effect::Explore => {}
        Effect::ExploreAll { .. } => {}
        Effect::Investigate => {}
        Effect::Tribute { .. } => {}
        Effect::TimeTravel => {}
        Effect::BecomeMonarch => {}
        Effect::NoOp => {}
        Effect::Proliferate => {}
        Effect::ProliferateTarget { .. } => {}
        Effect::Populate => {}
        Effect::Clash => {}
        Effect::Behold { .. } => {}
        Effect::EndTheTurn => {}
        Effect::EndCombatPhase => {}
        Effect::SwitchPT { .. } => {}
        Effect::EpicCopy { .. } => {}
        Effect::CastCopyOfCard { .. } => {}
        Effect::CreateTokenCopyFromPool { .. } => {}
        Effect::Myriad => {}
        Effect::Encore => {}
        Effect::CombineHost { .. } => {}
        Effect::ChooseAugmentAndCombineWithHost { .. } => {}
        Effect::Meld { .. } => {}
        Effect::ExileHaunting { .. } => {}
        Effect::HideawayConceal { .. } => {}
        Effect::CopyTokenBlockingAttacker { .. } => {}
        Effect::GainActivatedAbilitiesOfTarget { .. } => {}
        Effect::ChooseCard { .. } => {}
        Effect::PutCounter { .. } => {}
        Effect::ChooseCounterKind { .. } => {}
        Effect::PutChosenCounter { .. } => {}
        Effect::PutCounterAll { .. } => {}
        Effect::MultiplyCounter { .. } => {}
        Effect::ChooseCounterAdjustment { .. } => {}
        Effect::DoublePT { .. } => {}
        Effect::DoublePTAll { .. } => {}
        Effect::MoveCounters { .. } => {}
        Effect::Animate { .. } => {}
        Effect::RegisterBending { .. } => {}
        Effect::Cleanup { .. } => {}
        Effect::Mana { .. } => {}
        Effect::Discard { .. } => {}
        Effect::Shuffle { .. } => {}
        Effect::Transform { .. } => {}
        Effect::SearchLibrary { .. } => {}
        Effect::SearchOutsideGame { .. } => {}
        Effect::RevealHand { .. } => {}
        Effect::Reveal { .. } => {}
        Effect::RevealTop { .. } => {}
        Effect::ExileTop { .. } => {}
        Effect::TargetOnly { .. } => {}
        Effect::Choose { .. } => {}
        Effect::OpponentGuess { .. } => {}
        Effect::SwapChosenLabels { .. } => {}
        Effect::ChooseDamageSource { .. } => {}
        Effect::Suspect { .. } => {}
        Effect::Unsuspect { .. } => {}
        Effect::Connive { .. } => {}
        Effect::PhaseOut { .. } => {}
        Effect::PhaseIn { .. } => {}
        Effect::ForceBlock { .. } => {}
        Effect::ForceAttack { .. } => {}
        Effect::SolveCase => {}
        Effect::BecomePrepared { .. } => {}
        Effect::BecomeUnprepared { .. } => {}
        Effect::BecomeSaddled { .. } => {}
        Effect::SetClassLevel { .. } => {}
        // Nested-definition boundary — deliberately NOT recursed (see the grouped
        // note on `CreateDrawReplacement`/`CreatePlaneswalkReplacement` below).
        // `AddTargetReplacement.replacement: Box<ReplacementDefinition>` is built
        // structurally here (oracle_effect/mod.rs:1697 hand-constructs a fixed
        // `ChangeZone` `ReplacementDefinition`), never from a copy-except body, so
        // no `RetainPrinted*FromSource` can appear inside it.
        Effect::AddTargetReplacement { .. } => {}
        Effect::AddRestriction { .. } => {}
        Effect::ReduceNextSpellCost { .. } => {}
        Effect::GrantNextSpellAbility { .. } => {}
        Effect::AddPendingETBCounters { .. } => {}
        Effect::PayCost { .. } => {}
        Effect::CastFromZone { .. } => {}
        Effect::FreeCastFromZones { .. } => {}
        Effect::ExileResolvingSpellInsteadOfGraveyard { .. } => {}
        Effect::PreventDamage { .. } => {}
        Effect::CreateDamageReplacement { .. } => {}
        // Nested-definition boundary — intentionally NOT recursed. Both carry a
        // nested substitute (`replacement_effect: Box<Effect>`) parsed by
        // `parse_effect` (oracle_replacement.rs:5464 / :5520), which threads a bare
        // `ParseContext::default()` (oracle_effect/mod.rs:564) — an index-less ctx.
        // `parse_has_this_ability` reads `ctx.current_{trigger,ability}_index`, both
        // `None` there, so it declines and no `RetainPrinted*FromSource` can be
        // produced past this boundary; the `=> {}` is correct, not a missed carrier.
        //
        // The asymmetry is deliberate: top-level mod-vec carriers above are
        // over-visited defensively (the stamp is a no-op on non-retain mods), but a
        // nested-definition boundary resets the parse context and severs producer
        // propagation, so recursion would be inert. If a future change ever threads
        // a live trigger/ability index past a nested-definition boundary into a
        // copy-except-capable substitute, recurse here.
        Effect::CreateDrawReplacement { .. } => {}
        Effect::CreatePlaneswalkReplacement { .. } => {}
        Effect::LoseTheGame { .. } => {}
        Effect::WinTheGame { .. } => {}
        Effect::RingTemptsYou => {}
        Effect::VentureIntoDungeon => {}
        Effect::VentureInto { .. } => {}
        Effect::TakeTheInitiative => {}
        Effect::ArrangePlanarDeckTop { .. } => {}
        Effect::Planeswalk => {}
        Effect::ChaosEnsues => {}
        Effect::ReverseTurnOrder => {}
        Effect::RedistributeLifeTotals => {}
        Effect::OpenAttractions { .. } => {}
        Effect::RollToVisitAttractions => {}
        Effect::AssembleContraptions { .. } => {}
        Effect::AssembleContraptionsFromRollDifference => {}
        Effect::CrankContraptions { .. } => {}
        Effect::ReassembleContraption { .. } => {}
        Effect::AssembleContraptionOnSprocket { .. } => {}
        Effect::ReassembleContraptionOnSprocket { .. } => {}
        Effect::PutSticker { .. } => {}
        Effect::ApplySticker { .. } => {}
        Effect::ProcessRadCounters => {}
        Effect::ChooseFromZone { .. } => {}
        Effect::RememberCard { .. } => {}
        Effect::ForEachCategory { .. } => {}
        Effect::ChooseObjectsIntoTrackedSet { .. } => {}
        Effect::ChooseAndSacrificeRest { .. } => {}
        Effect::Exploit { .. } => {}
        Effect::GainEnergy { .. } => {}
        Effect::GivePlayerCounter { .. } => {}
        Effect::LoseAllPlayerCounters { .. } => {}
        Effect::ExileFromTopUntil { .. } => {}
        Effect::RevealUntil { .. } => {}
        Effect::Discover { .. } => {}
        Effect::Heist { .. } => {}
        Effect::HeistExile => {}
        Effect::Cascade => {}
        Effect::Ripple { .. } => {}
        Effect::MiracleCast { .. } => {}
        Effect::MadnessCast { .. } => {}
        Effect::PutAtLibraryPosition { .. } => {}
        Effect::ChooseDrawnThisTurnPayOrTopdeck { .. } => {}
        Effect::PutOnTopOrBottom { .. } => {}
        Effect::GiftDelivery { .. } => {}
        Effect::Goad { .. } => {}
        Effect::GoadAll { .. } => {}
        Effect::Detain { .. } => {}
        Effect::SetRoomDoorLock { .. } => {}
        Effect::ExchangeControl { .. } => {}
        Effect::ChangeTargets { .. } => {}
        Effect::Manifest { .. } => {}
        Effect::ManifestDread => {}
        Effect::Cloak { .. } => {}
        Effect::TurnFaceUp { .. } => {}
        Effect::TurnFaceDown { .. } => {}
        Effect::ExtraTurn { .. } => {}
        Effect::GrantExtraLoyaltyActivations { .. } => {}
        Effect::SkipNextTurn { .. } => {}
        Effect::SkipNextStep { .. } => {}
        Effect::AdditionalPhase { .. } => {}
        Effect::Double { .. } => {}
        Effect::RuntimeHandled { .. } => {}
        Effect::Incubate { .. } => {}
        Effect::Amass { .. } => {}
        Effect::Monstrosity { .. } => {}
        Effect::Specialize => {}
        Effect::Renown { .. } => {}
        Effect::Bolster { .. } => {}
        Effect::Adapt { .. } => {}
        Effect::Learn => {}
        Effect::Forage => {}
        Effect::Harness => {}
        Effect::CollectEvidence { .. } => {}
        Effect::Endure { .. } => {}
        Effect::BlightEffect { .. } => {}
        Effect::Seek { .. } => {}
        Effect::SetLifeTotal { .. } => {}
        Effect::ExchangeLifeWithStat { .. } => {}
        Effect::ExchangeLifeTotals { .. } => {}
        Effect::SetDayNight { .. } => {}
        Effect::GiveControl { .. } => {}
        Effect::RemoveFromCombat { .. } => {}
        Effect::BecomeBlocked { .. } => {}
        Effect::Conjure { .. } => {}
        Effect::ApplyPerpetual { .. } => {}
        Effect::Intensify { .. } => {}
        Effect::DraftFromSpellbook { .. } => {}
        Effect::Unimplemented { .. } => {}
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

    /// DISCRIMINATING. Two exact, NON-OVERLAPPING clauses on one physical line,
    /// both at `ordinal_within_span: 0`.
    ///
    /// `conflicts_with` accepts them — it only rejects siblings whose bytes
    /// overlap. Against the old `(first_line, ordinal_within_span)` key they then
    /// collided on `DuplicateItemPosition`, and the second clause was lost. With
    /// `start_byte` in the key they coexist and, crucially, `finish()` returns
    /// them in BYTE order rather than in emission or ordinal order.
    ///
    /// This is the shape unit 3b produces the moment it emits exact spans:
    /// `"Destroy target creature. It can't be regenerated."` is two clauses on
    /// one line.
    #[test]
    fn two_exact_clauses_on_one_line_coexist_and_order_by_byte() {
        let mut b = OracleDocBuilder::new();

        // Emit the LATER clause first, to prove ordering comes from the key and
        // not from emission order.
        let second = b.begin_item(span(0, 24, 48, 0), Some("It can't be regenerated."));
        let second_id = second.id();
        b.emit(second, OracleNodeIr::Keyword(Keyword::Vigilance))
            .unwrap();

        let first = b.begin_item(span(0, 0, 24, 0), Some("Destroy target creature."));
        let first_id = first.id();
        let emitted = b.emit(first, OracleNodeIr::Keyword(Keyword::Flying));
        assert!(
            emitted.is_ok(),
            "non-overlapping same-ordinal clauses on one line must coexist: {emitted:?}"
        );

        let doc = b.finish(
            "Destroy target creature. It can't be regenerated.",
            "Probe",
            vec![],
        );
        let ids: Vec<OracleItemId> = doc.items.iter().map(|i| i.id).collect();
        assert_eq!(
            ids,
            vec![first_id, second_id],
            "items must be ordered by start_byte, not by emission order or ordinal"
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

        // Fragment/precision coupling is enforced on children too. Only ONE
        // direction of that coupling is still REPRESENTABLE: every live tier
        // (`Exact`, `ChainRelative`) returns `carries_fragment() == true`, so
        // "a span that must not carry a fragment, carrying one" — the retired
        // `WholeDocument` case — can no longer be constructed. The guard is not
        // weaker for it; the case moved from a runtime rejection to a type-level
        // impossibility. The surviving direction still discriminates:
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

    /// A span that is NOT card-absolute must say so. `ChainRelative` carries a
    /// truthful byte range — but one measured inside a single effect chain, not
    /// into the card — so a position renderer must be able to refuse to print it
    /// as a card position. This is the discriminating case that keeps `is_exact`
    /// from being tautologically true now that `WholeDocument` is retired.
    #[test]
    fn span_precision_distinguishes_chain_relative_from_exact() {
        let chain_local = OracleSourceSpan::chain_relative(0, 0, 0, 6, 0);
        assert_eq!(chain_local.precision, SpanPrecision::ChainRelative);
        assert!(
            !chain_local.is_exact(),
            "a renderer must be able to refuse to print a chain-local offset as a card position"
        );

        let exact = OracleSourceSpan::exact(1, 1, 7, 20, 0);
        assert_eq!(exact.precision, SpanPrecision::Exact);
        assert!(exact.is_exact());

        // Both tiers locate a real byte range, so both carry their fragment.
        assert!(chain_local.carries_fragment() && exact.carries_fragment());
    }

    /// Two items may legitimately share one printed line — `Kicker {2}{G}` emits a
    /// `Keyword` and an `AdditionalCost` from the same bytes. Distinct ordinals are
    /// what keep those co-located siblings from tripping the overlap rule; the rule
    /// itself must still fire when the ordinal is NOT distinct.
    #[test]
    fn colocated_siblings_conflict_only_on_a_shared_ordinal() {
        let first = OracleSourceSpan::exact(0, 0, 0, 6, 0);
        let second = OracleSourceSpan::exact(0, 0, 0, 6, 1);
        assert!(first.is_contained_by(&second) && second.is_contained_by(&first));
        assert!(
            !first.conflicts_with(&second),
            "distinct ordinals must not conflict even with identical byte ranges"
        );
        assert!(
            first.conflicts_with(&first.clone()),
            "same ordinal + overlapping bytes must still conflict (rule is live)"
        );
    }
}
