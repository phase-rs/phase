//! Effect chain IR types.
//!
//! `EffectChainIr` represents the pre-assembly clause list produced by IR production.
//! `ClauseIr` captures each parsed chunk's effect plus all stripped context (conditions,
//! optionality, continuations, temporal markers). Lowering consumes this flat clause
//! list and performs all assembly operations (continuation patching, condition lifting,
//! delayed-trigger wrapping, sub_ability chain wiring).

use serde::Serialize;

use super::ast::{ClauseBoundary, ContinuationAst, ParsedEffectClause};
use super::doc::{OracleDocBuilder, OracleSourceSpan, OracleUnitSource};
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ControllerRef,
    DelayedTriggerCondition, MultiTargetSpec, OpponentMayScope, PlayerFilter, QuantityExpr,
    RoundingMode, SubAbilityLink, TargetFilter, TargetSelectionMode, UnlessPayModifier,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaExpiry;

/// Chain-level IR: the complete parsed representation of an effect chain before assembly.
///
/// Output of `parse_effect_chain_ir` (Plan 02). Consumed by `lower_effect_chain_ir`
/// to produce an `AbilityDefinition`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct EffectChainIr {
    /// Parsed clauses in source order — each `ClauseIr` captures one parsed
    /// chunk's effect plus all stripped context (conditions, optionality,
    /// continuations, temporal markers). Lowering converts this flat list into
    /// `AbilityDefinition`s via def assembly, continuation patching, and
    /// sub_ability chaining.
    pub(crate) clauses: Vec<ClauseIr>,
    /// The ability kind (Spell, Activated, etc.).
    pub(crate) kind: AbilityKind,
    /// CR 107.1a: Chain-level rounding annotation ("Round down/up each time").
    pub(crate) chain_rounding: Option<RoundingMode>,
    /// CR 701.21a: Actor context threaded from ParseContext (per D-07).
    pub(crate) actor: Option<ControllerRef>,
    /// CR 608.2c + CR 107.1c: chain-level "repeat this process" loop predicate.
    /// Set when a trailing "you may repeat this process" / "if you do, repeat
    /// this process" directive is recognized. Lowering applies it to the root
    /// `AbilityDefinition` so the resolver re-follows the whole chain.
    pub(crate) repeat_until: Option<crate::types::ability::RepeatContinuation>,
}

/// Root-level `AbilityDefinition` metadata that no `ClauseIr` can express.
///
/// The shell is the typed replacement for the `AbilityDefinition` escape hatch:
/// a whole-body recognizer that must stamp a root field returns an `AbilityIr`
/// carrying that field here, rather than a hand-built definition.
///
/// **Scope is measured, not guessed.** Auditing the `return` expressions of the
/// nine effect-side bypasses (not a field-name grep — a field set on a *nested*
/// sub-ability reads identically to one set on the returned root) shows they set
/// exactly `kind`, `effect`, `sub_ability`, `duration`, `player_scope`, and
/// `sub_link`. The first five are already `ParsedEffectClause`/`ClauseIr` fields.
/// `sub_link` is the sole residue, so it is the sole shell field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub(crate) struct AbilityShellIr {
    /// CR 608.2c: how the lowered root attaches to its parent — a resolution step
    /// of the parent's instruction, or an independent following instruction that
    /// resolves even when an optional parent is declined.
    ///
    /// `None` = keep whatever `lower_effect_chain_ir` stamped. `Some(_)` overrides
    /// it, which is required because the root clause has no *previous* boundary:
    /// `lower.rs` derives `sub_link` from `prev_boundary`, and `None` maps
    /// unconditionally to `ContinuationStep`. A recognizer whose root is three
    /// independent steps (`try_parse_balance_equalization`) therefore cannot say so
    /// through the chain, only through the shell.
    ///
    /// `Option<SubAbilityLink>` rather than a bare `SubAbilityLink`: the latter's
    /// `Default` is `ContinuationStep`, so a defaulted shell would silently
    /// *overwrite* the lowered stamp instead of deferring to it.
    pub(crate) sub_link: Option<SubAbilityLink>,
}

/// An effect chain plus the root-level metadata applied around it.
///
/// Lowered by `lower_ability_ir`, which is the single authority for
/// "lower the chain, then finalize it, then anchor it, then apply the shell".
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AbilityIr {
    /// The verbatim text this ability was parsed from.
    ///
    /// **Not an `OracleUnitSource`, on purpose.** `OracleUnitSource`'s fields are
    /// private and its only constructor is `UnitAllocator::allocate_with_span`,
    /// which requires a containing item span. That allocator is not yet threaded
    /// through `ParseContext`, and the entry points that build an `AbilityIr`
    /// (`parse_effect_chain`, `parse_effect_chain_with_context`, and die-result
    /// branch bodies) receive a bare fragment with no line/byte offsets into the
    /// card. Minting a span here would mean fabricating precision — the exact
    /// failure `SpanPrecision` exists to prevent. This becomes an
    /// `OracleUnitSource` in the unit that threads the allocator, not before.
    ///
    /// Read by `apply_owner_library_reveal_anchor_from_text`, which is text-driven.
    pub(crate) source_text: String,
    pub(crate) body: EffectChainIr,
    pub(crate) shell: AbilityShellIr,
}

