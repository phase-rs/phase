//! Effect chain IR types.
//!
//! `EffectChainIr` represents the pre-assembly clause list produced by IR production.
//! `ClauseIr` captures each parsed chunk's effect plus all stripped context (conditions,
//! optionality, continuations, temporal markers). Lowering consumes this flat clause
//! list and performs all assembly operations (continuation patching, condition lifting,
//! delayed-trigger wrapping, sub_ability chain wiring).

use serde::Serialize;

use super::ast::{with_clause_chain_duration, ClauseBoundary, ContinuationAst, ParsedEffectClause};
use super::doc::{OracleDocBuilder, OracleSourceSpan, OracleUnitSource};
use crate::parser::oracle_effect::lower::strip_trailing_duration;
use crate::parser::oracle_nom::filter::ChosenColorGrantReference;
use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, AbilityTag,
    ActivationManaPaymentRestriction, ActivationRestriction, ChoiceType, ControllerRef,
    CostReduction, DelayedTriggerCondition, Duration, Effect, ManaSpendPermission, MultiTargetSpec,
    OpponentMayScope, PlayerFilter, QuantityExpr, QuantityRef, ReturnResultReadSpec, RoundingMode,
    SubAbilityLink, TargetChoiceTiming, TargetFilter, TargetSelectionMode, UnlessPayModifier,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaExpiry;
use crate::types::zones::Zone;

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
    /// Kind to retain on definitions linked as this chain's continuations.
    ///
    /// Ordinary clause assembly normalizes linked definitions to `Spell`: they
    /// resolve with their parent rather than becoming independently activatable
    /// abilities. Whole-body recognizers that already constructed every link
    /// with the enclosing kind can opt into preserving that serialized shape
    /// while still routing through the shared chain assembly.
    pub(crate) continuation_kind: Option<AbilityKind>,
    /// Whether assembly applies the ordinary player-scope reference rewrites.
    pub(crate) player_scope_rewrite: PlayerScopeRewrite,
    /// CR 107.1a: Chain-level rounding annotation ("Round down/up each time").
    pub(crate) chain_rounding: Option<RoundingMode>,
    /// CR 701.21a: Actor context threaded from ParseContext (per D-07).
    pub(crate) actor: Option<ControllerRef>,
    /// CR 603.2c: Whether this chain is the body of a TRIGGERED ability,
    /// threaded from `ParseContext::in_trigger` (mirrors `actor`).
    ///
    /// Assembly needs it to reject an unbindable batch anaphor: a
    /// `TrackedSetAggregate { source: TriggeringBatch }` names the objects of
    /// the CURRENT TRIGGER EVENT, so in a chain with no trigger event (a spell,
    /// or a loyalty/activated ability) the pronoun has no antecedent and would
    /// silently reduce an empty set to 0. Such a chain must fail honestly
    /// instead. See `assemble_effect_chain`.
    pub(crate) in_trigger: bool,
    /// CR 608.2c + CR 107.1c: chain-level "repeat this process" loop predicate.
    /// Set when a trailing "you may repeat this process" / "if you do, repeat
    /// this process" directive is recognized. Lowering applies it to the root
    /// `AbilityDefinition` so the resolver re-follows the whole chain.
    pub(crate) repeat_until: Option<crate::types::ability::RepeatContinuation>,
    /// CR 607.2d: whether this chain's `Protection`/`HexproofFrom(ChosenColor)`
    /// grant may have a colour choice injected ahead of it, or reads a choice
    /// made by a LINKED ability elsewhere on the same object.
    ///
    /// Stamped from
    /// `DocumentRelationIr::LinkedChoice(LinkedChoiceKind::LinkedColorChoice)`
    /// before lowering, so the injector simply never fires and there is nothing
    /// to undo. `Permitted` is the default, so a chain that no relation names is
    /// byte-identical to before this axis existed.
    #[serde(default, skip_serializing_if = "InjectedColorChoice::is_permitted")]
    pub(crate) injected_color_choice: InjectedColorChoice,
}

/// CR 607.2d: whether an effect chain may receive an injected colour chooser
/// ahead of a `ChosenColor` keyword grant.
///
/// A `bool` is prohibited by the project's data-modelling rule and would not say
/// WHY the injection is withheld; this names the reason, which is what the
/// injector's guard cites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub(crate) enum InjectedColorChoice {
    /// No linked ability supplies a colour for this chain's grant, so the
    /// injector supplies one (Mother of Runes, Knight of Dawn).
    #[default]
    Permitted,
    /// CR 607.2d: a linked ability elsewhere on this object already makes the
    /// choice this chain's grant reads back (Floating Shield's as-enters
    /// replacement), so the grant must NOT make a second choice of its own.
    SuppressedByLinkedAbility,
}

impl InjectedColorChoice {
    /// Serialization guard: the default carries no information, so an untouched
    /// chain serializes exactly as it did before this field existed.
    pub(crate) fn is_permitted(&self) -> bool {
        matches!(self, InjectedColorChoice::Permitted)
    }
}

/// Whether `lower_effect_chain_ir` rewrites player-scoped references after
/// assembling a chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum PlayerScopeRewrite {
    /// Apply the shared player-scope rewrites used by ordinary parsed clauses.
    Apply,
    /// Preserve the recognizer's explicit scoped fields as parsed.
    Preserve,
}

impl EffectChainIr {
    /// Build a one-clause chain while retaining every field represented by
    /// `ParsedEffectClause` plus the clause-level player iteration scope.
    ///
    /// Whole-body recognizers use this instead of reconstructing an
    /// `AbilityDefinition`: `ParsedEffectClause` carries nested sub-abilities and
    /// durations, while `ClauseIr` carries `player_scope`. The actor and trigger
    /// context match the ordinary chain parser's output.
    pub(crate) fn single_clause(
        source_text: &str,
        kind: AbilityKind,
        parsed: ParsedEffectClause,
        player_scope: Option<PlayerFilter>,
        actor: Option<ControllerRef>,
        in_trigger: bool,
    ) -> Self {
        let mut builder = ClauseIrBuilder::new(source_text);
        builder
            .clause(
                source_text,
                parsed,
                None,
                ClauseDisposition::Emit {
                    followup: None,
                    intrinsic: None,
                },
            )
            .player_scope(player_scope)
            .push();
        Self {
            clauses: builder.finish(),
            kind,
            continuation_kind: None,
            player_scope_rewrite: PlayerScopeRewrite::Apply,
            chain_rounding: None,
            actor,
            in_trigger,
            repeat_until: None,
            injected_color_choice: InjectedColorChoice::Permitted,
        }
    }
}

impl AbilityIr {
    /// CR 706.3b: Whether the raw body contains an unassigned die roll that can
    /// own an immediately following results table. This collection gate scans
    /// source-ordered direct clauses and their pre-lowered sequential
    /// sub-ability chains. The P4/P9 roll producers emit ordinary clauses;
    /// duplicating full `ClauseDisposition` assembly here would create a second
    /// reachability authority. Post-assembly attachment remains authoritative.
    pub(crate) fn has_result_table_roll_die(&self) -> bool {
        self.body.clauses.iter().any(|clause| {
            matches!(&clause.parsed.effect, crate::types::ability::Effect::RollDie { results, .. } if results.is_empty())
                || clause
                    .parsed
                    .sub_ability
                    .as_deref()
                    .is_some_and(ability_definition_has_result_table_roll_die)
        })
    }
}

fn ability_definition_has_result_table_roll_die(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), crate::types::ability::Effect::RollDie { results, .. } if results.is_empty())
        || def
            .sub_ability
            .as_deref()
            .is_some_and(ability_definition_has_result_table_roll_die)
}

