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
    ActivationManaPaymentRestriction, ActivationRestriction, ChoiceType, ChosenGroupId,
    ControllerRef, CostReduction, DelayedTriggerCondition, Duration, Effect, ManaSpendPermission,
    MultiTargetSpec, OpponentMayScope, PlayerFilter, QuantityExpr, QuantityRef,
    ReturnResultReadSpec, RoundingMode, SubAbilityLink, TargetChoiceTiming, TargetFilter,
    TargetReadOrigin, TargetSelectionMode, UnlessPayModifier,
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
    /// CR 603.12 + CR 608.2c: name only the terminal printed producer. Earlier
    /// instructions, another paragraph, and mode-local instructions cannot lend
    /// this connector an antecedent.
    pub(crate) fn bind_counter_removal(
        &mut self,
        event: &crate::types::counter::CounterMatch,
    ) -> Option<ClauseId> {
        let terminal = self.clauses.last()?;
        let id = terminal.id;
        if self.clauses.iter().filter(|clause| clause.id == id).count() != 1
            || terminal.declares_instruction_result.is_some()
            || terminal.reads_return_result.is_some()
            || terminal.condition.is_some()
            || terminal.is_optional
            || terminal.player_scope.is_some()
            || terminal.repeat_for.is_some()
            || terminal.multi_target.is_some()
            || terminal.opponent_may_scope.is_some()
            || terminal.unless_pay.is_some()
            || terminal.delayed_condition.is_some()
            || terminal.prefix_delayed_condition.is_some()
            || self.repeat_until.is_some()
        {
            return None;
        }
        let (producer, target) = match &terminal.parsed.effect {
            Effect::ChooseCounterAdjustment {
                adjustment,
                counters,
                target,
                ..
            } if adjustment.allows_remove() => (counters.clone(), target),
            Effect::RemoveCounter {
                counter_type,
                target,
                ..
            } => (
                counter_type.clone().map_or(
                    crate::types::counter::CounterMatch::Any,
                    crate::types::counter::CounterMatch::OfType,
                ),
                target,
            ),
            _ => return None,
        };
        if !matches!(
            target,
            TargetFilter::SelfRef | TargetFilter::ParentTarget | TargetFilter::Typed(_)
        ) || !matches!(event, crate::types::counter::CounterMatch::Any) && event != &producer
        {
            return None;
        }
        self.clauses.last_mut()?.declares_instruction_result = Some(id);
        Some(id)
    }

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
    pub(crate) fn single_clause_with_context(
        source_text: &str,
        kind: AbilityKind,
        parsed: ParsedEffectClause,
        player_scope: Option<PlayerFilter>,
        actor: Option<ControllerRef>,
        in_trigger: bool,
        ctx: &mut super::context::ParseContext,
    ) -> Self {
        let mut builder = ClauseIrBuilder::with_context(source_text, ctx);
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
            clauses: builder.finish_with_context(ctx),
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
    /// Verbatim to the supplied chain-parser buffer, which may already be
    /// normalized, reminder-stripped and joined from prepared effect lines with
    /// spaces. This is the complete semantic body, not raw document bytes or the
    /// normalized document item's physical head-line anchor.
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
    /// Preserve the complete prepared normalized description, including newline
    /// joins between collected lines. Reminders have already been stripped;
    /// byte-exact original Oracle remains in the document's source_text.
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

/// CR 608.2c + CR 608.2d + CR 115.10a: one operand of an accepting-player clause's own
/// chain as parsed without and with the accepting-player seed. `node` is 0 for the
/// clause's head and n for the n-th node of `parsed.sub_ability`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AcceptingPlayerBinding {
    pub(crate) node: usize,
    pub(crate) unseeded: TargetFilter,
    pub(crate) seeded: TargetFilter,
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
    /// CR 608.2c: this exact instruction publishes its own typed result.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) declares_instruction_result: Option<ClauseId>,
    /// One choose instruction and its mutually exclusive instead branch share this id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) declares_chosen_clause: Option<ClauseId>,
    /// The exact choose instruction named by this selected-group consumer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reads_chosen_clause: Option<ClauseId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) declares_chosen_group: Option<ChosenGroupId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reads_chosen_group: Option<ChosenGroupId>,
    /// CR 115.1 + CR 608.2c: where this clause's `ObjectScope::Target` reads take
    /// their object from. Comparative gates and token definitions write it
    /// after proving the immediately preceding clause announced the object.
    #[serde(skip_serializing_if = "TargetReadOrigin::is_own")]
    pub(crate) target_reads: TargetReadOrigin,
    /// CR 111.3 + CR 608.2c: exact creation completed by this defining sentence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_definition_owner: Option<ClauseId>,
    /// The exact prior zone instruction whose settled results this clause names.
    /// Returns retain their delayed noun/destination predicate; Seek cost grants
    /// consume the completed cards independently of their actual destination.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reads_return_result: Option<(ClauseId, ReturnResultReadSpec)>,
    /// Chain-local source (`SpanPrecision::ChainRelative`) and complete chunk
    /// fragment. The chunk may already be normalized/derived; `locate` retains
    /// a zero-width cursor anchor if it is absent from the chain. Replaces the former
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
    /// CR 608.2c + CR 608.2d + CR 115.10a: the accepting-player seed's operand bindings
    /// for this clause's own chain (`lower::accepting_player_bindings`). Read only by
    /// `lower::lower_effect_chain_ir`, which emits them under its confinement check.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub(crate) accepting_player_bindings: Vec<AcceptingPlayerBinding>,
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
    /// CR 608.2c: the group a later clause's `TargetFilter::DeclaredPlayer` names
    /// this clause by; lowered onto its node's `declares_chosen_group`. Set by the
    /// chunk loop after the clause is pushed, and propagated by `absorb_clause`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) declared_player_group: Option<ChosenGroupId>,
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

impl ClauseIr {
    pub(crate) fn revealed_result_producer(&self) -> bool {
        let Some(source) = self.source.fragment() else {
            return false;
        };
        let Some(count) =
            crate::parser::oracle_effect::sequence::fixed_revealed_producer_count(source)
        else {
            return false;
        };
        if !matches!(&self.parsed.effect, Effect::RevealTop { count: actual, .. } if *actual == count)
            && !matches!(&self.parsed.effect, Effect::Dig { count: QuantityExpr::Fixed { value }, .. } if i64::from(*value) == i64::from(count))
        {
            return false;
        }
        let source_ir = crate::parser::oracle_effect::parse_effect_chain_ir(
            source,
            AbilityKind::Spell,
            &mut Default::default(),
        );
        let [printed] = source_ir.clauses.as_slice() else {
            return false;
        };
        if printed.parsed.effect != self.parsed.effect || printed.player_scope != self.player_scope
        {
            return false;
        }
        let mut shell = self.clone();
        shell.player_scope = None;
        shell.sought_result_shell_is_empty()
            && self.where_x_expression.is_none()
            && self.reads_return_result.is_none()
            && matches!(
                &self.parsed.effect,
                Effect::RevealTop {
                    player: TargetFilter::Controller | TargetFilter::ScopedPlayer,
                    ..
                } | Effect::Dig {
                    player: TargetFilter::Controller | TargetFilter::ScopedPlayer,
                    count: crate::types::ability::QuantityExpr::Fixed { .. },
                    keep_count: None,
                    keep_count_expr: None,
                    destination: None,
                    rest_destination: None,
                    rest_split_top_count: None,
                    rest_order: crate::types::ability::DigRestOrder::Preserve,
                    filter: TargetFilter::Any,
                    reveal: true,
                    up_to: false,
                    enter_tapped: false,
                    enters_attacking: false,
                    source: crate::types::ability::DigSource::Library
                }
            )
    }

    pub(crate) fn revealed_result_reader(&self) -> Option<ReturnResultReadSpec> {
        let (condition, noun, body) =
            crate::parser::oracle_effect::conditions::parse_all_revealed_condition(
                self.source.fragment()?,
                &mut Default::default(),
            )?;
        let effect = crate::parser::oracle_effect::sequence::parse_all_revealed_move(body)?;
        let mut shell = self.clone();
        shell.condition = None;
        if !shell.sought_result_shell_is_empty()
            || self.repeat_for.is_some()
            || self.where_x_expression.is_some()
            || self.condition.as_ref() != Some(&condition)
            || self.parsed.effect != effect
        {
            return None;
        }
        Some(ReturnResultReadSpec {
            noun,
            destination: Zone::Library,
            recipient: ControllerRef::ParentTargetOwner,
        })
    }
    /// CR 107.3c: inherited text-defined X is harmless only after the complete
    /// printed reader proves a constant cost and its exact repeat/empty shell.
    pub(crate) fn sought_cost_reader_parameters(
        &self,
    ) -> Option<(
        crate::types::mana::ManaCost,
        crate::types::statics::CostModifyMode,
    )> {
        let (repeat, difference, body) =
            crate::parser::oracle_effect::lower::strip_for_each_prefix_with_difference(
                self.source.fragment()?,
            );
        let (_, (amount, mode)) =
            crate::parser::oracle_effect::parse_sought_perpetual_cost(&body.to_lowercase()).ok()?;
        (self.sought_result_shell_is_empty()
            && self.repeat_for == repeat
            && difference.is_none()
            && !amount.has_x())
        .then_some((amount, mode))
    }

    pub(crate) fn for_each_quantity_ref(&self, f: &mut dyn FnMut(&QuantityRef)) {
        self.parsed.effect.for_each_quantity_ref(f);
        self.parsed.effect.for_each_selected_import_quantity_ref(f);
        self.disposition.for_each_quantity_ref(f);
        if let Some(player) = &self.player_scope {
            player.for_each_nested_quantity_ref(f);
        }
        if let Some(chooser) = &self.target_chooser {
            chooser.for_each_nested_quantity_ref(f);
        }
        for condition in [&self.condition, &self.parsed.condition]
            .into_iter()
            .flatten()
        {
            condition.for_each_nested_quantity_ref(f);
        }
        if let Some(repeat) = &self.repeat_for {
            repeat.for_each_ref(f);
        }
        if let Some(sub) = &self.parsed.sub_ability {
            sub.for_each_imported_quantity_ref(f);
        }
        for payment in [&self.unless_pay, &self.parsed.unless_pay]
            .into_iter()
            .flatten()
        {
            payment
                .cost
                .for_each_resolution_quantity_expr(&mut |expr| expr.for_each_ref(f));
            payment.cost.for_each_predicate_quantity_ref(f);
            payment.cost.for_each_selected_import_quantity_ref(f);
            payment.payer.for_each_nested_quantity_ref(f);
        }
    }

    fn for_each_quantity_ref_mut(&mut self, f: &mut dyn FnMut(&mut QuantityRef)) {
        self.parsed.effect.for_each_quantity_ref_mut(f);
        self.parsed
            .effect
            .for_each_selected_import_quantity_ref_mut(f);
        self.disposition.for_each_quantity_ref_mut(f);
        if let Some(player) = &mut self.player_scope {
            player.for_each_nested_quantity_ref_mut(f);
        }
        if let Some(chooser) = &mut self.target_chooser {
            chooser.for_each_nested_quantity_ref_mut(f);
        }
        for condition in [&mut self.condition, &mut self.parsed.condition]
            .into_iter()
            .flatten()
        {
            condition.for_each_nested_quantity_ref_mut(f);
        }
        if let Some(repeat) = &mut self.repeat_for {
            repeat.each_ref_mut(f);
        }
        if let Some(sub) = &mut self.parsed.sub_ability {
            sub.for_each_imported_quantity_ref_mut(f);
        }
        for payment in [&mut self.unless_pay, &mut self.parsed.unless_pay]
            .into_iter()
            .flatten()
        {
            payment
                .cost
                .for_each_resolution_quantity_expr_mut(&mut |expr| expr.each_ref_mut(f));
            payment.cost.for_each_predicate_quantity_ref_mut(f);
            payment.cost.for_each_selected_import_quantity_ref_mut(f);
            payment.payer.for_each_nested_quantity_ref_mut(f);
        }
    }

    /// CR 608.2c: only the complete supported mass-exile producer supplies this result class.
    pub(crate) fn zone_result_producer(&self) -> Option<(Zone, Zone)> {
        if !self.sought_result_shell_is_empty()
            || self.repeat_for.is_some()
            || self.where_x_expression.is_some()
            || self.reads_return_result.is_some()
        {
            return None;
        }
        match &self.parsed.effect {
            Effect::ChangeZoneAll {
                origin: None | Some(Zone::Battlefield),
                destination: Zone::Exile,
                ..
            } => {
                let proof = crate::parser::oracle_effect::parse_effect_clause(
                    self.source.fragment()?,
                    &mut Default::default(),
                );
                (proof.effect == self.parsed.effect
                    && proof.sub_ability.is_none()
                    && proof.condition.is_none()
                    && !proof.optional
                    && proof.unless_pay.is_none())
                .then_some((Zone::Battlefield, Zone::Exile))
            }
            _ => None,
        }
    }

    pub(crate) fn is_transparent_zone_result_corridor(&self) -> bool {
        self.is_transparent_sought_corridor()
    }

    /// Empty structural shell shared by the producer, corridor and reader proof.
    pub(crate) fn sought_result_shell_is_empty(&self) -> bool {
        matches!(
            self.disposition,
            ClauseDisposition::Emit {
                followup: None,
                intrinsic: None
            }
        ) && matches!(self.placement, ClausePlacement::Sibling)
            && self.condition.is_none()
            && self.parsed.condition.is_none()
            && !self.is_optional
            && !self.parsed.optional
            && self.unless_pay.is_none()
            && self.parsed.unless_pay.is_none()
            && self.parsed.sub_ability.is_none()
            && self.parsed.duration.is_none()
            && self.parsed.distribute.is_none()
            && self.parsed.multi_target.is_none()
            && self.parsed.unlowered_guard.is_none()
            && self.opponent_may_scope.is_none()
            && self.accepting_player_bindings.is_empty()
            && self.player_scope.is_none()
            && self.starting_with.is_none()
            && self.delayed_condition.is_none()
            && self.prefix_delayed_condition.is_none()
            && self.multi_target.is_none()
            && self.target_selection_mode == TargetSelectionMode::Chosen
            && self.target_chooser.is_none()
            && self.declared_target_choice_timing.is_none()
            && self.printed_color_choice.is_none()
            && self.declared_player_group.is_none()
            && self.declares_chosen_clause.is_none()
            && self.reads_chosen_clause.is_none()
            && self.declares_chosen_group.is_none()
            && self.reads_chosen_group.is_none()
            && self.token_definition_owner.is_none()
            && self.target_reads == TargetReadOrigin::OwnAnnouncement
    }

    fn is_transparent_sought_corridor(&self) -> bool {
        self.sought_result_shell_is_empty()
            && self.repeat_for.is_none()
            && self.where_x_expression.is_none()
            && self.reads_return_result.is_none()
            && matches!(&self.parsed.effect,
                Effect::GainLife { amount: QuantityExpr::Fixed { value }, player: TargetFilter::Controller }
                if *value >= 0 && crate::parser::oracle_effect::sought_corridor_life_amount(
                    self.source.fragment().unwrap_or_default()) == Some(QuantityExpr::Fixed { value: *value }))
    }

    /// Turn this clause, in place, into an honest parser gap
    /// (`Effect::unimplemented(name, <printed fragment>)`) that carries NO
    /// executable metadata — exactly what [`ClauseIrBuilder::clause`] +
    /// `Effect::unimplemented` would have minted for the same text. Identity,
    /// printed source and boundary are kept; every field that assembly would
    /// copy onto the lowered definition (optional gates, unless-payments,
    /// repeat counts, player scopes, delayed markers, target metadata,
    /// dispositions that patch other clauses, chosen-group links) is reset to
    /// its draft default, so the gap resolves as nothing and prompts no one.
    ///
    /// Destructures every field without `..`, so a new `ClauseIr` field must be
    /// classified here before this compiles.
    pub(crate) fn replace_with_gap(&mut self, name: &str) {
        let fragment = self.source.fragment().unwrap_or_default().to_string();
        let ClauseIr {
            id: _,
            declares_instruction_result,
            declares_chosen_clause,
            reads_chosen_clause,
            declares_chosen_group,
            reads_chosen_group,
            target_reads,
            token_definition_owner,
            reads_return_result,
            source: _,
            disposition,
            parsed,
            boundary: _,
            condition,
            is_optional,
            opponent_may_scope,
            accepting_player_bindings,
            repeat_for,
            player_scope,
            starting_with,
            delayed_condition,
            prefix_delayed_condition,
            multi_target,
            where_x_expression,
            unless_pay,
            target_selection_mode,
            target_chooser,
            declared_target_choice_timing,
            printed_color_choice,
            declared_player_group,
            chosen_color_grant,
            placement,
            _sealed: _,
        } = self;
        *declares_instruction_result = None;
        *declares_chosen_clause = None;
        *reads_chosen_clause = None;
        *declares_chosen_group = None;
        *reads_chosen_group = None;
        *target_reads = TargetReadOrigin::OwnAnnouncement;
        *token_definition_owner = None;
        *reads_return_result = None;
        *disposition = ClauseDisposition::Emit {
            followup: None,
            intrinsic: None,
        };
        *parsed =
            crate::parser::oracle_ir::ast::parsed_clause(Effect::unimplemented(name, fragment));
        *condition = None;
        *is_optional = false;
        *opponent_may_scope = None;
        accepting_player_bindings.clear();
        *repeat_for = None;
        *player_scope = None;
        *starting_with = None;
        *delayed_condition = None;
        *prefix_delayed_condition = None;
        *multi_target = None;
        *where_x_expression = None;
        *unless_pay = None;
        *target_selection_mode = TargetSelectionMode::Chosen;
        *target_chooser = None;
        *declared_target_choice_timing = None;
        *printed_color_choice = None;
        *declared_player_group = None;
        *chosen_color_grant = None;
        *placement = ClausePlacement::Sibling;
    }
}

// Continuations remain part of their source clause until assembly. In particular,
// an absorbed token counter clause must participate in address binding and reminting.
macro_rules! define_disposition_quantity_walk {
    ([$($mut_:tt)?]; $walk:ident; $refs:ident; $nested:ident; $owned:ident; $exprs:ident; $predicates:ident; $selected:ident) => {
        impl ClauseDisposition {
            fn $walk(& $($mut_)? self, f: &mut dyn FnMut(& $($mut_)? QuantityRef)) {
                match self {
                    Self::Emit { followup, intrinsic } => {
                        for continuation in [followup, intrinsic].into_iter().flatten() { continuation.$walk(f); }
                    }
                    Self::Continue { continuation } | Self::FoldSearchIntoElse { intrinsic: continuation } => {
                        if let Some(continuation) = continuation { continuation.$walk(f); }
                    }
                    Self::Absorb { rider, .. } | Self::BranchOtherwise { else_def: rider, .. }
                    | Self::ReplaceMeaning { kind: ReplaceMeaningKind::DigAlt(rider) | ReplaceMeaningKind::Instead(rider) } => rider.$owned(f),
                    Self::ModifyPrior { modifier: PriorModifier::AltCost(cost) } => {
                        cost.$exprs(&mut |expr| expr.$refs(f));
                        cost.$predicates(f);
                        cost.$selected(f);
                    }
                    Self::DrawnThisTurnFollowup { life_payment } => life_payment.$refs(f),
                    Self::ReplicatePerKeyword { .. }
                    | Self::ModifyPrior { modifier: PriorModifier::ManaRetention(_) | PriorModifier::ManaSpendPermission(_) | PriorModifier::EntersTappedAttacking }
                    | Self::ReplaceMeaning { kind: ReplaceMeaningKind::KeywordOverride } => {}
                }
            }
        }
        impl ContinuationAst {
            fn $walk(& $($mut_)? self, f: &mut dyn FnMut(& $($mut_)? QuantityRef)) {
                use super::ast::PutCount;
                use crate::types::ability::PtValue;
                match self {
                    Self::TokenEntersWithCounters { count, .. }
                    | Self::PutCountersOnLastCreated { count, .. } => count.$refs(f),
                    Self::TokenSourcePowerToughness { power, toughness } => {
                        for value in [power, toughness] { if let PtValue::Quantity(expr) = value { expr.$refs(f); } }
                    }
                    Self::ExileLookedAtCard { player, count, .. } => { player.$nested(f); count.$refs(f); }
                    Self::ExileOneOfThemFaceDown { enter_with_counters } => {
                        for (_, count) in enter_with_counters { count.$refs(f); }
                    }
                    Self::DigFromAmong { quantity, filter, rest_split_top_count, .. } => {
                        match quantity { PutCount::Up(expr) | PutCount::Exactly(expr) => expr.$refs(f), PutCount::All | PutCount::AnyNumber => {} }
                        filter.$nested(f);
                        if let Some(count) = rest_split_top_count { count.$refs(f); }
                    }
                    Self::SearchDestination { attach_host: filter, .. }
                    | Self::RevealHandFilter { card_filter: filter, .. }
                    | Self::EntersTappedAttacking { moved_filter: filter }
                    | Self::ChooseAndSacrificeRestFilter { sacrifice_filter: filter } => {
                        if let Some(filter) = filter { filter.$nested(f); }
                    }
                    Self::CounterSourceStatic { source_filter, .. } => {
                        if let Some(filter) = source_filter { filter.$nested(f); }
                    }
                    Self::RevealUntilConditionalKept { filter, .. } => filter.$nested(f),
                    Self::PutChosenCardsAtLibraryPosition { position } => {
                        use crate::types::ability::LibraryPosition;
                        match position { LibraryPosition::BeneathTop { depth } => depth.$refs(f), LibraryPosition::RandomWithinTop { n } => n.$refs(f), LibraryPosition::Top | LibraryPosition::Bottom | LibraryPosition::NthFromTop { .. } => {} }
                    }
                    Self::ManaRestriction { .. } | Self::ManaGrant { .. }
                    | Self::CounterSourceRiderUnreadable { .. } | Self::CounterSourceRiderDestroy
                    | Self::CounterSpellZoneRedirect { .. } | Self::CopyMayRetarget { .. }
                    | Self::SuspectLastCreated | Self::GoadLastCreated { .. }
                    | Self::SelfCostKeywordCostClarification | Self::CantRegenerate { .. }
                    | Self::EndEffectCost { .. } | Self::ExcessDamageToController { .. }
                    | Self::ChooseFromExile { .. } | Self::SearchResultClauseHandled
                    | Self::ExileSearchResultFaceDown | Self::SearchRevealResult
                    | Self::PutChoiceRemainderOnBottom | Self::ChoicePartitionDestinations { .. }
                    | Self::ExileSearchRemainder | Self::BecomesPlotted | Self::BecomesForetold
                    | Self::PutRest { .. } | Self::FaceDownProfileSpec { .. }
                    | Self::GrantExtraTurnAfterControlledTurn | Self::RevealUntilKept { .. }
                    | Self::RevealUntilAllToZone { .. } => {}
                }
            }
        }
    };
}
define_disposition_quantity_walk!([]; for_each_quantity_ref; for_each_ref; for_each_nested_quantity_ref; for_each_imported_quantity_ref; for_each_resolution_quantity_expr; for_each_predicate_quantity_ref; for_each_selected_import_quantity_ref);
define_disposition_quantity_walk!([mut]; for_each_quantity_ref_mut; each_ref_mut; for_each_nested_quantity_ref_mut; for_each_imported_quantity_ref_mut; for_each_resolution_quantity_expr_mut; for_each_predicate_quantity_ref_mut; for_each_selected_import_quantity_ref_mut);

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

/// Validate a stored address against its immutable source chain; never select a
/// replacement producer during assembly or relocation.
pub(crate) fn zone_result_reads_are_valid(clauses: &[ClauseIr], reader: &ClauseIr) -> bool {
    let reader_index = clauses
        .iter()
        .position(|clause| std::ptr::eq(clause, reader));
    let mut valid = true;
    reader.for_each_quantity_ref(&mut |reference| {
        if let QuantityRef::ZoneChangeAggregate {
            source: crate::types::ability::ZoneChangeRecordSource::InstructionResult { result }, from, to, ..
        } = reference {
            let owners: Vec<_> = clauses.iter().enumerate().filter(|(_, producer)| producer.id.0 == result.0).collect();
            valid &= matches!((owners.as_slice(), reader_index), ([(index, producer)], Some(reader_index))
                if *index < reader_index
                    && producer.declares_instruction_result == Some(producer.id)
                    && producer.zone_result_producer().is_some_and(|(origin, destination)| *from == Some(origin) && *to == Some(destination))
                    && clauses[*index+1..reader_index].iter().all(|clause| clause.zone_result_producer().is_some() || clause.is_transparent_zone_result_corridor()));
        }
    });
    valid
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
    next_chosen_object_group: u32,
    enclosing_chosen_objects: Vec<super::context::ChosenObjectDeclaration>,
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
            next_chosen_object_group: 0,
            enclosing_chosen_objects: Vec::new(),
            repeated_process_end: None,
            clauses: Vec::new(),
            pending_leading_duration: None,
        }
    }

    pub(crate) fn seed_chosen_objects(&mut self, ctx: &super::context::ParseContext) {
        self.next_chosen_object_group = ctx.next_chosen_object_group;
        self.enclosing_chosen_objects = ctx.enclosing_chosen_objects.clone();
    }

    pub(crate) fn with_context(chain_text: &str, ctx: &super::context::ParseContext) -> Self {
        let mut builder = Self::new(chain_text);
        builder.seed_chosen_objects(ctx);
        builder
    }

    pub(crate) fn finish_with_context(
        self,
        ctx: &mut super::context::ParseContext,
    ) -> Vec<ClauseIr> {
        ctx.next_chosen_object_group = ctx
            .next_chosen_object_group
            .max(self.next_chosen_object_group);
        self.finish()
    }

    pub(crate) fn synchronize_chosen_objects(&self, ctx: &mut super::context::ParseContext) {
        ctx.next_chosen_object_group = ctx
            .next_chosen_object_group
            .max(self.next_chosen_object_group);
        ctx.enclosing_chosen_objects = self.enclosing_chosen_objects.clone();
        let lexical_depth = self
            .enclosing_chosen_objects
            .iter()
            .map(|declaration| declaration.lexical_depth)
            .max()
            .map_or(0, |depth| {
                depth
                    .checked_add(1)
                    .expect("chosen declaration nesting exhausted")
            });
        let mut previous = None;
        for clause in &self.clauses {
            if let (Some(group), Effect::TargetOnly { target }) =
                (clause.declares_chosen_group, &clause.parsed.effect)
            {
                if ctx
                    .enclosing_chosen_objects
                    .iter()
                    .any(|declaration| declaration.group == group)
                {
                    continue;
                }
                ctx.enclosing_chosen_objects
                    .push(super::context::ChosenObjectDeclaration {
                        group,
                        lexical_depth,
                        noun: target.clone(),
                        chooser: clause.target_chooser.clone(),
                        preceding_group: previous,
                    });
                previous = Some(group);
            }
        }
    }

    pub(crate) fn resume_chosen_object_cursor(&mut self, ctx: &super::context::ParseContext) {
        self.next_chosen_object_group = self
            .next_chosen_object_group
            .max(ctx.next_chosen_object_group);
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
            token_definition_owner: None,
            condition: None,
            is_optional: false,
            opponent_may_scope: None,
            accepting_player_bindings: Vec::new(),
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
            declared_player_group: None,
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

    /// CR 608.2c: whether any "repeat this process" directive has been recognized
    /// in this chain — the chain then executes some of its instructions more
    /// than once, wherever the directive stood relative to a given clause.
    pub(crate) fn repeats_a_process(&self) -> bool {
        self.repeated_process_end.is_some()
    }

    /// CR 611.2a: arm (or disarm) the leading-duration stamp for the chunk about to
    /// be processed. Called once at the top of every chunk-loop iteration with
    /// `chunk.leading_duration`, so a chunk that carries none clears a previous
    /// chunk's stamp rather than inheriting it.
    pub(crate) fn set_pending_leading_duration(&mut self, duration: Option<Duration>) {
        self.pending_leading_duration = duration;
    }

    /// CR 608.2c: selection occurs in sealed, earlier clause space, before lowering.
    pub(crate) fn zone_result_antecedent(&self) -> Option<(ClauseId, Zone, Zone)> {
        if self.repeats_a_process() {
            return None;
        }
        let mut candidates = Vec::new();
        for clause in self.clauses.iter().rev() {
            if let Some((from, to)) = clause.zone_result_producer() {
                candidates.push((clause.id, from, to));
            } else if !clause.is_transparent_zone_result_corridor() {
                break;
            }
        }
        match candidates.as_slice() {
            [selected] => Some(*selected),
            _ => None,
        }
    }

    fn bind_zone_result_quantities(&mut self) {
        let Some(reader) = self.clauses.last() else {
            return;
        };
        let mut selected = Vec::new();
        reader.for_each_quantity_ref(&mut |reference| {
            if let QuantityRef::ZoneChangeAggregate {
                source: crate::types::ability::ZoneChangeRecordSource::InstructionResult { result },
                from,
                to,
                ..
            } = reference
            {
                selected.push((ClauseId(result.0), *from, *to));
            }
        });
        if selected.is_empty() {
            return;
        }
        let index = self.clauses.len() - 1;
        let valid = selected.iter().all(|(owner, from, to)| {
            let producers: Vec<_> = self.clauses.iter().enumerate().filter(|(_, clause)| clause.id == *owner).collect();
            matches!(producers.as_slice(), [(producer_index, producer)] if *producer_index < index
                && producer.zone_result_producer().is_some_and(|(origin, destination)| *from == Some(origin) && *to == Some(destination))
                && self.clauses[*producer_index+1..index].iter().all(|clause|
                    clause.zone_result_producer().is_some() || clause.is_transparent_zone_result_corridor()))
        });
        if !valid {
            self.clauses[index].replace_with_gap("departure_result_antecedent");
            return;
        }
        for (owner, _, _) in selected {
            let producer = self
                .clauses
                .iter_mut()
                .find(|clause| clause.id == owner)
                .expect("validated selected producer");
            producer.declares_instruction_result = Some(owner);
        }
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
    pub(crate) fn absorb_clauses(&mut self, mut clauses: Vec<ClauseIr>) {
        // Relocate the complete batch before minting any member: ids are local
        // to a chain, so an outer id with the same integer is unrelated.
        let relocation: Vec<_> = clauses
            .iter()
            .enumerate()
            .map(|(offset, c)| (c.id, ClauseId(self.next_clause_id + offset as u32)))
            .collect();
        let counter_producers: Vec<_> = clauses
            .iter()
            .filter(|clause| clause.declares_instruction_result == Some(clause.id))
            .map(|clause| clause.id)
            .collect();
        let zone_producers: Vec<_> = clauses
            .iter()
            .enumerate()
            .filter_map(|(index, clause)| {
                clause
                    .zone_result_producer()
                    .map(|zones| (index, clause.id, zones))
            })
            .collect();
        for (offset, clause) in clauses.iter_mut().enumerate() {
            let mut valid_zone_reads = true;
            clause.for_each_quantity_ref_mut(&mut |reference| {
                if let QuantityRef::ZoneChangeAggregate {
                    source:
                        crate::types::ability::ZoneChangeRecordSource::InstructionResult { result },
                    from,
                    to,
                    ..
                } = reference
                {
                    let producers: Vec<_> = zone_producers
                        .iter()
                        .filter(|(_, id, _)| id.0 == result.0)
                        .collect();
                    if let [(producer_index, owner, (origin, destination))] = producers.as_slice() {
                        if *producer_index < offset
                            && *from == Some(*origin)
                            && *to == Some(*destination)
                            && relocation.iter().filter(|(old, _)| old == owner).count() == 1
                        {
                            *result = crate::types::ability::ReturnResultId(
                                relocation[*producer_index].1 .0,
                            );
                        } else {
                            valid_zone_reads = false;
                        }
                    } else {
                        valid_zone_reads = false;
                    }
                }
            });
            if !valid_zone_reads {
                clause.replace_with_gap("departure_result_relocation");
            }
            if clause.condition.as_mut().is_some_and(|condition| {
                !relocate_counter_condition(condition, &relocation, &counter_producers, offset)
            }) {
                clause.replace_with_gap("counter_result_relocation");
            }
            if let Some(owner) = clause.declares_instruction_result {
                if owner == clause.id {
                    clause.declares_instruction_result = Some(relocation[offset].1);
                } else {
                    clause.replace_with_gap("instruction_result_declaration_relocation");
                }
            }
            if clause.revealed_result_reader().is_some() && clause.reads_return_result.is_none() {
                clause.replace_with_gap("all_revealed_result_relocation");
            }
            if let Some((owner, _)) = clause
                .reads_return_result
                .as_ref()
                .filter(|_| clause.revealed_result_reader().is_some())
            {
                let candidates: Vec<_> = relocation
                    .iter()
                    .enumerate()
                    .filter(|(_, (old, _))| old == owner)
                    .collect();
                if let [(producer, (_, new))] = candidates.as_slice() {
                    if *producer < offset {
                        clause.reads_return_result.as_mut().unwrap().0 = *new;
                    } else {
                        clause.replace_with_gap("all_revealed_result_relocation");
                    }
                } else {
                    clause.replace_with_gap("all_revealed_result_relocation");
                }
            }
            if let Some((owner, _)) =
                clause.reads_return_result.as_ref().filter(|_| {
                    matches!(
                        clause.parsed.effect,
                        Effect::ApplyPerpetual {
                            modification:
                                crate::types::ability::PerpetualModification::ModifyCost { .. },
                            ..
                        }
                    )
                })
            {
                let candidates: Vec<_> = relocation
                    .iter()
                    .enumerate()
                    .filter(|(_, (old, _))| old == owner)
                    .collect();
                if !matches!(candidates.as_slice(), [(producer, _)] if *producer < offset) {
                    clause.replace_with_gap("sought_cost_relocation");
                }
            }

            if let Some(owner) = clause.token_definition_owner {
                let mut matches = relocation
                    .iter()
                    .enumerate()
                    .filter(|(_, (old, _))| *old == owner);
                let mapped = matches.next().filter(|(producer, _)| *producer < offset);
                if let Some((_, (_, new))) = mapped.filter(|_| matches.next().is_none()) {
                    clause.token_definition_owner = Some(*new);
                } else {
                    clause.replace_with_gap("token_definition_owner_relocation");
                }
            }
        }
        for clause in clauses {
            self.absorb_clause(clause);
        }
    }

    fn absorb_clause(&mut self, c: ClauseIr) {
        let instruction_declaration = c.declares_instruction_result;
        let revealed_relation = c
            .revealed_result_reader()
            .and(c.reads_return_result.clone());
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
        .token_definition_owner(c.token_definition_owner)
        .condition(c.condition)
        .is_optional(c.is_optional)
        .opponent_may_scope(c.opponent_may_scope)
        .accepting_player_bindings(c.accepting_player_bindings)
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
        .declared_player_group(c.declared_player_group)
        .push();
        if let Some(absorbed) = self.clauses.last_mut() {
            absorbed.declares_instruction_result = instruction_declaration;
            absorbed.target_reads = c.target_reads;
            if let Some(relation) = revealed_relation {
                absorbed.reads_return_result = Some(relation);
            }
        }
    }

    fn bind_all_revealed_reader(&mut self) {
        let Some(reader) = self.clauses.last() else {
            return;
        };
        let Some(spec) = reader.revealed_result_reader() else {
            return;
        };
        let producer = self.clauses.iter().rev().nth(1).filter(|producer| {
            producer.revealed_result_producer()
                && producer.repeat_for.is_none()
                && producer.id < reader.id
                && self
                    .repeated_process_end
                    .is_none_or(|end| producer.id >= end)
        });
        let relation = producer.map(|producer| (producer.id, spec));
        let reader = self.clauses.last_mut().unwrap();
        if let Some(relation) = relation {
            reader.reads_return_result = Some(relation);
        } else {
            reader.replace_with_gap("all_revealed_result_consumer");
        }
    }

    /// CR 608.2c: prove a printed result relation before replacing the leaf.
    fn bind_sought_cost_reader(&mut self) {
        let Some(reader) = self.clauses.last() else {
            return;
        };
        if matches!(&reader.parsed.effect, Effect::Unimplemented { name, .. }
            if name == "sought_cost_antecedent" || name == "sought_cost_relocation")
        {
            return;
        }
        let (_, _, body) =
            crate::parser::oracle_effect::lower::strip_for_each_prefix_with_difference(
                reader.source.fragment().unwrap_or_default(),
            );
        let Ok((_, (amount, mode))) =
            crate::parser::oracle_effect::parse_sought_perpetual_cost(&body.to_lowercase())
        else {
            return;
        };
        let mut prior = self.clauses.iter().rev().skip(1);
        // allow-noncombinator: iterator over addressed clauses, not string dispatch.
        let producer = prior.find(|clause| !clause.is_transparent_sought_corridor());
        let selected = producer
            .filter(|producer| {
                reader.sought_cost_reader_parameters().is_some()
                    && producer.sought_result_shell_is_empty()
                    && producer.repeat_for.is_none()
                    && producer.reads_return_result.is_none()
                    && matches!(producer.parsed.effect, Effect::Seek { .. })
                    && self
                        .repeated_process_end
                        .is_none_or(|boundary| producer.id >= boundary)
                    && producer.id < reader.id
            })
            .map(|producer| {
                let Effect::Seek { destination, .. } = producer.parsed.effect else {
                    unreachable!()
                };
                (
                    producer.id,
                    ReturnResultReadSpec {
                        noun: TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                            crate::types::ability::TypeFilter::Card,
                        )),
                        destination,
                        recipient: ControllerRef::You,
                    },
                )
            });
        if let Some(selected) = selected {
            let reader = self.clauses.last_mut().expect("reader exists");
            reader.parsed.effect = Effect::ApplyPerpetual {
                target: TargetFilter::None,
                modification: crate::types::ability::PerpetualModification::ModifyCost {
                    amount,
                    mode,
                },
            };
            reader.reads_return_result = Some(selected);
        } else {
            // Existing conjure/chosen `they` has its own typed authority. A
            // barrier after an attempted Seek must never fall back to it.
            let independently_bound =
                producer.is_some_and(|producer| match &reader.parsed.effect {
                    Effect::ApplyPerpetual {
                        target: TargetFilter::LastCreated,
                        ..
                    } => matches!(producer.parsed.effect, Effect::Conjure { .. }),
                    Effect::ApplyPerpetual {
                        target: TargetFilter::ParentTarget,
                        ..
                    } => matches!(
                        producer.parsed.effect,
                        Effect::ChooseFromZone { .. } | Effect::TargetOnly { .. }
                    ),
                    _ => false,
                });
            let reader = self.clauses.last_mut().expect("reader exists");
            if !independently_bound {
                reader.replace_with_gap("sought_cost_antecedent");
            }
        }
    }

    /// Consume the builder, yielding the source-ordered clause list.
    pub(crate) fn finish(self) -> Vec<ClauseIr> {
        self.clauses
    }
}