/// CR 608.2c + CR 601.2c: Subject of a "does the same / does so" effect-replication
/// directive. Such a clause replicates the immediately-preceding sibling effect for
/// a different actor. Typed (never a `bool`/`String`) so the deferred player-set
/// fanout — "each opponent … does the same" (the Curse cycle, Warp World / Morphic
/// Tide) — slots in as a clean enum extension rather than a re-architecture.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum DoesTheSameSubject {
    /// CR 115.1a + CR 601.2c: "[then] target opponent does the same / does so." —
    /// replicate the preceding action for a single targeted opponent (The Wedding
    /// of River Song). The opponent is a cast-time target (CR 601.2c); at
    /// resolution they perform the same action on their own objects (CR 608.2d).
    TargetOpponent,
}

// ===========================================================================
// Typed clause provenance (Plan 01 §5) — Unit 5, milestone M1
// ===========================================================================
//
// A clause carries a stable chain-local `ClauseId`, an honest chain-relative
// `OracleUnitSource`, and exactly one `ClauseDisposition`.
//
// **Antecedent/reference layer JIT-DEFERRED to U6.** Plan 01 §5 also specifies a
// typed antecedent-declaration / reference-consumption vocabulary
// (`AntecedentValue`, `AntecedentSelector`, `ReferenceUse`, `ReferenceProjection`,
// `BindingLifetime`, `ReferenceSurface`). An audit of every field the pre-U5
// `ClauseIr` carried found NONE is an antecedent or a reference — the old
// cross-clause binding is implicit (via `ParseContext` threading + the
// continuation mechanism), so M1's faithful migration has ZERO producers of that
// vocabulary and its only consumer is U6's assembler (not yet built). Landing it
// now would be dead code under `-D warnings` and forcing empty per-site
// declarations would be vacuous. Per "build for the class, not the card" and the
// plan's multi-authority rule (Plan 01 §5, line 413), it is added in U6 where the
// assembler binds references to antecedents by typed id/selector rather than by
// lowered-tree shape search.

/// Chain-local identity for one parsed clause, assigned in source order by the
/// item-scoped [`ClauseIrBuilder`].
///
/// Distinct from the document-global `OracleUnitId`: a `ClauseId` is unique only
/// within one `parse_effect_chain_ir` invocation. Unit 6's assembly arena keys
/// its output nodes by `ClauseId` within a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub(crate) struct ClauseId(pub(crate) u32);