/// Root-level `AbilityDefinition` metadata that no `ClauseIr` can express.
///
/// The shell is the typed replacement for the `AbilityDefinition` escape hatch:
/// a whole-body recognizer that must stamp a root field returns an `AbilityIr`
/// carrying that field here, rather than a hand-built definition.
///
/// # The partition is the rules' own, not an engineering convenience
///
/// CR 602.1 — *"Activated abilities have a cost and an effect. They are written
/// as `[Cost]: [Effect.] [Activation instructions (if any).]`"* — draws exactly
/// the seam this type sits on, and CR 113.3b repeats the tripartite form for
/// abilities generally. So:
///
/// | shell field group | CR |
/// |---|---|
/// | `cost`, `cost_reduction` | CR 602.1a — everything before the colon |
/// | `activation_restrictions`, `activation_mana_payment_restriction`, `activator_filter`, `activation_zone` | CR 602.1b — activation instructions, *"not part of the ability's effect"* |
/// | `min_x_value` | CR 601.2b — the announced value of a variable cost |
/// | `ability_tag`, `cant_be_copied`, `description` | ability-level identity/provenance, not resolution steps |
///
/// while `EffectChainIr` holds the CR 608.2 resolution instructions. Because
/// the root-vs-clause axis follows a seam CR 602.1 already draws, the widening
/// satisfies the categorical-boundary rule rather than straddling rule
/// sections.
///
/// **This is 12 of `AbilityDefinition`'s 38 root fields, deliberately not a
/// mirror of the root.** (Counted from the source: this struct has thirteen
/// fields, twelve of which mirror a root field; `stages` is a transform list,
/// not a root field. A0's "10" counted the fields that tranche *added* and
/// omitted the pre-existing `sub_link`, so it read one low even before
/// `optional` arrived.) Fields
/// excluded on purpose — `effect`, `sub_ability`,
/// `else_ability`, `condition` — are all CR 608.2 resolution tree and are
/// already expressible as `ClauseIr`/`ClauseDisposition`. A shell that mirrored
/// the root would re-open the escape hatch this type exists to close.
///
/// # Applier semantics (see `lower_ability_ir`)
///
/// Every field is **defer-on-default**: an unset field leaves whatever lowering
/// produced, so a `default()` shell is exactly today's behavior. That is what
/// makes the widening byte-identical by construction — see the per-field docs
/// for the one-line rule each obeys.
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
    /// independent steps (`parse_balance_equalization_ir`) therefore cannot say so
    /// through the chain, only through the shell.
    ///
    /// `Option<SubAbilityLink>` rather than a bare `SubAbilityLink`: the latter's
    /// `Default` is `ContinuationStep`, so a defaulted shell would silently
    /// *overwrite* the lowered stamp instead of deferring to it.
    pub(crate) sub_link: Option<SubAbilityLink>,

    /// CR 602.1a: the activation cost — everything before the colon.
    /// `Some(_)` overrides; `None` defers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cost: Option<AbilityCost>,

    /// CR 601.2f: a cost reduction determined while computing total cost.
    ///
    /// Distinct from [`ShellStage::ExtractCostReduction`], which *derives* this
    /// field by folding a node out of the chain. A site that stamps it
    /// explicitly must NOT also run that stage — see the `ShellStage` docs.
    /// `Some(_)` overrides; `None` defers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cost_reduction: Option<CostReduction>,

    /// CR 602.1b: activation instructions restricting *when* the ability may be
    /// activated.
    ///
    /// **Applied with `extend`, never `=`.** The shell is applied after lowering,
    /// and no path inside `lower_ability_ir` writes the root's
    /// `activation_restrictions` — verified exhaustively: `rg
    /// activation_restrictions crates/engine/src/parser/oracle_effect/` returns
    /// zero hits, and that directory holds the whole of `lower_ability_ir`
    /// (`lower_effect_chain_ir`, `finalize_effect_chain`, the owner-library
    /// anchor, and all five whole-body bypasses `parse_ability_ir` dispatches
    /// to). The only writes reachable from there land on *nested* granted/static
    /// definitions boxed inside an `Effect` payload, never on the root the shell
    /// stamps. So `extend` reproduces a site that wrote `=` while additionally
    /// being correct if lowering ever does contribute one.
    ///
    /// Order is the site's: the vec is applied verbatim, so a site that pushed an
    /// implicit restriction before extending with parsed ones builds its vec in
    /// that order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) activation_restrictions: Vec<ActivationRestriction>,

    /// CR 602.1b: a restriction on which mana may pay the activation cost.
    /// `Some(_)` overrides; `None` defers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) activation_mana_payment_restriction: Option<ActivationManaPaymentRestriction>,

    /// CR 602.1b: which players may activate the ability ("Any player may
    /// activate this ability"). `Some(_)` overrides; `None` defers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) activator_filter: Option<PlayerFilter>,

    /// CR 113.6m: the zone the ability functions in. `Some(_)` overrides;
    /// `None` defers.
    ///
    /// Note for later tranches: the generic activated-ability recognizer derives
    /// this field by *reading the lowered def*
    /// (`activation_zone_from_self_cost` / `activation_zone_from_self_effect`),
    /// which a shell stamped before lowering cannot express. That is one of the
    /// reasons `parse_activated_ability_ir` is scoped to its own unit
    /// rather than to T8 — this field is here for the recognizers that know
    /// their zone from the printed keyword (Channel, Forecast), not for that one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) activation_zone: Option<Zone>,

    /// The keyword/ability-word class this ability was printed under (Boast,
    /// Exhaust, Power-up …), so meta-referencing effects can name the class.
    /// `Some(_)` overrides; `None` defers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ability_tag: Option<AbilityTag>,

    /// CR 601.2b: the floor on an announced variable cost ("X can't be 0").
    ///
    /// `u32`, not `Option<u32>`, because the root field is `u32` with a `0`
    /// default that already means "no floor". Applied with `max`, not `=`: `0`
    /// (the shell default) can then never lower a floor lowering established,
    /// and a site stamping `N` over a lowered `0` still yields `N`. `max` is also
    /// exactly the semantic `raise_last_spell_min_x` will need in T9.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub(crate) min_x_value: u32,

    /// CR 707.10: the stack-copy restriction printed as "This ability can't be
    /// copied" — CR 707.10 is the rule that defines copying an activated or
    /// triggered ability onto the stack, which is what the printed line forbids.
    /// (Not CR 707.9a, which is the unrelated rule for copy effects that cause a
    /// copy to *gain* an ability.)
    ///
    /// Applied as a monotone OR, never an assignment, so a `false` shell (the
    /// default) cannot clear a flag lowering set. Mirrors the root field's own
    /// `bool` rather than inventing a parallel encoding for a two-state fact the
    /// definition already stores as a `bool`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) cant_be_copied: bool,

    /// The verbatim printed text this ability was rendered from, for coverage
    /// and UI provenance. `Some(_)` overrides; `None` defers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) description: Option<String>,

    /// CR 608.2d: the controller chooses whether to perform the effect
    /// ("You may …"). Applied as a monotone OR, never an assignment.
    ///
    /// # This is the one field that is NOT the CR 602.1 activation envelope
    ///
    /// Everything else on this shell partitions along the seam CR 602.1 draws —
    /// cost before the colon, activation instructions after it. `optional` does
    /// not: CR 608.2d places the choice *"while applying the effect"*, which is
    /// CR 608.2 resolution, the half this type deliberately leaves to
    /// [`EffectChainIr`]. So its presence here is an explicit, named exception
    /// rather than an extension of the partition, and this doc block is where the
    /// exception is justified.
    ///
    /// # Why the exception is nonetheless correct
    ///
    /// The mechanical requirement is that `optional` be stamped
    /// **unconditionally, after lowering** — which is exactly what the
    /// game-start recognizers (`AbilityKind::Mulligan`, `BeginGame`) do by hand
    /// today: they call the chain parser, then set `def.optional = true` on the
    /// result. The alternative — expressing the flag on the first clause and
    /// letting assembly carry it to the root — is **not** equivalent:
    /// `assemble_effect_chain` maps a clause's optionality to the root
    /// conditionally, through four suppressions plus a `SearchOutsideGame` arm
    /// that forces `optional = false`. A recognizer whose printed text says
    /// "you may" would silently lose the flag on any input that took one of
    /// those arms. Named, so the claim is checkable: the `clause_ir.parsed
    /// .optional` propagation in `assemble_effect_chain` is suppressed for
    /// `Effect::GrantCastingPermission`, for `is_lingering_cast_from_zone`, for
    /// `is_join_forces_pay_any_amount_mana_cost` and for
    /// `is_pay_to_end_effect_termination`, and a following arm sets
    /// `def.optional = false` outright for `Effect::SearchOutsideGame`. It also
    /// assumes clause 0 becomes the emitted root, which `ClauseDisposition` does
    /// not guarantee. The shell is the only place the stamp can be unconditional
    /// *and* survive the IR conversion, so the field lives here and the
    /// categorical impurity is paid knowingly.
    ///
    /// # Why a monotone OR
    ///
    /// `def.optional |= shell.optional`, mirroring `cant_be_copied`. The `false`
    /// default can then never clear a flag lowering established, so
    /// `AbilityShellIr::default()` stays a no-op and the widening is
    /// byte-identical by construction — A0's defer-on-default property, which is
    /// the whole reason a shell field can be added without touching any existing
    /// producer. An assignment would break it: every unconverted site building a
    /// `default()` shell would begin clearing an `optional` that
    /// `lower_effect_chain_ir` had legitimately set from the printed "you may".
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) optional: bool,

    /// Chain-**structure** folds to run after the field stamps, in list order.
    ///
    /// Ordered `Vec`, not a set of flags: see [`ShellStage`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) stages: Vec<ShellStage>,
}