fn relocate_counter_condition(
    condition: &mut AbilityCondition,
    relocation: &[(ClauseId, ClauseId)],
    producers: &[ClauseId],
    reader: usize,
) -> bool {
    match condition {
        AbilityCondition::CounterRemovedThisWay { result, .. } => {
            let owner = ClauseId(result.0);
            let mut matches = relocation
                .iter()
                .enumerate()
                .filter(|(_, (old, _))| *old == owner);
            let Some((position, (_, new))) = matches.next() else {
                return false;
            };
            // allow-noncombinator: typed ClauseId set membership, not text dispatch.
            if position >= reader || matches.next().is_some() || !producers.contains(&owner) {
                return false;
            }
            result.0 = new.0;
            true
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => conditions
            .iter_mut()
            .all(|condition| relocate_counter_condition(condition, relocation, producers, reader)),
        AbilityCondition::Not { condition }
        | AbilityCondition::ConditionInstead { inner: condition } => {
            relocate_counter_condition(condition, relocation, producers, reader)
        }
        _ => true,
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
    token_definition_owner: Option<ClauseId>,
    condition: Option<AbilityCondition>,
    is_optional: bool,
    opponent_may_scope: Option<OpponentMayScope>,
    accepting_player_bindings: Vec<AcceptingPlayerBinding>,
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
    declared_player_group: Option<ChosenGroupId>,
    placement: ClausePlacement,
}

impl ClauseDraft<'_> {
    pub(crate) fn token_definition_owner(mut self, owner: Option<ClauseId>) -> Self {
        self.token_definition_owner = owner;
        self
    }
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
    pub(crate) fn accepting_player_bindings(mut self, v: Vec<AcceptingPlayerBinding>) -> Self {
        self.accepting_player_bindings = v;
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
    pub(crate) fn declared_player_group(mut self, v: Option<ChosenGroupId>) -> Self {
        self.declared_player_group = v;
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
        let chosen_reference = chosen_return
            .as_ref()
            .map(|spec| (spec.noun.clone(), spec.chooser.clone()))
            .or_else(|| {
                crate::parser::oracle_effect::imperative::parse_chosen_destroy_spec(
                    &self.source_text,
                )
            });
        let role_matches = |wanted: &Option<TargetFilter>,
                            actual: &Option<TargetFilter>,
                            has_preceding_group: bool| match wanted {
            None => true,
            Some(TargetFilter::Controller) => {
                actual.is_none() || actual.as_ref() == Some(&TargetFilter::Controller)
            }
            Some(TargetFilter::Opponent) => {
                actual.as_ref() == Some(&TargetFilter::Opponent)
                    || (has_preceding_group
                        && actual.as_ref() == Some(&TargetFilter::ParentTargetController))
            }
            Some(role) => actual.as_ref() == Some(role),
        };
        let mut local_chosen_candidates_present = false;
        let reads_chosen_clause = chosen_reference.as_ref().and_then(|(noun, chooser)| {
            let mut candidates = self
                .builder
                .clauses
                .iter()
                .enumerate()
                .filter(|(index, clause)| {
                    let Effect::TargetOnly { target } = &clause.parsed.effect else {
                        return false;
                    };
                    let has_preceding_group = self.builder.clauses[..*index]
                        .iter()
                        .any(|prior| prior.declares_chosen_group.is_some());
                    let compatible =
                        role_matches(chooser, &clause.target_chooser, has_preceding_group)
                            && chosen_noun_compatible(target, noun);
                    local_chosen_candidates_present |= compatible;
                    compatible
                })
                .filter_map(|(_, clause)| clause.declares_chosen_clause)
                .collect::<Vec<_>>();
            candidates.sort_unstable();
            candidates.dedup();
            (candidates.len() == 1).then(|| candidates[0])
        });
        let reads_chosen_group = reads_chosen_clause
            .and_then(|id| {
                self.builder
                    .clauses
                    .iter()
                    .find(|clause| clause.declares_chosen_clause == Some(id))
                    .and_then(|clause| clause.declares_chosen_group)
            })
            .or_else(|| {
                chosen_reference
                    .as_ref()
                    .filter(|_| !local_chosen_candidates_present)
                    .and_then(|(noun, chooser)| {
                        let candidates: Vec<_> = self
                            .builder
                            .enclosing_chosen_objects
                            .iter()
                            .filter(|declaration| {
                                role_matches(
                                    chooser,
                                    &declaration.chooser,
                                    declaration.preceding_group.is_some(),
                                ) && chosen_noun_compatible(&declaration.noun, noun)
                            })
                            .collect();
                        let depth = candidates
                            .iter()
                            .map(|declaration| declaration.lexical_depth)
                            .max()?;
                        let candidates: Vec<_> = candidates
                            .into_iter()
                            .filter(|declaration| declaration.lexical_depth == depth)
                            .collect();
                        (candidates.len() == 1).then(|| candidates[0].group)
                    })
            });
        if chosen_reference.is_some() && reads_chosen_group.is_none() {
            self.parsed.effect = Effect::unimplemented(
                if chosen_return.is_some() {
                    "return"
                } else {
                    "destroy"
                },
                &self.source_text,
            );
        }
        let declares_chosen_group = declares_chosen_clause.map(|local| {
            self.builder
                .clauses
                .iter()
                .find(|clause| clause.declares_chosen_clause == Some(local))
                .and_then(|clause| clause.declares_chosen_group)
                .unwrap_or_else(|| {
                    let group = ChosenGroupId(self.builder.next_chosen_object_group);
                    self.builder.next_chosen_object_group = self
                        .builder
                        .next_chosen_object_group
                        .checked_add(1)
                        .expect("chosen group namespace exhausted");
                    group
                })
        });
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
            declares_instruction_result: None,
            declares_chosen_clause,
            reads_chosen_clause,
            declares_chosen_group,
            reads_chosen_group,
            target_reads: TargetReadOrigin::OwnAnnouncement,
            token_definition_owner: self.token_definition_owner,
            reads_return_result,
            source,
            disposition: self.disposition,
            parsed: self.parsed,
            boundary: self.boundary,
            condition: self.condition,
            is_optional: self.is_optional,
            opponent_may_scope: self.opponent_may_scope,
            accepting_player_bindings: self.accepting_player_bindings,
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
            declared_player_group: self.declared_player_group,
            placement: self.placement,
            _sealed: (),
        });
        self.builder.bind_sought_cost_reader();
        self.builder.bind_all_revealed_reader();
        self.builder.bind_zone_result_quantities();
    }
}