/// The single explicit disposition of a clause: what it does relative to the
/// rest of the chain. Replaces the former ad-hoc `absorbed_by_followup` boolean,
/// `intrinsic_continuation`/`followup_continuation` options, `is_otherwise`
/// boolean, and the `special` marker enum (fully decomposed into typed
/// dispositions by U5-M2; the marker enum no longer exists).
///
/// A continuation clause STAYS in the IR with its own id/source even when it
/// emits no independent definition (Plan 01 §5). The explicit antecedent SELECTOR
/// a `Continue` binds to is JIT-deferred to U6 (see the module note above); in M1
/// the bound antecedent is the prior emitted def, exactly as the pre-U5 lowering
/// applied it.
///
/// The three arms are the top-level XOR discriminant of the pre-U5 lower.rs loop
/// (`if absorbed_by_followup … else if special … else …`, lower.rs:1314/1321).
/// The two continuation channels ride ORTHOGONALLY on the arms — they are applied
/// in multiple paths — so each arm carries the channels its path actually uses:
/// - normal/`Emit`: `followup` patches PRIOR defs (lower.rs:1703), then the def is
///   emitted, then `intrinsic` patches SELF (lower.rs:2078).
/// - absorbed/`Continue`: `continuation` patches PRIOR defs; no self def is
///   emitted (lower.rs:1314).
/// - `FoldSearchIntoElse`: applies `intrinsic` to the def it builds, inline at its
///   own tail (the former `special` path's only intrinsic carrier).
// Intentional: variants carry parser IR directly (the `Emit` channels hold two
// `ContinuationAst` options). Mirrors `oracle_ir::doc.rs`. This IR enum is
// short-lived per-clause and Vec-allocated, so the size gap is acceptable.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ClauseDisposition {
    /// CR 608.2c: this clause emits its own definition(s). `followup` is a
    /// continuation from THIS chunk that patches the PRIOR defs before this clause
    /// emits (formerly `followup_continuation` on the non-absorbed path,
    /// lower.rs:1703); `intrinsic` patches this clause's OWN lowered def after it
    /// emits (formerly `intrinsic_continuation`, lower.rs:2078).
    Emit {
        followup: Option<ContinuationAst>,
        intrinsic: Option<ContinuationAst>,
    },
    /// CR 608.2c: this clause continues/patches the prior emitted clause rather
    /// than emitting an independent def. Folds the former `absorbed_by_followup`
    /// and `followup_continuation` pair (absorbed path, lower.rs:1314). The clause
    /// remains addressable (its own id/source) even though it produces no sibling
    /// def. The explicit antecedent selector is JIT-deferred to U6 (module note);
    /// in M1 the target is the prior emitted def, as the pre-U5 lowering applied it.
    ///
    /// `continuation` is `Option` because the absorbed-but-inert state
    /// (`absorbed_by_followup: true, followup_continuation: None`) is reachable:
    /// the foretell-cost-override suppression clears the continuation while the
    /// clause stays absorbed (must NOT fall back to emitting its parsed effect).
    /// `None` = absorbed no-op.
    Continue {
        continuation: Option<ContinuationAst>,
    },
    /// CR 608.2c: this clause's def attaches as a sub_ability RIDER on the tail of
    /// the prior emitted def's sub_ability chain, emitting no sibling def. Promoted
    /// from the former special-clause markers `DieExileRider` / `CantBeRegeneratedRider`
    /// (U5-M2). `kind` preserves the distinct rules concept (they share the
    /// `append_to_deepest_sub_ability` mechanic — Plan 01 §5 line 811). The bound
    /// antecedent is the prior emitted def (implicit, as M1's `Continue`); the
    /// explicit antecedent selector is JIT-deferred to U6.
    Absorb {
        rider: Box<AbilityDefinition>,
        kind: AbsorbKind,
    },
    /// CR 608.2c: an "Otherwise, [effect]" else-branch. Promoted from
    /// the former special-clause markers `Otherwise` / `OtherwiseFallback` (U5-M2).
    /// `kind` carries the
    /// PARSE-TIME determination of whether a prior conditional exists — do NOT
    /// recompute it at lowering (parse-time and lower-time "prior conditional
    /// present?" states could diverge and move output).
    BranchOtherwise {
        else_def: Box<AbilityDefinition>,
        kind: OtherwiseKind,
    },
    /// CR 608.2c / CR 702: replicate an antecedent template clause once per listed
    /// keyword, swapping the keyword in both the granted ability/counter and its
    /// gating condition. Promoted from the former special-clause markers
    /// `SameIsTrueFor` / `RepeatProcessForKeywords` (U5-M2). `kind` selects the
    /// replication helper;
    /// the bound antecedent is the prior emitted clause (implicit, as `Continue`).
    ReplicatePerKeyword {
        keywords: Vec<Keyword>,
        kind: ReplicateKind,
    },
    /// CR 608.2c: fold a `PriorModifier` onto the prior emitted def; emits no
    /// sibling. Promoted from the three former rider special-clause markers (U5-M2). The
    /// bound antecedent is the prior emitted def (implicit, as `Continue`).
    ModifyPrior { modifier: PriorModifier },
    /// CR 608.2c / CR 614.1a: this clause replaces or overrides the meaning of the
    /// prior emitted def(s) rather than emitting an independent sibling. Promoted
    /// from the former special-clause markers `DigInsteadAlt` / `InsteadClause` /
    /// `KeywordInsteadOverride` (U5-M2). `kind` carries each variant's payload and
    /// keeps the distinct rules
    /// concept typed (Plan 01 §5 line 811). Bound antecedent is the prior emitted
    /// def(s) (implicit, as `Continue`).
    ReplaceMeaning { kind: ReplaceMeaningKind },
    /// CR 608.2c + CR 601.2b: an "if <additional cost was paid>, instead search …"
    /// clause — later text that modifies the meaning of earlier text (CR 608.2c),
    /// gated on an additional cost announced at cast (CR 601.2b). Build this clause's
    /// def, fold the PRIOR `SearchLibrary`'s trailing search-destination `ChangeZone`
    /// into this def's `else_ability`, then apply this clause's own intrinsic
    /// continuation. Promoted from the former special-clause marker
    /// `AdditionalCostInsteadSearch` (U5-M2).
    ///
    /// NOTE: the deleted marker's doc cited CR 608.2e; that rule is APNAP ordering for
    /// multi-player multi-step actions and does not describe this fold. Re-derived to
    /// CR 608.2c, which names this exact shape ("later text … may modify the meaning of
    /// earlier text").
    ///
    /// The sole intrinsic-carrying disposition besides `Emit`: the second
    /// `SearchLibrary` of an "additional cost … instead, search your library" chain
    /// needs its OWN `SearchDestination` self-patch, which the handler applies inline
    /// at its tail. It is also read by the parse-time `previous_is_search_with_hand_dest`
    /// guard (`oracle_effect/mod.rs`), so the `intrinsic()` accessor must expose it.
    FoldSearchIntoElse { intrinsic: Option<ContinuationAst> },
    /// CR 608.2c: follow-up to a drawn-this-turn choice ("For each of those cards,
    /// pay N life or put the card on top of your library") — later text that
    /// parameterizes earlier text. Sets the life payment on the prior
    /// `ChooseDrawnThisTurnPayOrTopdeck` effect and confirms the topdeck branch,
    /// emitting no separate def. Promoted from the former special-clause marker
    /// `DrawnThisTurnPayOrTopdeck` (U5-M2). The bound antecedent is the prior
    /// emitted def (implicit, as `Continue`).
    DrawnThisTurnFollowup { life_payment: QuantityExpr },
}