/// `serde` predicate: a `min_x_value` of `0` is the "no floor" default and is
/// skipped, so an unset shell serializes exactly as it did before the widening.
#[allow(clippy::trivially_copy_pass_by_ref)] // serde requires the `&T` predicate shape.
fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// A chain-**structure** transform `lower_ability_ir` runs after the shell's
/// field stamps, in list order.
///
/// # Why an ordered `Vec` and not a set of booleans
///
/// These stages are folds that rewrite the `sub_ability` chain *and* write a root
/// field, so their position relative to the field stamps is behavior-load-bearing
/// — a plain field bag cannot express "run these two, in this order, after the
/// stamps":
///
/// * [`ShellStage::ExtractCostReduction`] strips a node out of the `sub_ability`
///   chain **and writes** `def.cost_reduction`. It must therefore not run at a
///   site that stamped `cost_reduction` explicitly — the Power-up recognizer
///   (`oracle.rs`, CR 702.193b) does exactly that, and running the stage there
///   would let a chain node silently overwrite the keyword-defined reduction.
/// * [`ShellStage::ExtractManaSpendTrigger`] early-returns unless `def.effect` is
///   already `Effect::Mana`, so it is meaningful only *post*-lowering — it cannot
///   be hoisted into clause assembly.
///
/// Variants are named for the transform, not for a call site, so the list stays
/// a description of *what runs* rather than of *who asked*.
// The extraction variants are constructed by the T8-A2 recognizers (Channel,
// Boast, Exhaust, Forecast), each of which lists them in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ShellStage {
    /// CR 106.6: normalize an activated mana ability's `instead` alternative
    /// into the additional-mana delta used by the existing mana authority.
    /// Runs `oracle::normalize_activated_mana_instead_delta`.
    NormalizeActivatedManaInstead,
    /// CR 601.2f: fold a trailing self-referential "this ability costs {X} less
    /// to activate" node out of the `sub_ability` chain into `cost_reduction`.
    /// Runs `oracle::extract_cost_reduction_from_chain`.
    ExtractCostReduction,
    /// CR 106.6 + CR 603.3: fold a trailing "when you spend this mana …"
    /// sub-ability into the parent mana effect's `grants`. Runs
    /// `oracle::extract_mana_spend_trigger_from_chain`, which is a no-op unless
    /// the lowered root effect is already `Effect::Mana`.
    ExtractManaSpendTrigger,
}

/// CR 706.3a: one row of a die-roll results table — a possible-result range and
/// the effect associated with it ("N1–N2" or "N+").
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DieResultBranchIr {
    pub(crate) min: u8,
    pub(crate) max: u8,
    pub(crate) effect: Box<AbilityIr>,
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
    /// Result-table rows supplied by a whole-body die-roll recognizer.
    ///
    /// Empty is the default for every ordinary ability IR and is a lowering no-op.
    pub(crate) die_results: Vec<DieResultBranchIr>,
    /// Ordered root transforms applied after whole-ability lowering.
    ///
    /// This is intentionally separate from [`AbilityShellIr`]. The shell carries
    /// the activation envelope; these transforms compose post-chain resolution
    /// metadata whose order depends on the root that chain assembly selected.
    /// An empty list is a lowering no-op.
    pub(crate) root_transforms: Vec<AbilityRootTransform>,
    /// Modal metadata attached to this ability root and lowered with it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) modal: Option<ModalPayloadIr>,
}

/// Native modal payload. Its modes retain parser provenance until their
/// ordinary `AbilityIr` lowering runs at the owning root's lowering seam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ModalPayloadIr {
    pub(crate) choice: crate::types::ability::ModalChoice,
    pub(crate) modes: Vec<ModalModeIr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ModalModeIr {
    pub(crate) source_text: String,
    pub(crate) source_line: Option<usize>,
    pub(crate) ability: Box<AbilityIr>,
}

/// A root-level transform applied only after an [`AbilityIr`] has been fully
/// lowered.
///
/// CR 608.2c: chain assembly may change which parsed clause becomes the root,
/// so a whole-ability condition cannot be assigned to the first clause. These
/// transforms operate on the finalized root in their stored order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum AbilityRootTransform {
    /// CR 601.2b: stamp the announced-X floor from this printed ability.
    SetMinXValue(u32),
    /// Preserve the complete printed source text for this multi-line ability.
    SetDescription(String),
    /// CR 614.6 + CR 614.15: replace an unbindable self-replacement's final
    /// lowered root with the explicit honest-failure floor.
    InsteadOverrideResidual {
        fragment: String,
        condition_policy: ResidualConditionPolicy,
    },
    /// CR 608.2c: prepend a condition (ability word) before the chain-derived
    /// root condition.
    PrependCondition(AbilityCondition),
    /// CR 608.2c: append a condition extracted from a line-level `instead`.
    AppendCondition(AbilityCondition),
}

/// Whether an honest unbindable override floor retains the condition the legacy
/// parser had already lowered, or clears it for a partial replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) enum ResidualConditionPolicy {
    Preserve,
    Clear,
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

/// The object class and chooser role named by a selected-return instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ChosenReturnSpec {
    pub(crate) noun: TargetFilter,
    /// `None` requires one unambiguous antecedent of the named class.
    pub(crate) chooser: Option<TargetFilter>,
    pub(crate) destination: Zone,
    /// Only a printed recipient represented by the delayed-reader grammar can bind.
    pub(crate) recipient: Option<ControllerRef>,
}

fn chosen_noun_compatible(producer: &TargetFilter, noun: &TargetFilter) -> bool {
    match (producer, noun) {
        (TargetFilter::Typed(producer), TargetFilter::Typed(noun)) => noun
            .type_filters
            .iter()
            .all(|kind| producer.type_filters.contains(kind)),
        (TargetFilter::And { filters }, noun) => filters
            .iter()
            .any(|filter| chosen_noun_compatible(filter, noun)),
        _ => false,
    }
}

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
/// (`if absorbed_by_followup … else if special … else …`, the pre-U5 lowering pass).
/// The two continuation channels ride ORTHOGONALLY on the arms — they are applied
/// in multiple paths — so each arm carries the channels its path actually uses:
/// - normal/`Emit`: `followup` patches PRIOR defs, then the def is
///   emitted, then `intrinsic` patches SELF (the pre-U5 lowering pass).
/// - absorbed/`Continue`: `continuation` patches PRIOR defs; no self def is
///   emitted (the pre-U5 lowering pass).
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
    /// emits (formerly `followup_continuation` on the non-absorbed path);
    /// `intrinsic` patches this clause's OWN lowered def after it
    /// emits (formerly `intrinsic_continuation`, the pre-U5 lowering pass).
    Emit {
        followup: Option<ContinuationAst>,
        intrinsic: Option<ContinuationAst>,
    },
    /// CR 608.2c: this clause continues/patches the prior emitted clause rather
    /// than emitting an independent def. Folds the former `absorbed_by_followup`
    /// and `followup_continuation` pair (absorbed path, the pre-U5 lowering pass). The clause
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