#[cfg(test)]
mod departure_result_tests {
    use super::*;
    use crate::parser::oracle_effect::{assembly::assemble_effect_chain, parse_effect_chain_ir};
    use crate::types::ability::{ReturnResultId, ZoneChangeRecordSource};

    const PRINTED: &str = "Exile all creatures. Each player creates a 0/0 green and blue Fractal creature token and puts a number of +1/+1 counters on it equal to the total power of creatures they controlled that were exiled this way.";

    fn addressed_ids(definition: &AbilityDefinition) -> Vec<ReturnResultId> {
        let mut ids = Vec::new();
        definition.for_each_own_quantity_ref(&mut |reference| {
            if let QuantityRef::ZoneChangeAggregate {
                source: ZoneChangeRecordSource::InstructionResult { result },
                ..
            } = reference
            {
                ids.push(*result);
            }
        });
        ids
    }

    #[test]
    fn oversimplify_complete_source_binds_departure_quantity() {
        let text = format!("You gain 7 life. {PRINTED}");
        let ir = parse_effect_chain_ir(&text, AbilityKind::Spell, &mut Default::default());
        let producer = ir
            .clauses
            .iter()
            .find(|clause| clause.zone_result_producer().is_some())
            .expect("full source producer");
        assert_ne!(producer.id, ClauseId(0), "real clause offset is retained");
        assert_eq!(producer.declares_instruction_result, Some(producer.id));
        let output = assemble_effect_chain(&ir);
        assert_eq!(addressed_ids(&output), vec![ReturnResultId(producer.id.0)]);
        let token = std::iter::successors(Some(&output), |node| node.sub_ability.as_deref())
            .find(|node| matches!(*node.effect, Effect::Token { .. }))
            .expect("absorbed full token instruction");
        let Effect::Token {
            enter_with_counters,
            ..
        } = token.effect.as_ref()
        else {
            unreachable!()
        };
        assert_eq!(enter_with_counters.len(), 1);
        assert_eq!(
            enter_with_counters[0].1.instruction_result_ids(),
            vec![ReturnResultId(producer.id.0)]
        );
        assert!(
            !std::iter::successors(Some(&output), |node| node.sub_ability.as_deref())
                .any(|node| matches!(*node.effect, Effect::Unimplemented { .. }))
        );
    }