/// The distinct sub_ability-rider concepts that fold onto the prior emitted def.
/// Both share the `append_to_deepest_sub_ability` mechanic; `kind` keeps the CR
/// concept typed rather than collapsing it (Plan 01 §5 line 811).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum AbsorbKind {
    /// CR 614.1a + CR 514.2: die-exile rider (attach as sub_ability tail).
    DieExile,
    /// CR 608.2c + CR 701.19c: "dealt damage this way can't be regenerated" rider.
    CantBeRegenerated,
}

/// CR 608.2c: a field-level modification folded onto the prior emitted def
/// (emits no sibling). Promoted from the former special-clause markers
/// `AltCostRider` / `ManaRetention` / `EntersTappedAttacking` (U5-M2). Each
/// variant is a distinct rules concept that
/// modifies a different field/aspect of the prior def.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum PriorModifier {
    /// CR 118.9 + CR 119.4: fold an alternative cost onto the prior CastFromZone.
    AltCost(AbilityCost),
    /// CR 106.4: fold a mana-retention expiry onto the prior Mana effect.
    ManaRetention(ManaExpiry),
    /// CR 508.4 / CR 614.1: mark the prior token/copy/zone-change to enter tapped
    /// and attacking (conditional modifier; carries the gate on the clause's
    /// `condition`, with the unpatched original stashed in `else_ability`).
    EntersTappedAttacking,
}

/// CR 608.2c / CR 614.1a: which meaning-replacement the clause performs on the
/// prior emitted def(s). Each variant carries its own payload and rules concept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ReplaceMeaningKind {
    /// CR 608.2c: pop the prior def; wrap this alternative def with the prior as its
    /// `else_ability` (dig-instead alternative).
    DigAlt(Box<AbilityDefinition>),
    /// CR 614.1a + CR 608.2c: multi-clause base + "instead" override via Cow-swap;
    /// tail clauses stashed in the override's `else_ability`.
    Instead(Box<AbilityDefinition>),
    /// CR 608.2c: build this clause's def from `parsed` + condition, attach as the
    /// prior def's `sub_ability` (keyword-instead override).
    KeywordOverride,
}

/// CR 608.2c: whether the "Otherwise" else-branch binds to a prior conditional or
/// self-emits. The determination is made at PARSE time (whether a prior
/// conditional / opponent-may head was found) and carried here — never recomputed
/// at lowering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum OtherwiseKind {
    /// A prior conditional def (or opponent-may head) was found at parse time:
    /// attach the else-branch as its `else_ability` / synthesized reward.
    Bound,
    /// No prior conditional at parse time: self-emit (an Unimplemented "otherwise"
    /// marker def followed by the else def).
    Fallback,
}

/// CR 608.2c / CR 702: which per-keyword replication is performed. Both replicate
/// an antecedent template per listed keyword; `kind` selects which template shape
/// (static grant vs. counter placement) and thus which lowering helper runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum ReplicateKind {
    /// CR 702: "The same is true for <keywords>." — replicate the antecedent static
    /// keyword-GRANT clause per keyword (Odric, Lunarch Marshal).
    StaticGrant,
    /// CR 608.2c: "Repeat this process for <keywords>." — replicate the antecedent
    /// conditional keyword-COUNTER clause per keyword (Kathril, Aspect Warper).
    CounterPlacement,
}