/// CR 608.2c + CR 603.7a: WHERE a clause's lowered definition lives in the
/// assembled tree — an axis orthogonal to [`ClauseDisposition`], which says WHAT
/// the clause does relative to its neighbours.
///
/// The two are genuinely independent. A payload continuation *emits* its own
/// definition (so its disposition is `Emit`, with `Emit`'s continuation channels
/// intact), but that definition belongs inside a delayed trigger's payload chain
/// rather than beside it. Folding this into `ClauseDisposition` would either
/// duplicate `Emit`'s channels on a second variant or force every one of `Emit`'s
/// construction sites to restate "not nested".
///
/// CR 608.2c: later text can modify the meaning of earlier text. A continuation
/// whose referent was minted by a delayed payload must be followed when that
/// payload is followed — CR 603.7a: at the delayed ability's resolution, not at
/// its creation.
///
/// Set to `Sibling` for every clause by construction; only
/// `classify_continuation_clause` (`oracle_effect/mod.rs`) ever promotes a clause
/// to `NestedInDelayedPayload`, and only `assemble_effect_chain` ever reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub(crate) enum ClausePlacement {
    /// The clause's definition joins the chain as a top-level sibling.
    #[default]
    Sibling,
    /// CR 603.7a: the clause's definition is relocated into the nearest still-open
    /// delayed trigger installed earlier in this chain.
    NestedInDelayedPayload,
}

impl ClausePlacement {
    /// `skip_serializing_if` predicate — the default needs no JSON byte.
    pub(crate) fn is_sibling(placement: &Self) -> bool {
        matches!(placement, Self::Sibling)
    }
}