    #[test]
    fn departure_quantity_assembly_honors_stored_earlier_producer() {
        let mut ir = parse_effect_chain_ir(PRINTED, AbilityKind::Spell, &mut Default::default());
        assert_eq!(
            addressed_ids(&assemble_effect_chain(&ir)),
            vec![ReturnResultId(ir.clauses[0].id.0)]
        );
        let original = ir.clauses[0].id;
        let mut later = ir.clauses[0].clone();
        later.id = ClauseId(9);
        later.declares_instruction_result = None;
        ir.clauses.last_mut().unwrap().id = ClauseId(10);
        ir.clauses.insert(1, later);
        let output = assemble_effect_chain(&ir);
        assert_eq!(
            output.declares_return_result,
            Some(ReturnResultId(original.0))
        );
        assert_eq!(addressed_ids(&output), vec![ReturnResultId(original.0)]);
        assert_ne!(
            addressed_ids(&output),
            vec![ReturnResultId(9)],
            "assembly cannot reselect the later compatible producer"
        );
    }

    #[test]
    fn departure_quantity_relocation_is_transactional() {
        let ir = parse_effect_chain_ir(PRINTED, AbilityKind::Spell, &mut Default::default());
        let prefix = parse_effect_chain_ir(
            "You gain 7 life.",
            AbilityKind::Spell,
            &mut Default::default(),
        );
        let mut builder = ClauseIrBuilder::new(PRINTED);
        builder.absorb_clauses(prefix.clauses);
        builder.absorb_clauses(ir.clauses.clone());
        let relocated = builder.finish();
        assert_eq!(
            relocated[1].declares_instruction_result,
            Some(relocated[1].id)
        );
        let mut ids = Vec::new();
        relocated
            .last()
            .unwrap()
            .for_each_quantity_ref(&mut |reference| {
                if let QuantityRef::ZoneChangeAggregate {
                    source: ZoneChangeRecordSource::InstructionResult { result },
                    ..
                } = reference
                {
                    ids.push(*result);
                }
            });
        assert_eq!(ids, vec![ReturnResultId(relocated[1].id.0)]);
        assert_ne!(relocated[1].id, ir.clauses[0].id);
        for fault in 0..5 {
            let mut malformed = ir.clauses.clone();
            match fault {
                0 => malformed[0].id = ClauseId(999),
                1 => malformed.push(malformed[0].clone()),
                2 => malformed.swap(0, 1),
                3 => {
                    if let Effect::ChangeZoneAll { destination, .. } =
                        &mut malformed[0].parsed.effect
                    {
                        *destination = Zone::Hand;
                    }
                }
                4 => malformed[0].is_optional = true,
                _ => unreachable!(),
            }
            let mut builder = ClauseIrBuilder::new(PRINTED);
            builder.absorb_clauses(malformed);
            let relocated = builder.finish();
            let mut executable_addresses = Vec::new();
            for clause in &relocated {
                clause.for_each_quantity_ref(&mut |reference| {
                    if let QuantityRef::ZoneChangeAggregate {
                        source: ZoneChangeRecordSource::InstructionResult { result },
                        ..
                    } = reference
                    {
                        executable_addresses.push(*result);
                    }
                });
            }
            assert!(
                executable_addresses.is_empty(),
                "fault {fault} cannot leave a partly remapped owned reader"
            );
            assert!(relocated
                .iter()
                .any(|clause| matches!(clause.parsed.effect, Effect::Unimplemented { .. })));
        }
    }