/// Per-clause IR: captures everything about a single parsed chunk before chain assembly.
///
/// Each field corresponds to a local variable extracted during the chunk loop's
/// "strip cascade" in `parse_effect_chain_ir`. All assembly logic (continuation
/// patching, condition lifting, sub_ability wiring) is deferred to lowering.
///
/// **Construction is sealed to [`ClauseIrBuilder`].** The private `_sealed`
/// field makes a struct literal outside this module a compile error, so a clause
/// cannot exist without a `ClauseId`, an `OracleUnitSource`, and an explicit
/// `ClauseDisposition` — the construction gate's teeth (Plan 01 §5, line 343).
/// (The typed antecedent/reference declarations of Plan 01 §5 are JIT-deferred to
/// U6; see the module note above.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ClauseIr {
    /// The parsed effect clause (effect, duration, sub_ability from parse_effect_clause).
    /// Chain-local identity, assigned in source order by [`ClauseIrBuilder`].
    pub(crate) id: ClauseId,
    /// Honest chain-relative source (`SpanPrecision::ChainRelative`): exact byte
    /// range within this chain + verbatim fragment. Replaces the former
    /// unaddressed `source_text` string (Plan 01 §5, line 341). Upgrades to a
    /// card-absolute `OracleUnitSource` in the allocator-threading unit; the
    /// verbatim fragment is retained so that upgrade can re-locate it.
    pub(crate) source: OracleUnitSource,
    /// The one explicit disposition of this clause (Plan 01 §5). Folds the former
    /// `absorbed_by_followup`, `followup_continuation`, `intrinsic_continuation`,
    /// `is_otherwise`, and `special` fields. The typed antecedent/reference
    /// declarations of Plan 01 §5 are JIT-deferred to U6 (see the module note).
    pub(crate) disposition: ClauseDisposition,
    pub(crate) parsed: ParsedEffectClause,
    /// Clause boundary from split_clause_sequence.
    pub(crate) boundary: Option<ClauseBoundary>,
    /// CR 608.2c: Leading or suffix conditional guard.
    pub(crate) condition: Option<AbilityCondition>,
    /// CR 608.2d: "You may" optional effect.
    pub(crate) is_optional: bool,
    /// CR 608.2d: Opponent-may scope.
    pub(crate) opponent_may_scope: Option<OpponentMayScope>,
    /// CR 608.2c: "for each" / "N times" repeat quantity.
    pub(crate) repeat_for: Option<QuantityExpr>,
    /// Player scope iteration ("each opponent", "each player").
    pub(crate) player_scope: Option<PlayerFilter>,
    /// CR 101.4 + CR 800.4: Turn-order override for `player_scope` iteration.
    /// `None` (default) = use APNAP starting from the active player.
    /// `Some(ControllerRef::You)` = start with the controller (Join Forces
    /// "Starting with you, each player may pay any amount of mana").
    /// Stamped onto the produced `AbilityDefinition` during lowering.
    pub(crate) starting_with: Option<ControllerRef>,
    /// CR 603.7: Temporal suffix delayed trigger condition.
    pub(crate) delayed_condition: Option<DelayedTriggerCondition>,
    /// CR 603.7a: Temporal prefix delayed trigger condition.
    pub(crate) prefix_delayed_condition: Option<DelayedTriggerCondition>,
    /// CR 115.1d: Multi-target spec.
    pub(crate) multi_target: Option<MultiTargetSpec>,
    /// CR 107.3i: "where X is <expr>" binding.
    pub(crate) where_x_expression: Option<String>,
    /// CR 118.12: Resolution-time "unless [player] pays" modifier carried by
    /// this clause.
    pub(crate) unless_pay: Option<UnlessPayModifier>,
    /// CR 115.1 + CR 701.9b: Target selection mode captured from `ParseContext`
    /// after this chunk was parsed. Stamped onto the produced `AbilityDefinition`
    /// during lowering. `Chosen` (default) for ordinary "target X" phrases;
    /// `Random` when the parser stripped a leading "random " modifier.
    #[serde(default, skip_serializing_if = "TargetSelectionMode::is_chosen")]
    pub(crate) target_selection_mode: TargetSelectionMode,
    /// CR 601.2c + CR 603.3d: Target chooser captured from `ParseContext` after
    /// this chunk was parsed. Stamped onto the produced `AbilityDefinition` during
    /// lowering. `None` (default) = controller chooses; `Some(ScopedPlayer)` for a
    /// targeted "of their choice" controlled by the phase-trigger active player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) target_chooser: Option<TargetFilter>,
    /// Construction seal: a private field forces all construction through
    /// [`ClauseIrBuilder`], so no call site can mint a clause without identity,
    /// source, disposition, and provenance (Plan 01 §5 construction gate).
    #[serde(skip)]
    _sealed: (),
}

impl ClauseDisposition {
    /// The self-patch continuation parsed from a clause's own text (formerly the
    /// `intrinsic_continuation` field): applied to this clause's own lowered def
    /// after it emits. `Emit`/`FoldSearchIntoElse` carry it; the other dispositions
    /// never do.
    pub(crate) fn intrinsic(&self) -> Option<&ContinuationAst> {
        match self {
            ClauseDisposition::Emit { intrinsic, .. }
            | ClauseDisposition::FoldSearchIntoElse { intrinsic } => intrinsic.as_ref(),
            ClauseDisposition::Continue { .. }
            | ClauseDisposition::Absorb { .. }
            | ClauseDisposition::BranchOtherwise { .. }
            | ClauseDisposition::ReplicatePerKeyword { .. }
            | ClauseDisposition::ModifyPrior { .. }
            | ClauseDisposition::ReplaceMeaning { .. }
            | ClauseDisposition::DrawnThisTurnFollowup { .. } => None,
        }
    }

    /// The prior-patch continuation (formerly the `followup_continuation` field):
    /// a normal (`Emit`) clause's followup that patches the PRIOR def, or a
    /// `Continue` clause's continuation. `None` for every other disposition.
    pub(crate) fn followup(&self) -> Option<&ContinuationAst> {
        match self {
            ClauseDisposition::Emit { followup, .. }
            | ClauseDisposition::Continue {
                continuation: followup,
            } => followup.as_ref(),
            ClauseDisposition::Absorb { .. }
            | ClauseDisposition::BranchOtherwise { .. }
            | ClauseDisposition::ReplicatePerKeyword { .. }
            | ClauseDisposition::ModifyPrior { .. }
            | ClauseDisposition::ReplaceMeaning { .. }
            | ClauseDisposition::FoldSearchIntoElse { .. }
            | ClauseDisposition::DrawnThisTurnFollowup { .. } => None,
        }
    }
}