/// CR 608.2c: a field-level modification folded onto the prior emitted def
/// (emits no sibling). Promoted from the former special-clause markers
/// `AltCostRider` / `ManaRetention` / `EntersTappedAttacking` (U5-M2). Each
/// variant is a distinct rules concept that
/// modifies a different field/aspect of the prior def.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) enum PriorModifier {
    /// CR 118.9 + CR 119.4: fold an alternative cost onto the prior grant —
    /// a `CastFromZone` (Xander's Pact, Nashi) when one is in scope, else a
    /// `GrantCastingPermission { PlayFromExile }`'s `alt_ability_cost` (Inside
    /// Information's "you may play those cards" grant, which also authorizes
    /// land plays the alt cost must not affect).
    AltCost(AbilityCost),
    /// CR 106.4: fold a mana-retention expiry onto the prior Mana effect.
    ManaRetention(ManaExpiry),
    /// CR 118.14 + CR 609.4b: fold an any-color / any-type mana concession
    /// ("you may spend mana as though it were mana of any color to cast that
    /// spell", Siphon Insight; "Mana of any type can be spent to cast a spell
    /// this way", Gonti, Night Minister) onto the prior cast grant — a
    /// `CastFromZone` or a `GrantCastingPermission { PlayFromExile }` — since
    /// it applies only to mana spent casting through that grant (CR 118.14 for
    /// "any type"; the "any color" rider names its own object). The rider
    /// states no permission of its own.
    ManaSpendPermission(ManaSpendPermission),
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
    /// CR 614.1a + CR 608.2c: within one effect chain, a clause replaces a prior
    /// clause's definition via Cow-swap; tail clauses are stashed in the
    /// override's `else_ability`. This remains distinct from the cross-document-
    /// item `DocumentRelationIr::SelfReplacementOverride` relation.
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
    /// CR 702.1c + CR 608.2c: "The same is true for <keywords>." — replicate the antecedent
    /// conditional PERPETUAL keyword-GRANT clause per keyword (Mutable Pupa). Each
    /// replicated grant is gated on the entering object having THAT keyword, an
    /// independent OR-branch (unlike `StaticGrant`, whose Odric antecedent carries
    /// no per-keyword condition). Digital-only Alchemy (no CR entry for
    /// "perpetually").
    PerpetualKeywordGrant,
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
    /// One choose instruction and its mutually exclusive instead branch share this id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) declares_chosen_clause: Option<ClauseId>,
    /// The exact choose instruction named by this selected-group consumer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reads_chosen_clause: Option<ClauseId>,
    /// The prior return instruction whose actual results a delayed clause names.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reads_return_result: Option<(ClauseId, ReturnResultReadSpec)>,
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
    /// CR 115.10a + CR 701.41a: producer-declared target-choice timing captured
    /// from `ParseContext` after this chunk was parsed. When `Some`, it outranks
    /// the text-scan ladder in `lower::target_choice_timing_for_clause`, which
    /// cannot read a keyword-action shorthand correctly because the shorthand is
    /// not the ability's rules text ("support 2" contains no "target"; CR 701.41a
    /// defines it to mean "… up to two other target creatures"). `None` (the
    /// default) leaves that ladder in charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) declared_target_choice_timing: Option<TargetChoiceTiming>,
    /// CR 105.4 + CR 608.2c: this clause's text printed its own colour choice
    /// ("of the color of your choice"), captured from `ParseContext` after this
    /// chunk was parsed. Declared per-clause provenance — assembly gates the
    /// injected `Effect::Choose(Color)` on THIS, never on a shape scan of the
    /// lowered tree ("antecedents are named, never searched").
    ///
    /// Propagated by `absorb_clause` like every other intrinsic field: dropping it
    /// there is compile-silent (the `ClauseDraft` default supplies `None`) and
    /// would leave a nested-chain clause with `IsChosenColor` stamped and no
    /// chooser injected — a fail-closed, match-NOTHING filter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) printed_color_choice: Option<ChoiceType>,
    /// CR 607.2d + CR 608.2d: which KIND of chosen-colour
    /// reference this clause's KEYWORD GRANT printed, DERIVED ONCE from this
    /// clause's own verbatim `source_text` at `ClauseDraft::push` — the sealed
    /// single construction gate.
    ///
    /// Deriving here rather than lifting a `ParseContext` channel is deliberate:
    /// `Protection`/`HexproofFrom(ChosenColor)` are produced by
    /// `types/keywords.rs::parse_protection_target` / `parse_hexproof_filter`,
    /// pure context-free functions with ten call sites (backlog F8), so no ctx
    /// channel exists to lift. Because EVERY `ClauseIr` is minted here, no
    /// `.push()` site can forget it, and `absorb_clause` RE-DERIVES it from the
    /// re-located fragment rather than copying it.
    ///
    /// FAILURE POLARITY, stated once: `None` means "no chosen-colour grant phrase
    /// found", which the consumer treats as NOT independent, so the CR 607.2d
    /// suppression still fires and today's behaviour is preserved byte-for-byte.
    /// Suppression is withheld ONLY on an affirmative `IncludesIndependentChoice`
    /// stamp. A missed or unclassifiable clause therefore cannot un-suppress
    /// Floating Shield, the pool's only live consumer of this relation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) chosen_color_grant: Option<ChosenColorGrantReference>,
    /// CR 608.2c + CR 603.7a: where this clause's lowered definition attaches.
    /// `Sibling` (default) is promoted only when this clause continues an open
    /// delayed payload and is consumed by `assemble_effect_chain`'s relocation step.
    #[serde(default, skip_serializing_if = "ClausePlacement::is_sibling")]
    pub(crate) placement: ClausePlacement,
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
    /// First clause after the most recent printed back-reference repeat.
    /// A later reader cannot treat a return before this boundary as one
    /// instruction-local result: that return executed once per iteration.
    repeated_process_end: Option<ClauseId>,
    /// Accumulated clauses in source order.
    clauses: Vec<ClauseIr>,
    /// CR 611.2a + CR 608.2c: the printed leading duration of the chunk currently
    /// being processed, when that chunk was produced by
    /// `sequence::expand_leading_duration_chunks` and therefore does NOT carry the
    /// duration phrase in its own text.
    ///
    /// THE SINGLE FUNNEL. `ClauseDraft::push` is the one point every emitted clause
    /// passes through — `absorb_clause` re-mints through `clause(..).push()` too — so
    /// applying the stamp there, rather than at each of the chunk loop's many emit
    /// sites, is what makes "every recovered conjunct carries the printed duration"
    /// true by construction instead of by enumeration.
    pending_leading_duration: Option<Duration>,
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
            repeated_process_end: None,
            clauses: Vec::new(),
            pending_leading_duration: None,
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
            declared_target_choice_timing: None,
            printed_color_choice: None,
            placement: ClausePlacement::Sibling,
        }
    }

    /// Whether any clause has been pushed yet.
    pub(crate) fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// CR 608.2c: a recognized "repeat this process" directive executes the
    /// preceding process again. A following clause needs an aggregate result,
    /// not one repeated instruction's result, to refer back across it.
    pub(crate) fn note_repeated_process_boundary(&mut self) {
        self.repeated_process_end = Some(ClauseId(self.next_clause_id));
    }

    /// CR 611.2a: arm (or disarm) the leading-duration stamp for the chunk about to
    /// be processed. Called once at the top of every chunk-loop iteration with
    /// `chunk.leading_duration`, so a chunk that carries none clears a previous
    /// chunk's stamp rather than inheriting it.
    pub(crate) fn set_pending_leading_duration(&mut self, duration: Option<Duration>) {
        self.pending_leading_duration = duration;
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
        // CR 608.2c: `placement` is deliberately NOT propagated. Every field below is
        // an intrinsic property of the clause; `placement` is a RELATIONAL verdict
        // about one position in one chain's delayed-payload placement registry, and a
        // verdict computed against a nested chain's registry has no meaning in this
        // one. The re-minted clause starts at `ClausePlacement::Sibling` (the
        // `#[default]`, set by `ClauseIrBuilder::clause`), and only this chain's own
        // `classify_continuation_clause` may promote it. Same reasoning as §R8.1.1's
        // rejection of a recursively-lowered rider, one chain level down.
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
        .declared_target_choice_timing(c.declared_target_choice_timing)
        .printed_color_choice(c.printed_color_choice)
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
    declared_target_choice_timing: Option<TargetChoiceTiming>,
    printed_color_choice: Option<ChoiceType>,
    placement: ClausePlacement,
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
    /// CR 115.10a + CR 701.41a: declare THIS clause's target-choice timing
    /// directly, for a producer that expanded a keyword-action shorthand into a
    /// targeted effect. The only writer is the chain chunk loop, lifting
    /// `ParseContext::declared_target_choice_timing` immediately after the chunk
    /// parses. `lower::target_choice_timing_for_clause` honours it ahead of its
    /// text-scan ladder, which would otherwise read "support 2", find no
    /// "target", and classify a targeted announcement as a described
    /// resolution-time pick — suppressing its target slots entirely.
    pub(crate) fn declared_target_choice_timing(mut self, v: Option<TargetChoiceTiming>) -> Self {
        self.declared_target_choice_timing = v;
        self
    }
    /// CR 105.4 + CR 608.2c: declare that THIS clause's text printed its own
    /// colour choice ("… of the color of your choice").
    ///
    /// The only writer is the chain chunk loop, lifting
    /// `ParseContext::pending_printed_color_choice` immediately after the chunk
    /// parses. Setting it here is what licenses `assemble_effect_chain` to
    /// inject the matching `Effect::Choose(Color)`; the injector reads THIS
    /// declared provenance and never a shape scan of the lowered tree, so a
    /// clause that stamped `FilterProp::IsChosenColor` and a clause that gets a
    /// chooser are the same clause by construction. Leaving it `None` on a
    /// clause that did stamp the filter yields a fail-closed, match-NOTHING
    /// filter with no `Effect::Unimplemented` and no parse warning.
    pub(crate) fn printed_color_choice(mut self, v: Option<ChoiceType>) -> Self {
        self.printed_color_choice = v;
        self
    }

    /// Mint the `ClauseId` + `ChainRelative` `OracleUnitSource` and commit the
    /// clause into the builder's source-ordered list.
    pub(crate) fn push(mut self) {
        // CR 611.2a + CR 608.2c: a chunk severed from a leading-duration sentence
        // carries the printed duration as a TYPED value rather than in its own text,
        // so the duration must be applied here — the single funnel every emitted
        // clause passes through — and through the same chain-distributing authority
        // U1 installed, so a recovered conjunct that is itself a chain-building
        // recognizer distributes to its own governed links too.
        // CR 608.2c (read the whole text): a recovered conjunct is arbitrary printed
        // text and may state its OWN window, which the sentence's leading duration
        // must not clobber.
        if self.builder.pending_leading_duration.is_some() {
            // "Printed its own window" is a TEXTUAL fact, deliberately NOT read off
            // `ParsedEffectClause.duration`: several recognizers inject
            // `Some(Duration::UntilEndOfTurn)` into that field as a DEFAULT which is
            // byte-identical to a printed window, so the carrier cannot distinguish
            // the two. `oracle_effect/subject.rs`'s tapped-bound prohibition is the
            // proof — one recognizer emits a printed `ForAsLongAs{SourceIsTapped}`
            // (Braided Net) or an injected `UntilEndOfTurn` (Dovin Baan, Xathrid
            // Gorgon) depending only on a suffix.
            //
            // NOTE those two cards are cited for the RECOGNIZER's behaviour, not as
            // traffic through this seam: `starts_clause_text_or_conjugated` excludes
            // "its", so Dovin Baan's bare " and its ..." never splits and never
            // reaches `push` at all. Its link is governed by
            // `with_clause_chain_duration`'s sub-link walk, whose gate is still
            // CARRIER-based and therefore still cannot tell an injected default from
            // a printed window — measured, Dovin Baan, Edifice of Authority and
            // Mythos of Vadrok carry a link stuck at `UntilEndOfTurn` under an
            // `UntilNextTurnOf` head. Teferi's Protection is a fourth instance in a
            // different shape and from a DIFFERENT CAUSE — carrier `None` (which
            // would PASS the walk's gate), injected `UntilEndOfTurn` on the EMBEDDED
            // effect duration, different recognizer — so the head window never
            // reaches that link at all. Both are KNOWN REMAINING GAPS this seam does
            // not fix, and the second is not closed by the injected-default class
            // remedy alone; see the U1 section note in the 7923 integration test for
            // the measured detail and tasks #138/#144 in `game/effects/effect.rs`.
            //
            // COUPLING: `strip_trailing_duration` finds only trailing windows, so a
            // conjunct printing a LEADING window of its own would read as unprinted.
            // Unreachable today only because no duration phrase is a member of
            // `starts_clause_text_lower`, so no split ever cuts a chunk immediately
            // before "until ". If that changes, ask `strip_leading_duration` here too.
            let printed_own_window = strip_trailing_duration(&self.source_text).1.is_some();
            let governing = if printed_own_window {
                // The text printed a window. If the carrier holds it, distribute THAT
                // down the links. If the carrier is empty, the recognizer routed the
                // printed window into an embedded effect field instead — stamping the
                // sentence's window here would have `apply_duration_to_effect`
                // overwrite it, the exact clobber this gate exists to prevent — so
                // leave the clause untouched.
                //
                // Since #7959 the EMBEDDED-side clobber this branch guards against is refused
                // structurally: `apply_duration_to_effect` yields to a written embedded window
                // on every `Option<Duration>` writer, through `duration_is_unset_sentinel`
                // (CR 611.2a). This textual gate STAYS, because it guards the OTHER
                // carrier: `AbilityDefinition.duration` is still written unconditionally by
                // `with_clause_duration`, and an injected `UntilEndOfTurn` is still
                // indistinguishable from a printed one there (#7962). Do not delete it as
                // redundant.
                self.parsed.duration.clone()
            } else {
                self.builder.pending_leading_duration.clone()
            };
            if let Some(duration) = governing {
                self.parsed = with_clause_chain_duration(self.parsed, duration);
            }
        }
        let id = ClauseId(self.builder.next_clause_id);
        let declares_chosen_clause = if matches!(&self.parsed.effect, Effect::TargetOnly { .. }) {
            if matches!(
                self.condition,
                Some(AbilityCondition::AdditionalCostPaidInstead)
            ) {
                self.builder
                    .clauses
                    .last()
                    .filter(|clause| matches!(clause.parsed.effect, Effect::TargetOnly { .. }))
                    .and_then(|clause| clause.declares_chosen_clause)
                    .or(Some(id))
            } else {
                Some(id)
            }
        } else {
            None
        };
        let chosen_return = crate::parser::oracle_effect::imperative::parse_chosen_return_spec(
            &self.source_text,
            &self.source_text.to_ascii_lowercase(),
        );
        let reads_chosen_clause = chosen_return.as_ref().and_then(|spec| {
            let mut candidates = self
                .builder
                .clauses
                .iter()
                .filter(|clause| {
                    let Effect::TargetOnly { target } = &clause.parsed.effect else {
                        return false;
                    };
                    let role_matches = match &spec.chooser {
                        None => true,
                        Some(TargetFilter::Controller) => clause.target_chooser.is_none(),
                        Some(role) => clause.target_chooser.as_ref() == Some(role),
                    };
                    role_matches && chosen_noun_compatible(target, &spec.noun)
                })
                .filter_map(|clause| clause.declares_chosen_clause)
                .collect::<Vec<_>>();
            candidates.sort_unstable();
            candidates.dedup();
            (candidates.len() == 1).then(|| candidates[0])
        });
        if chosen_return.is_some() && reads_chosen_clause.is_none() {
            self.parsed.effect = Effect::unimplemented("return", &self.source_text);
        }
        let mut reads_return_result = None;
        let delayed_result_reader = self.delayed_condition.is_some()
            || self.prefix_delayed_condition.is_some()
            || matches!(&self.parsed.effect, Effect::CreateDelayedTrigger { .. });
        if let Some(parsed_reader) = delayed_result_reader
            .then(|| {
                crate::parser::oracle_effect::imperative::parse_returned_this_way_quantity(
                    &self.source_text.to_ascii_lowercase(),
                )
            })
            .flatten()
        {
            let body_valid = match &self.parsed.effect {
                Effect::CreateDelayedTrigger {
                    condition:
                        DelayedTriggerCondition::AtNextPhase {
                            phase: crate::types::phase::Phase::Upkeep,
                        },
                    effect,
                    ..
                } => {
                    matches!(
                        &*effect.effect,
                        Effect::Token {
                            count: QuantityExpr::Ref {
                                qty: QuantityRef::TrackedSetSize
                                    | QuantityRef::FilteredTrackedSetSize { .. }
                            },
                            ..
                        }
                    )
                }
                _ => false,
            };
            let antecedent = parsed_reader.as_ref().and_then(|reader| {
                let mut candidates = self
                    .builder
                    .clauses
                    .iter()
                    .filter(|clause| {
                        clause.reads_chosen_clause.is_some()
                            && matches!(
                                &clause.parsed.effect,
                                Effect::BounceAll {
                                    destination: None | Some(Zone::Hand),
                                    ..
                                }
                            )
                            && crate::parser::oracle_effect::imperative::parse_chosen_return_spec(
                                clause.source.fragment().unwrap_or_default(),
                                &clause
                                    .source
                                    .fragment()
                                    .unwrap_or_default()
                                    .to_ascii_lowercase(),
                            )
                            .is_some_and(|producer| {
                                producer.destination == reader.destination
                                    && producer.recipient.as_ref() == Some(&reader.recipient)
                                    && chosen_noun_compatible(&producer.noun, &reader.noun)
                            })
                    })
                    .map(|clause| clause.id)
                    .collect::<Vec<_>>();
                candidates.sort_unstable();
                candidates.dedup();
                if candidates.iter().any(|id| {
                    self.builder
                        .repeated_process_end
                        .is_some_and(|boundary| *id < boundary)
                }) {
                    return None;
                }
                match candidates.as_slice() {
                    [id] => Some(*id),
                    _ => None,
                }
            });
            if body_valid {
                if let (Some(spec), Some(id)) = (parsed_reader, antecedent) {
                    reads_return_result = Some((id, spec));
                } else {
                    self.parsed.effect = Effect::unimplemented("create", &self.source_text);
                }
            } else {
                self.parsed.effect = Effect::unimplemented("create", &self.source_text);
            }
        }
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
            declares_chosen_clause,
            reads_chosen_clause,
            reads_return_result,
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
            declared_target_choice_timing: self.declared_target_choice_timing,
            printed_color_choice: self.printed_color_choice,
            chosen_color_grant: crate::parser::oracle_nom::filter::classify_chosen_color_grant(
                &self.source_text,
            ),
            placement: self.placement,
            _sealed: (),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::ast::parsed_clause;
    use crate::types::ability::{Duration, Effect};

    #[test]
    fn chosen_return_ir_links_the_matching_producer_past_an_unrelated_return() {
        let text = "Choose target creature you own. Return each chosen creature to your hand. Choose target artifact you own. Return each chosen artifact to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way.";
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            text,
            AbilityKind::Spell,
            &mut crate::parser::oracle_effect::ParseContext::default(),
        );
        let clauses = &ir.clauses;
        assert_eq!(clauses.len(), 5, "{clauses:#?}");
        let first = clauses[0]
            .declares_chosen_clause
            .expect("creature producer");
        let second = clauses[2]
            .declares_chosen_clause
            .expect("artifact producer");
        assert_ne!(first, second);
        assert_eq!(clauses[1].reads_chosen_clause, Some(first));
        assert_eq!(clauses[3].reads_chosen_clause, Some(second));
        assert_eq!(
            clauses[4].reads_return_result.as_ref().map(|(id, _)| *id),
            Some(clauses[1].id)
        );
        assert!(matches!(
            clauses[4].parsed.effect,
            Effect::CreateDelayedTrigger { .. }
        ));
    }

    #[test]
    fn delayed_return_reader_requires_one_matching_recipient_and_producer() {
        for (text, expected_producers) in [
            ("Choose target creature you own. Choose target creature of an opponent's choice. Return each creature you chose to your hand. Return each creature that opponent chose to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way.", 2),
            ("Choose target creature you own. Return each chosen creature to its owner's hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way.", 1),
        ] {
            let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
                text,
                AbilityKind::Spell,
                &mut crate::parser::oracle_effect::ParseContext::default(),
            );
            let reader = ir.clauses.last().expect("delayed clause");
            assert_eq!(ir.clauses.iter().filter(|clause| clause.reads_chosen_clause.is_some()).count(), expected_producers, "producer grammar must be reached: {ir:#?}");
            assert!(reader.reads_return_result.is_none(), "ambiguous or mismatched recipient: {ir:#?}");
            assert!(matches!(reader.parsed.effect, Effect::Unimplemented { .. }), "reader must remain an honest gap: {ir:#?}");
        }
    }

    #[test]
    fn delayed_return_reader_after_a_repeat_is_not_a_single_iteration_result() {
        let text = "Choose target creature you own. Return each chosen creature to your hand. Repeat this process once. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way.";
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            text,
            AbilityKind::Spell,
            &mut crate::parser::oracle_effect::ParseContext::default(),
        );
        assert_eq!(
            ir.clauses.len(),
            3,
            "choose, return, and reader must all parse: {ir:#?}"
        );
        assert!(
            ir.repeat_until.is_none() && ir.clauses[0].repeat_for.is_none(),
            "the bare once directive is consumed but its loop predicate remains deferred: {ir:#?}"
        );
        assert!(matches!(
            ir.clauses[1].parsed.effect,
            Effect::BounceAll { .. }
        ));
        let reader = ir.clauses.last().expect("delayed reader clause");
        assert!(
            matches!(
                crate::parser::oracle_effect::imperative::parse_returned_this_way_quantity(
                    &reader
                        .source
                        .fragment()
                        .unwrap_or_default()
                        .to_ascii_lowercase()
                ),
                Some(Some(_))
            ),
            "the printed delayed-result reader grammar must be recognized"
        );
        assert!(
            matches!(reader.parsed.effect, Effect::Unimplemented { .. }),
            "after-loop aggregate must remain unsupported unless explicitly accumulated: {ir:#?}"
        );
        assert!(
            reader.reads_return_result.is_none(),
            "after-loop reader must not bind one repeated body's result: {ir:#?}"
        );
        let lowered = crate::parser::oracle_effect::parse_effect_chain(text, AbilityKind::Spell);
        let mut cursor = &lowered;
        while let Some(next) = cursor.sub_ability.as_deref() {
            cursor = next;
        }
        assert!(
            matches!(&*cursor.effect, Effect::Unimplemented { .. }),
            "lowering must preserve the strict gap: {lowered:#?}"
        );
        assert!(cursor.reads_return_result.is_none());
    }

    #[test]
    fn delayed_return_reader_inside_repeated_process_keeps_its_instruction_link() {
        let text = "Choose target creature you own. Return each chosen creature to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way. Repeat this process once.";
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            text,
            AbilityKind::Spell,
            &mut crate::parser::oracle_effect::ParseContext::default(),
        );
        assert_eq!(
            ir.clauses.len(),
            3,
            "the directive emits no clause: {ir:#?}"
        );
        assert!(
            ir.repeat_until.is_none() && ir.clauses[0].repeat_for.is_none(),
            "the bare once directive is consumed but its loop predicate remains deferred: {ir:#?}"
        );
        assert_eq!(
            ir.clauses[2]
                .reads_return_result
                .as_ref()
                .map(|(id, _)| *id),
            Some(ir.clauses[1].id)
        );
        assert!(matches!(
            ir.clauses[2].parsed.effect,
            Effect::CreateDelayedTrigger { .. }
        ));
        let lowered = crate::parser::oracle_effect::parse_effect_chain(text, AbilityKind::Spell);
        let mut cursor = &lowered;
        while let Some(next) = cursor.sub_ability.as_deref() {
            cursor = next;
        }
        assert!(matches!(
            &*cursor.effect,
            Effect::CreateDelayedTrigger { .. }
        ));
        assert!(cursor.reads_return_result.is_some());
    }

    #[test]
    fn delayed_return_reader_cannot_bind_across_any_repeated_process_boundary() {
        for (directive, expect_count, expect_stop) in [
            ("Repeat this process once.", false, false),
            ("Repeat this process one more time.", true, false),
            (
                "Repeat this process until you put a card into your hand.",
                false,
                true,
            ),
            ("Repeat this process.", false, false),
        ] {
            let text = format!("Choose target creature you own. Return each chosen creature to your hand. {directive} Choose target artifact you own. Return each chosen artifact to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way.");
            let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
                &text,
                AbilityKind::Spell,
                &mut crate::parser::oracle_effect::ParseContext::default(),
            );
            assert_eq!(
                ir.clauses.len(),
                5,
                "both returns and reader must parse: {ir:#?}"
            );
            assert!(matches!(
                ir.clauses[1].parsed.effect,
                Effect::BounceAll { .. }
            ));
            assert!(matches!(
                ir.clauses[3].parsed.effect,
                Effect::BounceAll { .. }
            ));
            assert!(
                matches!(
                    crate::parser::oracle_effect::imperative::parse_returned_this_way_quantity(
                        &ir.clauses[4]
                            .source
                            .fragment()
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                    ),
                    Some(Some(_))
                ),
                "the delayed reader must reach the named-result grammar: {ir:#?}"
            );
            if expect_count {
                assert!(
                    ir.clauses[0].repeat_for.is_some(),
                    "fixed-count repeat must be stamped: {ir:#?}"
                );
            } else if expect_stop {
                assert!(
                    ir.repeat_until.is_some(),
                    "stop-condition repeat must be stamped: {ir:#?}"
                );
            }
            let reader = &ir.clauses[4];
            assert!(
                reader.reads_return_result.is_none(),
                "a later unrelated return cannot hide the earlier repeated producer: {ir:#?}"
            );
            assert!(
                matches!(reader.parsed.effect, Effect::Unimplemented { .. }),
                "cross-boundary aggregate needs its own model: {ir:#?}"
            );
        }
    }

    #[test]
    fn a_second_repeated_process_boundary_advances_the_reader_cutoff() {
        let text = "Choose target creature you own. Return each chosen creature to your hand. Repeat this process once. Choose target artifact you own. Return each chosen artifact to your hand. Repeat this process once. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each artifact returned to your hand this way.";
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            text,
            AbilityKind::Spell,
            &mut crate::parser::oracle_effect::ParseContext::default(),
        );
        assert_eq!(
            ir.clauses.len(),
            5,
            "both directives emit no clause: {ir:#?}"
        );
        assert!(matches!(
            ir.clauses[3].parsed.effect,
            Effect::BounceAll { .. }
        ));
        assert!(ir.clauses[4].reads_return_result.is_none());
        assert!(matches!(
            ir.clauses[4].parsed.effect,
            Effect::Unimplemented { .. }
        ));
    }

    #[test]
    fn later_independent_return_after_repeat_can_bind_its_own_reader() {
        let text = "Choose target creature you own. Return each chosen creature to your hand. Repeat this process once. Choose target artifact you own. Return each chosen artifact to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each artifact returned to your hand this way.";
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            text,
            AbilityKind::Spell,
            &mut crate::parser::oracle_effect::ParseContext::default(),
        );
        assert_eq!(
            ir.clauses.len(),
            5,
            "both return producers and reader must parse: {ir:#?}"
        );
        assert!(
            matches!(ir.clauses[1].parsed.effect, Effect::BounceAll { .. }),
            "the pre-boundary return must parse: {ir:#?}"
        );
        assert!(matches!(
            ir.clauses[3].parsed.effect,
            Effect::BounceAll { .. }
        ));
        assert_eq!(
            ir.clauses[4]
                .reads_return_result
                .as_ref()
                .map(|(id, _)| *id),
            Some(ir.clauses[3].id)
        );
        assert!(matches!(
            ir.clauses[4].parsed.effect,
            Effect::CreateDelayedTrigger { .. }
        ));
    }

    #[test]
    fn returned_to_owners_hand_reader_stays_strict_without_recipient_support() {
        let text = "Choose target creature you own. Return each chosen creature to its owner's hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to its owner's hand this way.";
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            text,
            AbilityKind::Spell,
            &mut crate::parser::oracle_effect::ParseContext::default(),
        );
        let clauses = &ir.clauses;
        assert_eq!(clauses.len(), 3, "{clauses:#?}");
        assert_eq!(
            clauses[1].reads_chosen_clause,
            clauses[0].declares_chosen_clause
        );
        assert!(clauses[2].reads_return_result.is_none());
        assert!(matches!(
            clauses[2].parsed.effect,
            Effect::Unimplemented { .. }
        ));
    }

    #[test]
    fn role_named_ir_reader_is_stable_under_producer_order_reversal() {
        for (text, expected_producer) in [
            ("Choose target creature you own. Choose target creature of an opponent's choice. Return each creature you chose to its owner's hand.", 0),
            ("Choose target creature of an opponent's choice. Choose target creature you own. Return each creature you chose to its owner's hand.", 1),
            ("Choose target creature you own. Choose target creature of an opponent's choice. Return each creature that opponent chose to its owner's hand.", 1),
        ] {
            let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
                text,
                AbilityKind::Spell,
                &mut crate::parser::oracle_effect::ParseContext::default(),
            );
            let clauses = &ir.clauses;
            assert_eq!(clauses.len(), 3, "{clauses:#?}");
            assert_ne!(clauses[0].declares_chosen_clause, clauses[1].declares_chosen_clause);
            assert_eq!(clauses[2].reads_chosen_clause, clauses[expected_producer].declares_chosen_clause);
            assert!(matches!(clauses[2].parsed.effect, Effect::BounceAll { .. }));
        }
    }

    #[test]
    fn eagles_ir_keeps_exact_return_link_and_supported_delayed_token() {
        let text = "Choose target creature you own. If this spell was kicked, instead choose any number of target creatures you own. Return each chosen creature to your hand. At the beginning of the next upkeep, create a 4/4 white Bird Soldier creature token with flying for each creature returned to your hand this way.";
        let ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            text,
            AbilityKind::Spell,
            &mut crate::parser::oracle_effect::ParseContext::default(),
        );
        let clauses = &ir.clauses;
        assert_eq!(clauses.len(), 4, "{clauses:#?}");
        let producer = clauses[0].declares_chosen_clause.expect("unpaid producer");
        assert_eq!(clauses[1].declares_chosen_clause, Some(producer));
        assert_eq!(clauses[2].reads_chosen_clause, Some(producer));
        assert_eq!(
            clauses[3].reads_return_result.as_ref().map(|(id, _)| *id),
            Some(clauses[2].id)
        );
        assert!(clauses
            .iter()
            .all(|clause| !matches!(clause.parsed.effect, Effect::Unimplemented { .. })));
        let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = &clauses[3].parsed.effect
        else {
            panic!("expected supported delayed trigger: {:#?}", clauses[3]);
        };
        assert!(matches!(
            condition,
            DelayedTriggerCondition::AtNextPhase {
                phase: crate::types::phase::Phase::Upkeep
            }
        ));
        assert!(matches!(&*effect.effect, Effect::Token { .. }));
    }

    /// CR 608.2c + CR 611.2a: `ClauseDraft::push` stamps the sentence's leading
    /// duration onto a recovered conjunct, but a recovered conjunct is arbitrary
    /// printed text and may state its OWN window, which the leading duration must
    /// NOT clobber. Both sides are asserted: the printed window survives, the unset
    /// clause is stamped. Removing the gate in `push` turns the first row red.
    ///
    /// Latent by measurement: no corpus card today pairs a leading-duration sentence
    /// with a conjunct carrying a differing printed duration, so this shape is
    /// CONSTRUCTED DIRECTLY rather than drawn from a card.
    #[test]
    fn push_yields_to_a_recovered_conjuncts_own_printed_duration() {
        fn governed() -> Effect {
            Effect::GenericEffect {
                static_abilities: Vec::new(),
                duration: None,
                target: None,
                end_cost: None,
            }
        }
        fn emit() -> ClauseDisposition {
            ClauseDisposition::Emit {
                followup: None,
                intrinsic: None,
            }
        }

        // Source text is load-bearing: the gate reads whether the conjunct PRINTED
        // a window, so a degenerate fixture ("a" / "b") would take the unprinted
        // branch and never exercise row 1 at all.
        let mut b = ClauseIrBuilder::new(
            "tap target creature until end of combat. it can't block. its activated \
             abilities can't be activated. it gains flying until end of turn",
        );
        // Deliberately NOT `UntilEndOfTurn`: row 3's injected default IS
        // `UntilEndOfTurn`, so an identical leading window would let that row pass
        // no matter which side of the gate it took.
        let leading = Duration::UntilNextTurnOf {
            player: crate::types::ability::PlayerScope::Controller,
        };
        b.set_pending_leading_duration(Some(leading.clone()));

        // Row 1: the conjunct states its own, narrower window -> PRESERVED, and
        // that window is DISTRIBUTED to the governed link beneath it (which would
        // otherwise reach the resolver with `None` and fall back to a default).
        let mut own = parsed_clause(governed());
        own.duration = Some(Duration::UntilEndOfCombat);
        own.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            governed(),
        )));
        b.clause("tap target creature until end of combat", own, None, emit())
            .push();

        // Row 2: the conjunct states nothing -> STAMPED with the leading duration.
        b.clause("it can't block", parsed_clause(governed()), None, emit())
            .push();

        // Row 3 — THE DISCRIMINATING ROW for the injected-default hazard. The
        // conjunct PRINTS no window, but a recognizer injected
        // `Some(UntilEndOfTurn)` into the parsed carrier — exactly what
        // `oracle_effect/subject.rs`'s tapped-bound prohibition does for Dovin
        // Baan's PROHIBITION RECOGNIZER — cited for the recognizer, not because
        // Dovin Baan itself reaches this seam (it never splits, so its link is
        // governed by the sub-link walk instead). Reading the carrier would mistake
        // that default for a printed window and skip the stamp.
        let mut injected = parsed_clause(governed());
        injected.duration = Some(Duration::UntilEndOfTurn);
        b.clause(
            "its activated abilities can't be activated",
            injected,
            None,
            emit(),
        )
        .push();

        // Row 4: the text PRINTS a window but the carrier is empty — the recognizer
        // routed it into an embedded effect field. Stamping the sentence's window
        // here would clobber that printed inner window, so the clause is left
        // untouched. (Fourth of the four (printed, carrier) combinations.)
        b.clause(
            "it gains flying until end of turn",
            parsed_clause(governed()),
            None,
            emit(),
        )
        .push();

        let clauses = b.finish();
        assert_eq!(clauses.len(), 4);
        assert_eq!(
            clauses[0].parsed.duration,
            Some(Duration::UntilEndOfCombat),
            "a recovered conjunct's own printed window must survive the sentence's \
             leading duration"
        );
        assert_eq!(
            clauses[0]
                .parsed
                .sub_ability
                .as_ref()
                .expect("row 1 sub-link must survive")
                .duration,
            Some(Duration::UntilEndOfCombat),
            "the conjunct's OWN window governs the links beneath it — skipping the \
             walk entirely would leave this `None` and fall back to a default"
        );
        assert_eq!(
            clauses[1].parsed.duration,
            Some(leading.clone()),
            "POSITIVE REACH GUARD: the stamp still fires on an unset conjunct, so \
             row 1 is not passing because the stamp was disabled outright"
        );
        assert_eq!(
            clauses[2].parsed.duration,
            Some(leading.clone()),
            "an INJECTED `UntilEndOfTurn` on a conjunct that printed no window must \
             not be mistaken for a printed one — the leading window governs"
        );
        assert_eq!(
            clauses[3].parsed.duration, None,
            "text printed a window but the carrier is empty: the clause must be left \
             ALONE, not stamped with the sentence's window over a printed inner one"
        );
    }

    #[test]
    fn effect_chain_ir_empty_construction() {
        let ir = EffectChainIr {
            clauses: vec![],
            kind: AbilityKind::Spell,
            continuation_kind: None,
            player_scope_rewrite: PlayerScopeRewrite::Apply,
            chain_rounding: None,
            actor: None,
            in_trigger: false,
            repeat_until: None,
            injected_color_choice: InjectedColorChoice::Permitted,
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
            continuation_kind: None,
            player_scope_rewrite: PlayerScopeRewrite::Apply,
            chain_rounding: None,
            actor: None,
            in_trigger: false,
            repeat_until: None,
            injected_color_choice: InjectedColorChoice::Permitted,
        };
        assert_eq!(ir.clauses.len(), 1);
        assert_eq!(ir.kind, AbilityKind::Spell);
    }

    #[test]
    fn single_clause_preserves_parsed_fields_and_player_scope() {
        let mut parsed = parsed_clause(Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        });
        parsed.duration = Some(Duration::Permanent);
        parsed.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::NoOp,
        )));

        let ir = EffectChainIr::single_clause(
            "draw a card",
            AbilityKind::Spell,
            parsed,
            Some(PlayerFilter::All),
            None,
            false,
        );

        let clause = &ir.clauses[0];
        assert_eq!(clause.player_scope, Some(PlayerFilter::All));
        assert_eq!(clause.parsed.duration, Some(Duration::Permanent));
        assert!(clause.parsed.sub_ability.is_some());
    }
}