    #[test]
    fn departure_absorption_remints_all_enclosing_fields_and_thirteen_selected_edges() {
        use crate::types::ability::*;
        fn quantity(id: u32) -> QuantityExpr {
            QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate {
                    source: ZoneChangeRecordSource::InstructionResult {
                        result: ReturnResultId(id),
                    },
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Exile),
                    filter: TargetFilter::Any,
                    function: AggregateFunction::Sum,
                    property: ObjectProperty::Power,
                },
            }
        }
        fn player(id: u32) -> PlayerFilter {
            PlayerFilter::PlayerAttribute {
                relation: PlayerRelation::All,
                attr: Box::new(QuantityRef::PreviousEffectCount),
                comparator: Comparator::GE,
                value: Box::new(quantity(id)),
            }
        }
        fn payer(id: u32) -> TargetFilter {
            TargetFilter::PlayerMatching {
                player: Box::new(player(id)),
            }
        }
        fn wrapper(edge: usize, body: AbilityDefinition) -> Effect {
            let body = Box::new(body);
            match edge {
                0 | 1 => Effect::Vote {
                    choices: if edge == 0 {
                        vec!["read".into()]
                    } else {
                        vec![]
                    },
                    per_choice_effect: if edge == 0 {
                        vec![body.clone()]
                    } else {
                        vec![]
                    },
                    starting_with: ControllerRef::You,
                    voter_scope: VoterScope::AllPlayers,
                    tally_mode: VoteTally::PerVote,
                    subject: if edge == 1 {
                        VoteSubject::Objects {
                            candidate_filter: TargetFilter::Any,
                            outcome_template: body,
                        }
                    } else {
                        VoteSubject::Named
                    },
                    visibility: VoteVisibility::Open,
                },
                2 | 3 => Effect::SeparateIntoPiles {
                    partition_subject: VoterScope::EachOpponent,
                    object_filter: TargetFilter::Any,
                    chooser: PlayerScope::Controller,
                    pile_source: PileSource::Battlefield,
                    chosen_pile_effect: if edge == 2 {
                        body.clone()
                    } else {
                        Box::new(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp))
                    },
                    unchosen_pile_effect: (edge == 3).then_some(body),
                },
                4 => Effect::ExileTopCardContest {
                    winner_effect: body,
                },
                5 => Effect::RevealFromHand {
                    filter: TargetFilter::Any,
                    on_decline: Some(body),
                },
                6 => Effect::RollDie {
                    count: QuantityExpr::Fixed { value: 1 },
                    sides: 1,
                    modifier: None,
                    results: vec![DieResultBranch {
                        min: 1,
                        max: 1,
                        effect: body,
                    }],
                },
                7 | 8 => Effect::FlipCoins {
                    flips: crate::types::ability::CoinFlipRun::single(),
                    win_effect: (edge == 7).then_some(body.clone()),
                    lose_effect: (edge == 8).then_some(body),
                    flipper: TargetFilter::Controller,
                },
                9 | 10 => Effect::FlipCoins {
                    flips: crate::types::ability::CoinFlipRun::Exactly {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    win_effect: (edge == 9).then_some(body.clone()),
                    lose_effect: (edge == 10).then_some(body),
                    flipper: TargetFilter::Controller,
                },
                11 => Effect::FlipCoins {
                    flips: crate::types::ability::CoinFlipRun::UntilLoss,
                    win_effect: Some(body),
                    lose_effect: None,
                    flipper: TargetFilter::Controller,
                },
                12 => Effect::ChooseOneOf {
                    chooser: PlayerFilter::Controller,
                    branches: vec![*body],
                    kind: ChooseOneOfKind::Ordinary,
                },
                _ => unreachable!(),
            }
        }
        // Expected fields are selected directly, independently of both walks.
        fn selected(effect: &Effect, edge: usize) -> &AbilityDefinition {
            match (effect, edge) {
                (
                    Effect::Vote {
                        per_choice_effect, ..
                    },
                    0,
                ) => &per_choice_effect[0],
                (
                    Effect::Vote {
                        subject:
                            VoteSubject::Objects {
                                outcome_template, ..
                            },
                        ..
                    },
                    1,
                ) => outcome_template,
                (
                    Effect::SeparateIntoPiles {
                        chosen_pile_effect, ..
                    },
                    2,
                ) => chosen_pile_effect,
                (
                    Effect::SeparateIntoPiles {
                        unchosen_pile_effect: Some(body),
                        ..
                    },
                    3,
                ) => body,
                (Effect::ExileTopCardContest { winner_effect }, 4) => winner_effect,
                (
                    Effect::RevealFromHand {
                        on_decline: Some(body),
                        ..
                    },
                    5,
                ) => body,
                (Effect::RollDie { results, .. }, 6) => &results[0].effect,
                (
                    Effect::FlipCoins {
                        win_effect: Some(body),
                        ..
                    },
                    7,
                ) => body,
                (
                    Effect::FlipCoins {
                        lose_effect: Some(body),
                        ..
                    },
                    8,
                ) => body,
                (
                    Effect::FlipCoins {
                        win_effect: Some(body),
                        ..
                    },
                    9,
                ) => body,
                (
                    Effect::FlipCoins {
                        lose_effect: Some(body),
                        ..
                    },
                    10,
                ) => body,
                (
                    Effect::FlipCoins {
                        flips: crate::types::ability::CoinFlipRun::UntilLoss,
                        win_effect: Some(win_effect),
                        ..
                    },
                    11,
                ) => win_effect,
                (Effect::ChooseOneOf { branches, .. }, 12) => &branches[0],
                _ => unreachable!(),
            }
        }
        let base = parse_effect_chain_ir(PRINTED, AbilityKind::Spell, &mut Default::default());
        assert_eq!(base.clauses[0].id, ClauseId(0));
        for edge in 0..13 {
            for cost_position in 0..8 {
                for local in [false, true] {
                    let mut imported = base.clauses.clone();
                    let mut body = AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::GainLife {
                            player: TargetFilter::Controller,
                            amount: quantity(0),
                        },
                    );
                    if local {
                        let mut producer = AbilityDefinition::new(
                            AbilityKind::Spell,
                            base.clauses[0].parsed.effect.clone(),
                        );
                        producer.declares_return_result = Some(ReturnResultId(0));
                        producer.sub_ability = Some(Box::new(body));
                        body = producer;
                    }
                    let effect = wrapper(edge, body);
                    let clause = imported.last_mut().unwrap();
                    clause.parsed.effect = Effect::NoOp;
                    clause.player_scope = Some(player(0));
                    clause.target_chooser = Some(payer(0));
                    clause.unless_pay = Some(UnlessPayModifier {
                        cost: AbilityCost::PayLife {
                            amount: QuantityExpr::Fixed { value: 0 },
                        },
                        payer: payer(0),
                    });
                    clause.parsed.unless_pay = clause.unless_pay.clone();
                    match cost_position {
                        0 => clause.parsed.effect = effect,
                        1 => {
                            clause.unless_pay.as_mut().unwrap().cost = AbilityCost::EffectCost {
                                effect: Box::new(effect),
                            }
                        }
                        2 => {
                            clause.parsed.unless_pay.as_mut().unwrap().cost =
                                AbilityCost::EffectCost {
                                    effect: Box::new(effect),
                                }
                        }
                        3 => {
                            clause.disposition = ClauseDisposition::Absorb {
                                rider: Box::new(AbilityDefinition::new(AbilityKind::Spell, effect)),
                                kind: AbsorbKind::DieExile,
                            }
                        }
                        4 => {
                            clause.disposition = ClauseDisposition::BranchOtherwise {
                                else_def: Box::new(AbilityDefinition::new(
                                    AbilityKind::Spell,
                                    effect,
                                )),
                                kind: OtherwiseKind::Bound,
                            }
                        }
                        5 | 6 => {
                            let body = Box::new(AbilityDefinition::new(AbilityKind::Spell, effect));
                            clause.disposition = ClauseDisposition::ReplaceMeaning {
                                kind: if cost_position == 5 {
                                    ReplaceMeaningKind::DigAlt(body)
                                } else {
                                    ReplaceMeaningKind::Instead(body)
                                },
                            };
                        }
                        7 => {
                            clause.disposition = ClauseDisposition::ModifyPrior {
                                modifier: PriorModifier::AltCost(AbilityCost::Composite {
                                    costs: vec![AbilityCost::OneOf {
                                        costs: vec![AbilityCost::EffectCost {
                                            effect: Box::new(effect),
                                        }],
                                    }],
                                }),
                            }
                        }
                        _ => unreachable!(),
                    }
                    let mut prefix = base.clauses.clone();
                    // A colliding R0 remains declared by its original direct reader;
                    // its distinct object population cannot become the imported source.
                    if let Effect::ChangeZoneAll { target, .. } = &mut prefix[0].parsed.effect {
                        *target = TargetFilter::SpecificObject {
                            id: crate::types::identifiers::ObjectId(999),
                        };
                    }
                    let mut builder = ClauseIrBuilder::new(PRINTED);
                    builder.absorb_clauses(prefix);
                    builder.absorb_clauses(imported.clone());
                    let relocated = builder.finish();
                    let expected = relocated[2].id.0;
                    assert_eq!(expected, 2);
                    assert_eq!(relocated[0].declares_instruction_result, Some(ClauseId(0)));
                    assert_eq!(relocated[2].declares_instruction_result, Some(ClauseId(2)));
                    let clause = relocated.last().unwrap();
                    assert_eq!(clause.player_scope, Some(player(expected)));
                    assert_eq!(clause.target_chooser, Some(payer(expected)));
                    assert_eq!(clause.unless_pay.as_ref().unwrap().payer, payer(expected));
                    assert_eq!(
                        clause.parsed.unless_pay.as_ref().unwrap().payer,
                        payer(expected)
                    );
                    let effect = match cost_position {
                        0 => &clause.parsed.effect,
                        1 | 2 => {
                            let payment = if cost_position == 1 {
                                clause.unless_pay.as_ref().unwrap()
                            } else {
                                clause.parsed.unless_pay.as_ref().unwrap()
                            };
                            let AbilityCost::EffectCost { effect } = &payment.cost else {
                                unreachable!()
                            };
                            effect
                        }
                        3..=6 => {
                            let rider = match &clause.disposition {
                                ClauseDisposition::Absorb { rider, .. } => rider,
                                ClauseDisposition::BranchOtherwise { else_def, .. } => else_def,
                                ClauseDisposition::ReplaceMeaning {
                                    kind:
                                        ReplaceMeaningKind::DigAlt(body)
                                        | ReplaceMeaningKind::Instead(body),
                                } => body,
                                _ => unreachable!(),
                            };
                            rider.effect.as_ref()
                        }
                        7 => {
                            let ClauseDisposition::ModifyPrior {
                                modifier: PriorModifier::AltCost(AbilityCost::Composite { costs }),
                            } = &clause.disposition
                            else {
                                unreachable!()
                            };
                            let AbilityCost::OneOf { costs } = &costs[0] else {
                                unreachable!()
                            };
                            let AbilityCost::EffectCost { effect } = &costs[0] else {
                                unreachable!()
                            };
                            effect
                        }
                        _ => unreachable!(),
                    };
                    let body = selected(effect, edge);
                    let reader = if local {
                        assert_eq!(body.declares_return_result, Some(ReturnResultId(0)));
                        body.sub_ability.as_deref().unwrap()
                    } else {
                        body
                    };
                    let Effect::GainLife { amount, .. } = reader.effect.as_ref() else {
                        unreachable!()
                    };
                    assert_eq!(
                        *amount,
                        quantity(if local { 0 } else { expected }),
                        "edge {edge}, cost {cost_position}, local {local}"
                    );
                    if cost_position == 0 || cost_position == 3 {
                        let mut combined = base.clone();
                        combined.clauses = relocated.clone();
                        let executable = assemble_effect_chain(&combined);
                        assert!(
                            !matches!(*executable.effect, Effect::Unimplemented { .. }),
                            "complete reminted selected edge {edge}, local {local} assembles"
                        );
                        if edge == 12 && !local {
                            use crate::game::scenario::{GameScenario, P0};
                            use crate::types::actions::GameAction;
                            use crate::types::game_state::WaitingFor;
                            // Supplemental canonical composition isolates the selected
                            // import after the direct field assertions above.
                            let last = combined.clauses.last_mut().unwrap();
                            last.player_scope = None;
                            last.target_chooser = None;
                            last.unless_pay = None;
                            last.parsed.unless_pay = None;
                            let executable = assemble_effect_chain(&combined);
                            let mut scenario = GameScenario::new();
                            scenario.at_phase(crate::types::phase::Phase::PreCombatMain);
                            let first = scenario.add_creature(P0, "Centaur Courser", 3, 3).id();
                            let second = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
                            let spell = scenario
                                .add_spell_to_hand(
                                    P0,
                                    "Supplemental reminted selected import",
                                    false,
                                )
                                .with_ability_definition(executable)
                                .id();
                            let mut runner = scenario.build();
                            runner.cast(spell).resolve();
                            assert!(matches!(
                                runner.state().waiting_for,
                                WaitingFor::ChooseOneOfBranch { .. }
                            ));
                            let saved = serde_json::to_value(runner.state()).unwrap();
                            *runner.state_mut() = serde_json::from_value(saved).unwrap();
                            runner.act(GameAction::ChooseBranch { index: 0 }).unwrap();
                            assert_eq!(
                                runner.state().players[0].life,
                                25,
                                "the selected import reads reminted five, not colliding empty R0"
                            );
                            assert_eq!(runner.state().objects[&first].zone, Zone::Exile);
                            assert_eq!(runner.state().objects[&second].zone, Zone::Exile);
                            assert!(matches!(
                                runner.state().waiting_for,
                                WaitingFor::Priority { .. }
                            ));
                            assert!(runner.state().stack.is_empty());
                            assert!(runner.state().resolution_stack.is_empty());
                            assert!(runner.state().return_result_frames.is_empty());
                        }
                    }
                    // Invalid enclosing imports refuse the complete owned reader,
                    // without mutating the caller's original batch.
                    let mut malformed = imported.clone();
                    malformed[0].id = ClauseId(99);
                    let original = malformed.clone();
                    let mut bad = ClauseIrBuilder::new(PRINTED);
                    bad.absorb_clauses(malformed);
                    let refused = bad.finish();
                    assert!(matches!(
                        refused.last().unwrap().parsed.effect,
                        Effect::Unimplemented { .. }
                    ));
                    assert_eq!(original[0].id, ClauseId(99));
                    assert_eq!(imported[0].id, ClauseId(0));
                }
            }
        }
    }

    #[test]
    fn departure_disposition_raw_epic_payloads_preserve_their_independent_namespace() {
        use crate::types::ability::*;
        use crate::types::identifiers::ObjectId;
        use crate::types::player::PlayerId;
        let base = parse_effect_chain_ir(PRINTED, AbilityKind::Spell, &mut Default::default());
        for disposition in 0..5 {
            for local in [false, true] {
                let read = AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        player: TargetFilter::Controller,
                        amount: QuantityExpr::Ref {
                            qty: QuantityRef::ZoneChangeAggregate {
                                source: ZoneChangeRecordSource::InstructionResult {
                                    result: ReturnResultId(0),
                                },
                                from: Some(Zone::Battlefield),
                                to: Some(Zone::Exile),
                                filter: TargetFilter::Any,
                                function: AggregateFunction::Sum,
                                property: ObjectProperty::Power,
                            },
                        },
                    },
                );
                let mut body = read.clone();
                if local {
                    body = AbilityDefinition::new(
                        AbilityKind::Spell,
                        base.clauses[0].parsed.effect.clone(),
                    );
                    body.declares_return_result = Some(ReturnResultId(0));
                    body.sub_ability = Some(Box::new(read));
                }
                let snapshot = crate::game::ability_utils::build_resolved_from_def(
                    &body,
                    ObjectId(33),
                    PlayerId(0),
                );
                let effect = Effect::EpicCopy {
                    spell: Box::new(snapshot.clone()),
                };
                let mut imported = base.clauses.clone();
                let clause = imported.last_mut().unwrap();
                clause.parsed.effect = Effect::NoOp;
                let body = Box::new(AbilityDefinition::new(AbilityKind::Spell, effect));
                clause.disposition = match disposition {
                    0 => ClauseDisposition::Absorb {
                        rider: body,
                        kind: AbsorbKind::DieExile,
                    },
                    1 => ClauseDisposition::BranchOtherwise {
                        else_def: body,
                        kind: OtherwiseKind::Bound,
                    },
                    2 => ClauseDisposition::ReplaceMeaning {
                        kind: ReplaceMeaningKind::DigAlt(body),
                    },
                    3 => ClauseDisposition::ReplaceMeaning {
                        kind: ReplaceMeaningKind::Instead(body),
                    },
                    4 => ClauseDisposition::ModifyPrior {
                        modifier: PriorModifier::AltCost(AbilityCost::Composite {
                            costs: vec![AbilityCost::OneOf {
                                costs: vec![AbilityCost::EffectCost {
                                    effect: body.effect,
                                }],
                            }],
                        }),
                    },
                    _ => unreachable!(),
                };
                let mut builder = ClauseIrBuilder::new(PRINTED);
                builder.absorb_clauses(base.clauses.clone());
                builder.absorb_clauses(imported);
                let relocated = builder.finish();
                let effect = match &relocated.last().unwrap().disposition {
                    ClauseDisposition::Absorb { rider, .. } => rider.effect.as_ref(),
                    ClauseDisposition::BranchOtherwise { else_def, .. } => else_def.effect.as_ref(),
                    ClauseDisposition::ReplaceMeaning {
                        kind: ReplaceMeaningKind::DigAlt(body) | ReplaceMeaningKind::Instead(body),
                    } => body.effect.as_ref(),
                    ClauseDisposition::ModifyPrior {
                        modifier: PriorModifier::AltCost(AbilityCost::Composite { costs }),
                    } => {
                        let AbilityCost::OneOf { costs } = &costs[0] else {
                            unreachable!()
                        };
                        let AbilityCost::EffectCost { effect } = &costs[0] else {
                            unreachable!()
                        };
                        effect
                    }
                    _ => unreachable!(),
                };
                let Effect::EpicCopy { spell } = effect else {
                    unreachable!()
                };
                assert_eq!(
                    spell.as_ref(),
                    &snapshot,
                    "raw independent namespace is never relocated by disposition {disposition}"
                );
                assert_eq!(
                    validate_instruction_result_definition(&AbilityDefinition::new(
                        AbilityKind::Spell,
                        effect.clone()
                    ))
                    .is_ok(),
                    local
                );
            }
        }
    }

    #[test]
    fn departure_complete_ir_mixed_selected_body_refuses_transactionally_and_import_only_twin_assembles(
    ) {
        use crate::types::ability::{
            AggregateFunction, ChooseOneOfKind, ObjectProperty, PlayerFilter, QuantityExpr,
            QuantityRef, TargetFilter,
        };
        let base = parse_effect_chain_ir(PRINTED, AbilityKind::Spell, &mut Default::default());
        let producer = base
            .clauses
            .iter()
            .find(|clause| clause.zone_result_producer().is_some())
            .unwrap();
        let original = ReturnResultId(producer.id.0);
        let reference = |result| QuantityExpr::Ref {
            qty: QuantityRef::ZoneChangeAggregate {
                source: ZoneChangeRecordSource::InstructionResult { result },
                from: Some(Zone::Battlefield),
                to: Some(Zone::Exile),
                filter: TargetFilter::Any,
                function: AggregateFunction::Sum,
                property: ObjectProperty::Power,
            },
        };
        for local in [false, true] {
            let reader = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    player: TargetFilter::Controller,
                    amount: if local {
                        QuantityExpr::Sum {
                            exprs: vec![reference(original), reference(ReturnResultId(211))],
                        }
                    } else {
                        reference(original)
                    },
                },
            );
            let branch = if local {
                let mut child =
                    AbilityDefinition::new(AbilityKind::Spell, producer.parsed.effect.clone());
                child.declares_return_result = Some(ReturnResultId(211));
                child.sub_ability = Some(Box::new(reader));
                child
            } else {
                reader
            };
            let mut ir = base.clone();
            ir.clauses.last_mut().unwrap().parsed.effect = Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![branch],
                kind: ChooseOneOfKind::Ordinary,
            };
            let output = assemble_effect_chain(&ir);
            if local {
                assert!(matches!(output.effect.as_ref(), Effect::Unimplemented { .. }), "a complete selected child cannot mix its local result with the enclosing instruction's import");
                assert!(addressed_ids(&output).is_empty());
                assert_eq!(output.declares_return_result, None);
                let wire = serde_json::to_value(&output).unwrap();
                assert!(
                    // allow-noncombinator: test assertion inspects serialized identities, not Oracle parsing.
                    !wire.to_string().contains("InstructionResult"),
                    "transactional refusal removes every executable nested address"
                );
            } else {
                assert!(!matches!(
                    output.effect.as_ref(),
                    Effect::Unimplemented { .. }
                ));
                assert_eq!(output.declares_return_result, Some(original));
                assert!(
                    crate::types::ability::validate_instruction_result_definition(&output).is_ok()
                );
                assert!(
                    addressed_ids(&output).contains(&original),
                    "the functioning read-only selected twin preserves its stored earlier address"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::oracle_ir::ast::parsed_clause;
    use crate::types::ability::{Duration, Effect};

    #[test]
    fn nested_chosen_namespace_keeps_local_clause_ids_and_exact_enclosing_roles() {
        use crate::parser::oracle_effect::{parse_effect_chain_ir, ParseContext};
        use crate::types::ability::ChosenGroupId;
        let mut context = ParseContext::default();
        let outer_text = "You choose target creature an opponent controls, and that opponent chooses target creature.";
        let outer = parse_effect_chain_ir(outer_text, AbilityKind::Spell, &mut context);
        assert!(
            context.enclosing_chosen_objects.is_empty(),
            "a completed body does not publish its local names"
        );
        // Enter the actual parsed outer declarations as an enclosing lexical
        // scope, as the common builder does before parsing a coin outcome.
        let mut enclosing = ClauseIrBuilder::new(outer_text);
        enclosing.clauses = outer.clauses.clone();
        enclosing.resume_chosen_object_cursor(&context);
        enclosing.synchronize_chosen_objects(&mut context);
        assert_eq!(
            outer.clauses.len(),
            2,
            "actual coordinated declarations: {outer:#?}"
        );
        assert_eq!(outer.clauses[0].id, ClauseId(0));
        assert_eq!(outer.clauses[1].id, ClauseId(1));
        assert_eq!(
            outer.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(0))
        );
        assert_eq!(
            outer.clauses[1].declares_chosen_group,
            Some(ChosenGroupId(1))
        );
        assert_eq!(
            outer.clauses[1].target_chooser,
            Some(TargetFilter::ParentTargetController)
        );
        assert_eq!(context.next_chosen_object_group, 2);
        assert_eq!(context.enclosing_chosen_objects.len(), 2);
        assert_eq!(
            context.enclosing_chosen_objects[1].preceding_group,
            Some(ChosenGroupId(0))
        );
        let nested_text = "Choose target artifact you control. Destroy the creature your opponent chose. Flip a coin. If you win the flip, destroy the artifact you chose. If you lose the flip, destroy the creature you chose.";
        let nested = parse_effect_chain_ir(nested_text, AbilityKind::Spell, &mut context);
        assert_eq!(
            context.enclosing_chosen_objects.len(),
            2,
            "nested local names do not escape"
        );
        assert_eq!(
            nested.clauses[0].id,
            ClauseId(0),
            "source ClauseIds remain local"
        );
        assert_eq!(
            nested.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(2)),
            "runtime IDs do not alias outer ClauseId(0)"
        );
        assert_eq!(nested.clauses[1].reads_chosen_group, Some(ChosenGroupId(1)));
        let lowered = crate::parser::oracle_effect::assembly::assemble_effect_chain(&nested);
        let flip = std::iter::successors(Some(&lowered), |node| node.sub_ability.as_deref())
            .find_map(|node| match &*node.effect {
                Effect::FlipCoins {
                    win_effect,
                    lose_effect,
                    ..
                } => Some((
                    win_effect.as_deref().unwrap(),
                    lose_effect.as_deref().unwrap(),
                )),
                _ => None,
            })
            .expect("nested real coin conditional");
        assert_eq!(
            flip.0.reads_chosen_group,
            Some(ChosenGroupId(2)),
            "local compatible declaration shadows enclosing roles"
        );
        assert_eq!(
            flip.1.reads_chosen_group,
            Some(ChosenGroupId(0)),
            "different noun reads exact enclosing producer"
        );
        assert_eq!(context.next_chosen_object_group, 3);
        let next = parse_effect_chain_ir("Choose target enchantment you control. Flip a coin. If you win the flip, destroy the enchantment you chose. If you lose the flip, destroy the creature your opponent chose.", AbilityKind::Spell, &mut context);
        assert_eq!(next.clauses[0].id, ClauseId(0));
        assert_eq!(
            next.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(3))
        );
        assert_eq!(context.next_chosen_object_group, 4);
        assert_eq!(
            context.enclosing_chosen_objects.len(),
            2,
            "sequential nested bodies retain only their outer scope"
        );
        for (source, ir) in [(outer_text, &outer), (nested_text, &nested)] {
            for (index, clause) in ir.clauses.iter().enumerate() {
                assert_eq!(clause.id, ClauseId(index as u32));
                let fragment = clause.source.fragment().expect("honest source fragment");
                assert!(
                    source.contains(fragment),
                    "source fragment remains in its owning body"
                );
                assert!(
                    !matches!(clause.parsed.effect, Effect::Unimplemented { .. }),
                    "positive parser reach: {clause:#?}"
                );
            }
        }
    }

    #[test]
    fn contextual_chosen_builders_publish_cursor_without_lexical_names() {
        use crate::parser::oracle_effect::ParseContext;
        let mut context = ParseContext {
            next_chosen_object_group: 11,
            ..ParseContext::default()
        };
        let text = "Choose target artifact you control.";
        let parsed = parsed_clause(Effect::TargetOnly {
            target: TargetFilter::Typed(crate::types::ability::TypedFilter::new(
                crate::types::ability::TypeFilter::Artifact,
            )),
        });
        let mut builder = ClauseIrBuilder::with_context(text, &context);
        builder
            .clause(
                text,
                parsed.clone(),
                None,
                ClauseDisposition::Emit {
                    followup: None,
                    intrinsic: None,
                },
            )
            .push();
        let clauses = builder.finish_with_context(&mut context);
        assert_eq!(clauses[0].id, ClauseId(0));
        assert_eq!(clauses[0].declares_chosen_group, Some(ChosenGroupId(11)));
        assert_eq!(context.next_chosen_object_group, 12);
        assert!(context.enclosing_chosen_objects.is_empty());
        let same_body = EffectChainIr::single_clause_with_context(
            text,
            AbilityKind::Spell,
            parsed.clone(),
            None,
            None,
            false,
            &mut context,
        );
        assert_eq!(same_body.clauses[0].id, ClauseId(0));
        assert_eq!(
            same_body.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(12))
        );
        assert_eq!(context.next_chosen_object_group, 13);
        assert!(context.enclosing_chosen_objects.is_empty());
        let independent =
            EffectChainIr::single_clause(text, AbilityKind::Spell, parsed, None, None, false);
        assert_eq!(
            independent.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(0))
        );
        assert_eq!(context.next_chosen_object_group, 13);
    }

    #[test]
    fn sequential_coin_outcomes_do_not_publish_sibling_declarations() {
        use crate::parser::oracle_effect::{parse_effect_chain_ir, ParseContext};
        let mut context = ParseContext {
            next_chosen_object_group: 7,
            ..ParseContext::default()
        };
        for (noun, group) in [("artifact", 7), ("enchantment", 8)] {
            // Constructed grammar composition: the losing outcome has no
            // producer of its own and cannot borrow the winning declaration.
            let text = format!("Flip a coin. If you win the flip, choose target {noun} you control. If you lose the flip, destroy the {noun} you chose.");
            let ir = parse_effect_chain_ir(&text, AbilityKind::Spell, &mut context);
            let lowered = crate::parser::oracle_effect::assembly::assemble_effect_chain(&ir);
            let Effect::FlipCoins {
                win_effect,
                lose_effect,
                ..
            } = &*lowered.effect
            else {
                panic!("real coin route: {lowered:#?}")
            };
            let win = win_effect.as_deref().expect("winning declaration reached");
            assert!(matches!(&*win.effect, Effect::TargetOnly { .. }));
            assert_eq!(win.declares_chosen_group, Some(ChosenGroupId(group)));
            let lose = lose_effect.as_deref().expect("losing reader reached");
            assert!(
                matches!(&*lose.effect, Effect::Unimplemented { .. }),
                "missing producer remains an honest gap: {lose:#?}"
            );
            assert_eq!(lose.reads_chosen_group, None);
            assert_eq!(context.next_chosen_object_group, group + 1);
            assert!(context.enclosing_chosen_objects.is_empty());
            for (index, clause) in ir.clauses.iter().enumerate() {
                assert_eq!(clause.id, ClauseId(index as u32));
                let span = clause.source.span();
                assert_eq!(
                    text.get(span.start_byte..span.end_byte),
                    clause.source.fragment()
                );
            }
        }
        let independent = parse_effect_chain_ir(
            "Choose target creature.",
            AbilityKind::Spell,
            &mut ParseContext::default(),
        );
        assert_eq!(
            independent.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(0))
        );
    }

    #[test]
    fn specialized_and_no_producer_returns_preserve_chosen_namespace() {
        use crate::parser::oracle_effect::{
            parse_ability_ir_with_context, parse_effect_chain_ir, ParseContext,
        };
        let mut context = ParseContext {
            next_chosen_object_group: 19,
            ..ParseContext::default()
        };
        let declaration = super::super::context::ChosenObjectDeclaration {
            group: ChosenGroupId(18),
            lexical_depth: 0,
            noun: TargetFilter::Typed(crate::types::ability::TypedFilter::creature()),
            chooser: None,
            preceding_group: None,
        };
        context.enclosing_chosen_objects.push(declaration.clone());
        for text in [
            "Open a sealed Magic booster pack, reveal the cards, and put one of them into your hand.",
            "For each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature.",
            "For each creature attacking you or a planeswalker you control, create a token that's a copy of that creature and that's blocking that creature. Exile those tokens at end of combat.",
            "Exile any number of face-up creatures you control with disguise in a face-down pile, shuffle that pile, then cloak them.",
            "Exile the top card of each player's library with a collection counter on it.",
            "Draw a card.",
        ] {
            let ir = parse_ability_ir_with_context(text, AbilityKind::Spell, &mut context).body;
            assert!(!ir.clauses.is_empty(), "positive specialized/body reach: {text}");
            assert!(ir.clauses.iter().all(|clause| !matches!(&clause.parsed.effect, Effect::Unimplemented { .. })), "{text}: {ir:#?}");
            assert!(ir.clauses.iter().all(|clause| clause.declares_chosen_group.is_none()));
            assert_eq!(context.next_chosen_object_group, 19, "resolution choices/tracked sets do not declare target groups: {text}");
            assert_eq!(context.enclosing_chosen_objects, vec![declaration.clone()]);
        }
        let gap_text = "Choose any number of target players or planeswalkers. For each of them, reveal cards from the top of your library until you reveal a nonland card, Kaboom! deals damage equal to that card's mana value to that player or planeswalker, then you put the revealed cards on the bottom of your library in any order.";
        let gap = parse_effect_chain_ir(gap_text, AbilityKind::Spell, &mut context);
        assert_eq!(gap.clauses.len(), 1);
        assert!(
            // allow-noncombinator: test assertion compares a typed gap category, not Oracle text.
            matches!(&gap.clauses[0].parsed.effect, Effect::Unimplemented { name, .. } if name == "per_recipient_reveal_loop")
        );
        assert_eq!(gap.clauses[0].source.fragment(), Some(gap_text));
        assert_eq!(context.next_chosen_object_group, 19);
        assert_eq!(context.enclosing_chosen_objects, vec![declaration.clone()]);
        // Constructed meld grammar exercises the direct early return without
        // introducing any chosen-object declaration.
        context.card_name = Some("Source".into());
        context.pending_meld_partner = Some("Partner".into());
        let meld = parse_effect_chain_ir(
            "Exile them, then meld them into Result.",
            AbilityKind::Spell,
            &mut context,
        );
        assert!(
            matches!(&meld.clauses[0].parsed.effect, Effect::Meld { .. }),
            "positive meld reach: {meld:#?}"
        );
        assert_eq!(context.next_chosen_object_group, 19);
        assert_eq!(context.enclosing_chosen_objects, vec![declaration]);
        let producer = parse_effect_chain_ir(
            "Choose target artifact you control.",
            AbilityKind::Spell,
            &mut context,
        );
        assert_eq!(
            producer.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(19))
        );
        assert_eq!(context.next_chosen_object_group, 20);
        assert_eq!(context.enclosing_chosen_objects.len(), 1);
    }

    #[test]
    fn nested_same_noun_shadows_outer_scope_but_not_local_ambiguity() {
        use crate::parser::oracle_effect::{parse_effect_chain_ir, ParseContext};
        let mut context = ParseContext::default();
        let outer_text = "Choose target artifact you control.";
        let outer = parse_effect_chain_ir(outer_text, AbilityKind::Spell, &mut context);
        assert_eq!(
            outer.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(0))
        );
        let mut enclosing = ClauseIrBuilder::new(outer_text);
        enclosing.clauses = outer.clauses;
        enclosing.resume_chosen_object_cursor(&context);
        enclosing.synchronize_chosen_objects(&mut context);
        let outer_scope = context.enclosing_chosen_objects.clone();
        let local = parse_effect_chain_ir("Choose target artifact you control. Flip a coin. If you win the flip, destroy the artifact you chose.", AbilityKind::Spell, &mut context);
        assert_eq!(
            local.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(1))
        );
        let lowered = crate::parser::oracle_effect::assembly::assemble_effect_chain(&local);
        let Effect::FlipCoins { win_effect, .. } = &*lowered
            .sub_ability
            .as_deref()
            .expect("coin follows local declaration")
            .effect
        else {
            panic!("positive nested coin reach: {lowered:#?}")
        };
        let win = win_effect.as_deref().expect("winning reader");
        assert!(matches!(&*win.effect, Effect::Destroy { .. }));
        assert_eq!(
            win.reads_chosen_group,
            Some(ChosenGroupId(1)),
            "nearest compatible lexical scope shadows outer group0"
        );
        assert_eq!(context.enclosing_chosen_objects, outer_scope);
        assert_eq!(context.next_chosen_object_group, 2);

        let ambiguous = parse_effect_chain_ir("Choose target artifact you control. Choose target artifact you control. Flip a coin. If you win the flip, destroy the artifact you chose.", AbilityKind::Spell, &mut context);
        assert_eq!(
            ambiguous.clauses[0].declares_chosen_group,
            Some(ChosenGroupId(2))
        );
        assert_eq!(
            ambiguous.clauses[1].declares_chosen_group,
            Some(ChosenGroupId(3))
        );
        let lowered = crate::parser::oracle_effect::assembly::assemble_effect_chain(&ambiguous);
        let flip = std::iter::successors(Some(&lowered), |node| node.sub_ability.as_deref())
            .find_map(|node| match &*node.effect {
                Effect::FlipCoins { win_effect, .. } => win_effect.as_deref(),
                _ => None,
            })
            .expect("ambiguous reader reached its real coin body");
        assert!(
            matches!(&*flip.effect, Effect::Unimplemented { .. }),
            "two local producers cannot fall back to a unique outer producer: {flip:#?}"
        );
        assert_eq!(flip.reads_chosen_group, None);
        assert_eq!(context.next_chosen_object_group, 4);
        assert_eq!(context.enclosing_chosen_objects, outer_scope);
    }

    #[test]
    fn all_revealed_relation_remints_and_stops_at_boundaries() {
        use crate::parser::oracle_effect::{parse_effect_chain_ir, ParseContext};
        let text = "Each player reveals the top card of their library. If all cards revealed this way are creature cards, put those cards onto the battlefield under their owners' control.";
        let ir = parse_effect_chain_ir(text, AbilityKind::Spell, &mut ParseContext::default());
        assert_eq!(ir.clauses.len(), 2);
        assert_eq!(
            ir.clauses[1].reads_return_result.as_ref().unwrap().0,
            ir.clauses[0].id
        );
        let mut outer = ClauseIrBuilder::new(text);
        let prefix = parse_effect_chain_ir(
            "You gain 1 life.",
            AbilityKind::Spell,
            &mut Default::default(),
        );
        outer.absorb_clauses(prefix.clauses);
        outer.absorb_clauses(ir.clauses.clone());
        let relocated = outer.finish();
        assert_eq!(
            relocated[2].reads_return_result.as_ref().unwrap().0,
            relocated[1].id
        );
        assert_ne!(relocated[1].id, ir.clauses[0].id);
        for fault in 0..4 {
            let mut bad = ir.clauses.clone();
            match fault {
                0 => bad[1].reads_return_result = None,
                1 => bad[1].reads_return_result.as_mut().unwrap().0 = ClauseId(999),
                2 => bad[1].reads_return_result.as_mut().unwrap().0 = bad[1].id,
                3 => bad[1].id = bad[0].id,
                _ => unreachable!(),
            }
            let mut builder = ClauseIrBuilder::new(text);
            builder.absorb_clauses(bad);
            let relocated = builder.finish();
            assert!(
                matches!(relocated[1].parsed.effect, Effect::Unimplemented { .. }),
                "fault {fault}"
            );
            assert!(relocated[1].reads_return_result.is_none());
        }
        let mut gap = ir.clauses[1].clone();
        gap.replace_with_gap("hostile reset");
        assert!(gap.reads_return_result.is_none());
        for barrier in [
            "You gain 1 life.",
            "You may gain 1 life.",
            "Do something unsupported.",
            "Repeat this process one more time.",
        ] {
            let text = format!("Each player reveals the top card of their library. {barrier} If all cards revealed this way are creature cards, put those cards onto the battlefield under their owners' control.");
            let ir = parse_effect_chain_ir(&text, AbilityKind::Spell, &mut Default::default());
            assert!(matches!(
                ir.clauses[0].parsed.effect,
                Effect::RevealTop { .. } | Effect::Dig { .. }
            ));
            assert!(
                ir.clauses.last().unwrap().reads_return_result.is_none(),
                "{barrier}: {ir:#?}"
            );
        }
    }

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
        let text = "Choose target creature you own. Return each chosen creature to your hand. Repeat this process any number of times. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way.";
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
        let text = "Choose target creature you own. Return each chosen creature to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each creature returned to your hand this way. Repeat this process any number of times.";
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
            ("Repeat this process any number of times.", false, false),
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
        let text = "Choose target creature you own. Return each chosen creature to your hand. Repeat this process any number of times. Choose target artifact you own. Return each chosen artifact to your hand. Repeat this process any number of times. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each artifact returned to your hand this way.";
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
        let text = "Choose target creature you own. Return each chosen creature to your hand. Repeat this process any number of times. Choose target artifact you own. Return each chosen artifact to your hand. At the beginning of the next upkeep, create a 1/1 white Soldier creature token for each artifact returned to your hand this way.";
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