/// Item-scoped builder that is the single authority for `ClauseIr` construction.
///
/// It owns a LOCAL source-unit allocator seeded over the chain text, because the
/// document allocator is not yet threaded through `ParseContext` (the same wall
/// `AbilityIr` documents). Each clause therefore receives an honest
/// `SpanPrecision::ChainRelative` `OracleUnitSource`: a byte range exact *within
/// this chain* plus its verbatim fragment, upgradeable to card-absolute when the
/// allocator is threaded.
///
/// `ClauseId`s are minted in source order. Construction of a clause is possible
/// only through [`ClauseIrBuilder::clause`], which requires the disposition up
/// front — the construction gate (Plan 01 §5, line 343).
pub(crate) struct ClauseIrBuilder {
    /// The chain-item slot whose `UnitAllocator` mints per-clause child units.
    slot: super::doc::ItemSlot,
    /// The chain text, for monotonic offset resolution of each clause fragment.
    chain_text: String,
    /// Monotonic byte cursor into `chain_text`: each located fragment advances it,
    /// so repeated identical fragments resolve to distinct, source-ordered spans.
    cursor: usize,
    /// Next `ClauseId` to assign (source order within this chain).
    next_clause_id: u32,
    /// Accumulated clauses in source order.
    clauses: Vec<ClauseIr>,
}

impl ClauseIrBuilder {
    /// Create a builder scoped to one `parse_effect_chain_ir` invocation's text.
    pub(crate) fn new(chain_text: &str) -> Self {
        let mut doc = OracleDocBuilder::new();
        let last_line = chain_text.lines().count().saturating_sub(1);
        // The chain item itself is chain-relative: offsets 0..len into its own
        // text, verbatim fragment = the whole chain. Children (clauses) are
        // sub-ranges validated for containment by `allocate_with_span`.
        let span = OracleSourceSpan::chain_relative(0, last_line, 0, chain_text.len(), 0);
        let slot = doc.begin_item(span, Some(chain_text));
        Self {
            slot,
            chain_text: chain_text.to_string(),
            cursor: 0,
            next_clause_id: 0,
            clauses: Vec::new(),
        }
    }

    /// Resolve `fragment`'s honest chain-relative byte span, advancing the cursor.
    ///
    /// A monotonic forward search keeps repeated identical clause fragments
    /// distinct and source-ordered. When the fragment cannot be located — the
    /// chunk text was normalized/derived and no longer appears verbatim in the
    /// chain — it falls back to a zero-width span at the cursor. That is honest,
    /// not fabricated: `ChainRelative` already disclaims card-absolute precision,
    /// and the verbatim fragment is still carried for the later upgrade.
    fn locate(&mut self, fragment: &str) -> OracleSourceSpan {
        let (start, end) = match self.chain_text.get(self.cursor..).and_then(|tail| {
            // allow-noncombinator: byte-offset provenance bookkeeping, not parsing dispatch
            tail.find(fragment)
        }) {
            Some(rel) => {
                let start = self.cursor + rel;
                let end = start + fragment.len();
                self.cursor = end;
                (start, end)
            }
            None => (self.cursor, self.cursor),
        };
        let first_line = self
            .chain_text
            .get(..start)
            .map_or(0, |p| p.matches('\n').count());
        let last_line = self
            .chain_text
            .get(..end)
            .map_or(first_line, |p| p.matches('\n').count());
        // Ordinal within span disambiguates co-located units; each clause is a
        // distinct unit, so the monotonically-increasing clause id doubles as a
        // per-span ordinal that never collides.
        OracleSourceSpan::chain_relative(first_line, last_line, start, end, self.next_clause_id)
    }

    /// Begin a clause. The `disposition` is REQUIRED here (not an optional
    /// setter) so a clause cannot be built without one — the construction gate's
    /// teeth. `source_text` is the verbatim clause fragment that becomes the
    /// `ChainRelative` `OracleUnitSource`. (Typed antecedent/reference
    /// declarations are JIT-deferred to U6; see the module note.)
    pub(crate) fn clause(
        &mut self,
        source_text: &str,
        parsed: ParsedEffectClause,
        boundary: Option<ClauseBoundary>,
        disposition: ClauseDisposition,
    ) -> ClauseDraft<'_> {
        ClauseDraft {
            builder: self,
            source_text: source_text.to_string(),
            parsed,
            boundary,
            disposition,
            condition: None,
            is_optional: false,
            opponent_may_scope: None,
            repeat_for: None,
            player_scope: None,
            starting_with: None,
            delayed_condition: None,
            prefix_delayed_condition: None,
            multi_target: None,
            where_x_expression: None,
            unless_pay: None,
            target_selection_mode: TargetSelectionMode::Chosen,
            target_chooser: None,
        }
    }

    /// Whether any clause has been pushed yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// Read the already-built clauses for mid-chain lookback (prior-referent
    /// checks, condition/opponent-may scans). Returns already-constructed
    /// clauses — it constructs nothing, so the single-construction gate holds.
    pub(crate) fn clauses(&self) -> &[ClauseIr] {
        &self.clauses
    }

    /// Mutate already-built clauses for mid-chain patching (e.g. absorbing a
    /// continuation into a prior clause, suppressing a continuation). Mutates
    /// existing clauses only — constructs nothing, so the gate holds.
    pub(crate) fn clauses_mut(&mut self) -> &mut [ClauseIr] {
        &mut self.clauses
    }

    /// The most recently pushed clause, mutably. `None` before the first push.
    pub(crate) fn last_mut(&mut self) -> Option<&mut ClauseIr> {
        self.clauses.last_mut()
    }

    /// Absorb an already-built clause from a NESTED chain: re-mint a fresh
    /// source-order `ClauseId` + `ChainRelative` span (re-locating its fragment in
    /// THIS chain), preserving all content. Keeps single-construction — still
    /// routes through [`ClauseIrBuilder::clause`] + [`ClauseDraft::push`].
    pub(crate) fn absorb_clause(&mut self, c: ClauseIr) {
        self.clause(
            c.source.fragment().unwrap_or_default(),
            c.parsed,
            c.boundary,
            c.disposition,
        )
        .condition(c.condition)
        .is_optional(c.is_optional)
        .opponent_may_scope(c.opponent_may_scope)
        .repeat_for(c.repeat_for)
        .player_scope(c.player_scope)
        .starting_with(c.starting_with)
        .delayed_condition(c.delayed_condition)
        .prefix_delayed_condition(c.prefix_delayed_condition)
        .multi_target(c.multi_target)
        .where_x_expression(c.where_x_expression)
        .unless_pay(c.unless_pay)
        .target_selection_mode(c.target_selection_mode)
        .target_chooser(c.target_chooser)
        .push();
    }

    /// Consume the builder, yielding the source-ordered clause list.
    pub(crate) fn finish(self) -> Vec<ClauseIr> {
        self.clauses
    }
}

/// A clause under construction: mandatory provenance was supplied to
/// [`ClauseIrBuilder::clause`]; optional local attributes are set by chaining,
/// then [`ClauseDraft::push`] mints identity + source and commits it.
#[must_use = "a ClauseDraft does nothing until `.push()` commits it"]
pub(crate) struct ClauseDraft<'a> {
    builder: &'a mut ClauseIrBuilder,
    source_text: String,
    parsed: ParsedEffectClause,
    boundary: Option<ClauseBoundary>,
    disposition: ClauseDisposition,
    condition: Option<AbilityCondition>,
    is_optional: bool,
    opponent_may_scope: Option<OpponentMayScope>,
    repeat_for: Option<QuantityExpr>,
    player_scope: Option<PlayerFilter>,
    starting_with: Option<ControllerRef>,
    delayed_condition: Option<DelayedTriggerCondition>,
    prefix_delayed_condition: Option<DelayedTriggerCondition>,
    multi_target: Option<MultiTargetSpec>,
    where_x_expression: Option<String>,
    unless_pay: Option<UnlessPayModifier>,
    target_selection_mode: TargetSelectionMode,
    target_chooser: Option<TargetFilter>,
}