#[cfg(test)]
mod token_definition_tests {
    use super::*;
    use crate::parser::oracle_effect::{parse_effect_chain_ir, ParseContext};

    #[test]
    fn token_definition_default_reset_clone_and_batch_remint() {
        let text = "Create a 1/2 green Bear creature token. Create a colorless Construct artifact creature token named Twin. Its power is equal to this creature's power and its toughness is equal to this creature's toughness.";
        let ir = parse_effect_chain_ir(text, AbilityKind::Spell, &mut ParseContext::default());
        let definition = ir.clauses.last().unwrap();
        let owner = definition
            .token_definition_owner
            .expect("source definition owner");
        assert_eq!(owner, ir.clauses[1].id);
        assert_eq!(definition.clone().token_definition_owner, Some(owner));
        assert!(serde_json::to_value(&ir.clauses[0])
            .unwrap()
            .get("token_definition_owner")
            .is_none());
        assert!(serde_json::to_value(definition)
            .unwrap()
            .get("token_definition_owner")
            .is_some());
        let mut gap = definition.clone();
        gap.replace_with_gap("unit gap");
        assert_eq!(gap.token_definition_owner, None);
        assert_eq!(gap.target_reads, TargetReadOrigin::OwnAnnouncement);
        let mut outer = ClauseIrBuilder::new(text);
        outer.absorb_clauses(ir.clauses[..2].to_vec());
        // Outer id1 collides numerically with the nested creator's old id1.
        outer.absorb_clauses(ir.clauses.clone());
        let clauses = outer.finish();
        assert_eq!(
            clauses.last().unwrap().token_definition_owner,
            Some(clauses[3].id)
        );
        assert_ne!(
            clauses.last().unwrap().token_definition_owner,
            Some(clauses[1].id)
        );
        let mut parent_read = ir.clauses.clone();
        parent_read[1].target_reads = TargetReadOrigin::ParentAnnouncement;
        let mut nested = ClauseIrBuilder::new(text);
        nested.absorb_clauses(parent_read);
        assert_eq!(
            nested.finish()[1].target_reads,
            TargetReadOrigin::ParentAnnouncement
        );
        let mut malformed = ir.clauses;
        malformed.last_mut().unwrap().token_definition_owner = Some(ClauseId(999));
        let mut outer = ClauseIrBuilder::new(text);
        outer.absorb_clauses(malformed);
        let clauses = outer.finish();
        assert_eq!(clauses.last().unwrap().token_definition_owner, None);
        assert!(matches!(
            clauses.last().unwrap().parsed.effect,
            Effect::Unimplemented { .. }
        ));
    }
}

#[cfg(test)]
mod sought_cost_tests {
    use super::*;
    use crate::parser::oracle_effect::{parse_effect_chain_ir, ParseContext};
    const GRANT: &str = "Those cards perpetually gain \"This spell costs {1} less to cast.\"";
    fn parse(text: &str) -> EffectChainIr {
        parse_effect_chain_ir(text, AbilityKind::Spell, &mut ParseContext::default())
    }
    #[test]
    fn sought_constant_reader_proof_distinguishes_inherited_x_from_cost_x() {
        // SHAPE: text-defined X remains on the reader, but its quoted cost is constant.
        let text = format!("Seek X cards, where X is the number of colors among permanents you control. For each color among permanents you control, {GRANT}");
        let ir = parse(&text);
        let reader = ir.clauses.last().unwrap();
        assert!(reader.where_x_expression.is_some());
        assert!(reader.sought_cost_reader_parameters().is_some(), "{ir:#?}");
        assert!(reader.reads_return_result.is_some(), "{ir:#?}");
        let lowered = crate::parser::oracle_effect::assembly::assemble_effect_chain(&ir);
        assert!(lowered
            .sub_ability
            .as_ref()
            .unwrap()
            .reads_return_result
            .is_some());
        for cost in ["{X}", "{1}{X}"] {
            let bad = parse(&text.replace("{1}", cost));
            let reader = bad.clauses.last().unwrap();
            assert!(matches!(bad.clauses[0].parsed.effect, Effect::Seek { .. }));
            assert!(reader.sought_cost_reader_parameters().is_none());
            assert!(reader.reads_return_result.is_none());
            assert!(
                matches!(reader.parsed.effect, Effect::Unimplemented { .. }),
                "{bad:#?}"
            );
        }
    }
    #[test]
    fn jev_sought_cost_reader_binds_named_clause() {
        let text = format!("Seek a creature card. Seek an artifact card. {GRANT}");
        let ir = parse(&text);
        assert_eq!(ir.clauses.len(), 3, "{ir:#?}");
        assert_eq!(
            ir.clauses[2]
                .reads_return_result
                .as_ref()
                .map(|(id, _)| *id),
            Some(ir.clauses[1].id)
        );
        let mut changed = ir.clone();
        changed.clauses[2].reads_return_result.as_mut().unwrap().0 = ir.clauses[0].id;
        let lowered = crate::parser::oracle_effect::assembly::assemble_effect_chain(&changed);
        assert_eq!(
            lowered.declares_return_result,
            Some(crate::types::ability::ReturnResultId(ir.clauses[0].id.0))
        );
        let grant = lowered
            .sub_ability
            .as_deref()
            .unwrap()
            .sub_ability
            .as_deref()
            .unwrap();
        assert_eq!(
            grant.reads_return_result.as_ref().unwrap().0,
            lowered.declares_return_result.unwrap()
        );
        // Clone/reset/remint retain identity only inside the relocated batch.
        let mut gap = ir.clauses[2].clone();
        gap.replace_with_gap("isolation");
        assert!(gap.reads_return_result.is_none());
        let mut outer = ClauseIrBuilder::new(&text);
        outer.absorb_clauses(ir.clauses[..1].to_vec());
        outer.absorb_clauses(ir.clauses.clone());
        let relocated = outer.finish();
        assert_eq!(
            relocated[3].reads_return_result.as_ref().unwrap().0,
            relocated[2].id
        );
        let mut isolated = ClauseIrBuilder::new(&text);
        isolated.absorb_clauses(ir.clauses[..1].to_vec());
        isolated.absorb_clauses(ir.clauses[2..].to_vec());
        let isolated = isolated.finish();
        assert!(isolated[1].reads_return_result.is_none());
        assert!(matches!(
            isolated[1].parsed.effect,
            Effect::Unimplemented { .. }
        ));
    }
    #[test]
    fn sought_corridor_proves_complete_source_and_empty_typed_shells() {
        for corridor in [
            "",
            "You gain 7 life. ",
            "You gain 0 life. You gain 23 life. ",
        ] {
            let ir = parse(&format!("Seek two cards. {corridor}{GRANT}"));
            assert!(
                ir.clauses.last().unwrap().reads_return_result.is_some(),
                "{ir:#?}"
            );
        }
        for barrier in [
            "Draw a card.",
            "Conjure a card named Divination into your hand.",
            "You may gain 2 life.",
            "You gain life equal to your life total.",
            "You gain 2 life for each creature you control.",
        ] {
            let ir = parse(&format!("Seek two cards. {barrier} {GRANT}"));
            assert!(matches!(ir.clauses[0].parsed.effect, Effect::Seek { .. }));
            assert!(
                ir.clauses.last().unwrap().reads_return_result.is_none(),
                "{ir:#?}"
            );
            assert!(
                matches!(
                    ir.clauses.last().unwrap().parsed.effect,
                    Effect::Unimplemented { .. }
                ),
                "{ir:#?}"
            );
        }
        let ir = parse(&format!("Seek two cards. You gain 7 life. {GRANT}"));
        let mut mismatch = ir.clauses.clone();
        mismatch[1].parsed.effect = Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 8 },
            player: TargetFilter::Controller,
        };
        let mut builder = ClauseIrBuilder::new("isolation");
        builder.absorb_clauses(mismatch);
        assert!(builder
            .finish()
            .last()
            .unwrap()
            .reads_return_result
            .is_none());
        // Typed isolation: dispositions with no owner must be barriers even
        // when they lower to no sibling or carry an otherwise innocent effect.
        for disposition in [
            ClauseDisposition::Continue { continuation: None },
            ClauseDisposition::Absorb {
                rider: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 7 },
                        player: TargetFilter::Controller,
                    },
                )),
                kind: AbsorbKind::DieExile,
            },
        ] {
            let mut clauses = ir.clauses.clone();
            clauses[1].disposition = disposition;
            let mut builder = ClauseIrBuilder::new("isolation");
            builder.absorb_clauses(clauses);
            assert!(builder
                .finish()
                .last()
                .unwrap()
                .reads_return_result
                .is_none());
        }
    }
    #[test]
    fn sought_lowering_rejects_missing_dangling_duplicate_later_shape_and_spec() {
        let ir = parse(&format!("Seek two cards. {GRANT}"));
        assert!(ir.clauses[1].reads_return_result.is_some());
        for mutation in 0..7 {
            let mut malformed = ir.clone();
            match mutation {
                0 => malformed.clauses[1].reads_return_result = None,
                1 => malformed.clauses[1].reads_return_result.as_mut().unwrap().0 = ClauseId(999),
                2 => {
                    malformed.clauses[0].parsed.effect = Effect::GainLife {
                        amount: QuantityExpr::Fixed { value: 2 },
                        player: TargetFilter::Controller,
                    }
                }
                3 => {
                    malformed.clauses[1]
                        .reads_return_result
                        .as_mut()
                        .unwrap()
                        .1
                        .destination = Zone::Exile
                }
                4 => {
                    malformed.clauses[1]
                        .reads_return_result
                        .as_mut()
                        .unwrap()
                        .1
                        .recipient = ControllerRef::Opponent
                }
                5 => malformed.clauses.push(malformed.clauses[0].clone()),
                6 => malformed.clauses.swap(0, 1),
                _ => unreachable!(),
            }
            let lowered = crate::parser::oracle_effect::assembly::assemble_effect_chain(&malformed);
            assert!(
                std::iter::successors(Some(&lowered), |node| node.sub_ability.as_deref())
                    .any(|node| matches!(*node.effect, Effect::Unimplemented { .. })),
                "mutation {mutation}: {lowered:#?}"
            );
        }
    }
    #[test]
    fn sought_admission_does_not_erase_guarded_or_compound_shells() {
        let ir = parse(&format!("Seek two cards. {GRANT}"));
        assert!(ir.clauses[1].reads_return_result.is_some());
        for slot in [0, 1] {
            for mutation in 0..9 {
                let mut clauses = ir.clauses.clone();
                let clause = &mut clauses[slot];
                match mutation {
                    0 => clause.is_optional = true,
                    1 => clause.parsed.optional = true,
                    2 => clause.parsed.duration = Some(Duration::UntilEndOfTurn),
                    3 => clause.player_scope = Some(PlayerFilter::All),
                    4 => clause.target_chooser = Some(TargetFilter::Controller),
                    5 => clause.token_definition_owner = Some(ClauseId(0)),
                    6 => clause.reads_chosen_clause = Some(ClauseId(0)),
                    7 => clause.placement = ClausePlacement::NestedInDelayedPayload,
                    8 => {
                        clause.parsed.sub_ability = Some(Box::new(AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::GainLife {
                                amount: QuantityExpr::Fixed { value: 1 },
                                player: TargetFilter::Controller,
                            },
                        )))
                    }
                    _ => unreachable!(),
                }
                // Isolation on the existing assembler catches mutated shells;
                // remint deliberately resets relational placement, so it is
                // not the test entry for malformed already-built IR.
                let mut malformed = ir.clone();
                malformed.clauses = clauses;
                let lowered =
                    crate::parser::oracle_effect::assembly::assemble_effect_chain(&malformed);
                assert!(
                    std::iter::successors(Some(&lowered), |node| node.sub_ability.as_deref())
                        .any(|node| matches!(*node.effect, Effect::Unimplemented { .. })),
                    "slot {slot}, mutation {mutation}: {lowered:#?}"
                );
            }
        }
    }
}