impl ClauseDraft<'_> {
    pub(crate) fn condition(mut self, v: Option<AbilityCondition>) -> Self {
        self.condition = v;
        self
    }
    // Consuming builder setter mirroring the `is_optional` field name; the
    // `is_*`-takes-`&self` convention does not apply to a chainable builder.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn is_optional(mut self, v: bool) -> Self {
        self.is_optional = v;
        self
    }
    pub(crate) fn opponent_may_scope(mut self, v: Option<OpponentMayScope>) -> Self {
        self.opponent_may_scope = v;
        self
    }
    pub(crate) fn repeat_for(mut self, v: Option<QuantityExpr>) -> Self {
        self.repeat_for = v;
        self
    }
    pub(crate) fn player_scope(mut self, v: Option<PlayerFilter>) -> Self {
        self.player_scope = v;
        self
    }
    pub(crate) fn starting_with(mut self, v: Option<ControllerRef>) -> Self {
        self.starting_with = v;
        self
    }
    pub(crate) fn delayed_condition(mut self, v: Option<DelayedTriggerCondition>) -> Self {
        self.delayed_condition = v;
        self
    }
    pub(crate) fn prefix_delayed_condition(mut self, v: Option<DelayedTriggerCondition>) -> Self {
        self.prefix_delayed_condition = v;
        self
    }
    pub(crate) fn multi_target(mut self, v: Option<MultiTargetSpec>) -> Self {
        self.multi_target = v;
        self
    }
    pub(crate) fn where_x_expression(mut self, v: Option<String>) -> Self {
        self.where_x_expression = v;
        self
    }
    pub(crate) fn unless_pay(mut self, v: Option<UnlessPayModifier>) -> Self {
        self.unless_pay = v;
        self
    }
    pub(crate) fn target_selection_mode(mut self, v: TargetSelectionMode) -> Self {
        self.target_selection_mode = v;
        self
    }
    pub(crate) fn target_chooser(mut self, v: Option<TargetFilter>) -> Self {
        self.target_chooser = v;
        self
    }

    /// Mint the `ClauseId` + `ChainRelative` `OracleUnitSource` and commit the
    /// clause into the builder's source-ordered list.
    pub(crate) fn push(self) {
        let id = ClauseId(self.builder.next_clause_id);
        let span = self.builder.locate(&self.source_text);
        // `allocate_with_span` validates containment + fragment/precision. A
        // ChainRelative child of the chain item always satisfies both by
        // construction; the fallback zero-width span is contained too.
        let source = self
            .builder
            .slot
            .allocator()
            .allocate_with_span(span, Some(&self.source_text))
            .expect("chain-relative clause span is contained by its chain item");
        self.builder.next_clause_id += 1;
        self.builder.clauses.push(ClauseIr {
            id,
            source,
            disposition: self.disposition,
            parsed: self.parsed,
            boundary: self.boundary,
            condition: self.condition,
            is_optional: self.is_optional,
            opponent_may_scope: self.opponent_may_scope,
            repeat_for: self.repeat_for,
            player_scope: self.player_scope,
            starting_with: self.starting_with,
            delayed_condition: self.delayed_condition,
            prefix_delayed_condition: self.prefix_delayed_condition,
            multi_target: self.multi_target,
            where_x_expression: self.where_x_expression,
            unless_pay: self.unless_pay,
            target_selection_mode: self.target_selection_mode,
            target_chooser: self.target_chooser,
            _sealed: (),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::ast::parsed_clause;
    use crate::types::ability::Effect;

    #[test]
    fn effect_chain_ir_empty_construction() {
        let ir = EffectChainIr {
            clauses: vec![],
            kind: AbilityKind::Spell,
            chain_rounding: None,
            actor: None,
            repeat_until: None,
        };
        assert!(ir.clauses.is_empty());
    }

    #[test]
    fn builder_mints_source_order_ids_and_chain_relative_spans() {
        let chain = "draw a card. draw two cards";
        let mut b = ClauseIrBuilder::new(chain);
        assert!(b.is_empty());
        assert!(b.clauses().last().is_none());
        b.clause(
            "draw a card",
            parsed_clause(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }),
            Some(ClauseBoundary::Sentence),
            ClauseDisposition::Emit {
                followup: None,
                intrinsic: None,
            },
        )
        .push();
        assert_eq!(b.clauses().last().map(|c| c.id), Some(ClauseId(0)));
        b.clause(
            "draw two cards",
            parsed_clause(Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            }),
            None,
            ClauseDisposition::Emit {
                followup: None,
                intrinsic: None,
            },
        )
        .is_optional(true)
        .push();
        let clauses = b.finish();
        assert_eq!(clauses.len(), 2);
        // Source-order ids.
        assert_eq!(clauses[0].id, ClauseId(0));
        assert_eq!(clauses[1].id, ClauseId(1));
        // Verbatim fragment carried; span is chain-relative (not card-absolute).
        assert_eq!(clauses[0].source.fragment(), Some("draw a card"));
        assert!(!clauses[0].source.span().is_exact());
        // Monotonic cursor keeps the second "draw" distinct from the first.
        assert_eq!(clauses[1].source.fragment(), Some("draw two cards"));
        assert!(clauses[1].source.span().start_byte > clauses[0].source.span().start_byte);
        assert!(clauses[1].is_optional);
    }

    #[test]
    fn builder_continue_disposition_stays_in_ir_with_own_id() {
        let chain = "exile the top card. play that card";
        let mut b = ClauseIrBuilder::new(chain);
        b.clause(
            "exile the top card",
            parsed_clause(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            }),
            Some(ClauseBoundary::Sentence),
            ClauseDisposition::Emit {
                followup: None,
                intrinsic: None,
            },
        )
        .push();
        let prior = b.clauses().last().expect("prior clause").id;
        // A continuation clause STAYS in the IR with its own id even though it
        // emits no independent def — the honest replacement for the old
        // `absorbed_by_followup` boolean.
        b.clause(
            "play that card",
            parsed_clause(Effect::NoOp),
            None,
            ClauseDisposition::Continue {
                continuation: Some(ContinuationAst::SearchResultClauseHandled),
            },
        )
        .push();
        let clauses = b.finish();
        assert_eq!(clauses.len(), 2);
        assert_eq!(prior, ClauseId(0));
        assert_eq!(clauses[1].id, ClauseId(1));
        assert!(matches!(
            clauses[1].disposition,
            ClauseDisposition::Continue { .. }
        ));
    }

    #[test]
    fn effect_chain_ir_with_single_clause() {
        let mut b = ClauseIrBuilder::new("draw two cards");
        b.clause(
            "draw two cards",
            parsed_clause(Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            }),
            Some(ClauseBoundary::Sentence),
            ClauseDisposition::Emit {
                followup: None,
                intrinsic: None,
            },
        )
        .push();
        let ir = EffectChainIr {
            clauses: b.finish(),
            kind: AbilityKind::Spell,
            chain_rounding: None,
            actor: None,
            repeat_until: None,
        };
        assert_eq!(ir.clauses.len(), 1);
        assert_eq!(ir.kind, AbilityKind::Spell);
    }
}
