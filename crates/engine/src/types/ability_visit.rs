//! The engine's single complete `AbilityDefinition` / `Effect` traversal.
//!
//! This code was moved verbatim out of `game/printed_cards.rs`, where it was
//! specialized to conjure-name collection, and parameterized by a visitor
//! closure so any "can this ability tree contain effect shape X" question can
//! reuse it. The name-extraction leaf stayed behind in `printed_cards.rs`.
//!
//! The `match`es over `Effect`, `ContinuousModification`, and `AbilityCost` are
//! wildcard-free **on purpose**: a new variant on any of those three enums is a
//! compile error here, which forces a descend-or-leaf decision at the one place
//! that owns the answer.
//!
//! That guarantee is necessary but not sufficient. A new nested **struct field**
//! is field access, not a match arm, so it compiles silently. Three fixtures are
//! the complementary safety nets:
//!
//! - `game::printed_cards::tests::walker_covers_every_nested_carrier`
//! - `ai_support::targeted_exchange::tests::predicate_sees_a_fight_in_every_nested_carrier`
//! - `parser::oracle::tests::render_net_reaches_every_nested_description_carrier`
//!
//! Each plants a marker in every carrier its walker descends into.
//! Extend **all three** whenever a carrier is added.
//!
//! The third belongs to a **`description`-shaped** walker
//! (`parser::oracle::render_effect_descriptions`, the CR 201.5a display-render
//! net), not an `Effect`-shaped one, and at the `Effect`-ARM level its descend
//! set is a strict SUPERSET of this module's: it also descends `Effect::Mana`,
//! `GrantCastingPermission`, `ExileResolvingSpellInsteadOfGraveyard`,
//! `CreateDelayedTrigger.condition`, the copy family,
//! `AddPendingEntersModifications` and `EachPlayerCopyChosen`, all leaves here.
//! `ReturnAsAura` descends its registered continuous-modification grants here. So a carrier added there is not necessarily a carrier here,
//! but a carrier added here is always one there. The superset relation does NOT
//! extend one level down: this module descends
//! `ContinuousModification::CopyValues` into `visit_copiable_values_scoped` and
//! that net deliberately does not (`CopyValues` is parse-unreachable).
//!
//! Two narrower ad-hoc walkers remain unmigrated and are candidate future
//! consumers: `game::coverage::ability_tree_any` (which has a `_ => {}`
//! wildcard and omits many carriers — broadening it would change the coverage
//! report) and `game::replacement::ability_tree_creates_tokens` (which walks
//! only `Token` / `ChooseOneOf` / `sub_ability` / `else_ability` — broadening it
//! would change replacement behavior). Neither is migrated here, because either
//! migration would change behavior.
//!
//! The effect-axis walk is one `define_walk!` body, expanded as the shared
//! `&Effect` walk here and as the exclusive definition-node walk in [`nodes_mut`].
//!
//! # [`ResolutionScope`] — the own-resolution boundary
//!
//! Some callers need to know only what an ability does during **its own**
//! resolution, not what it merely *registers* to happen later. [`ResolutionScope`]
//! names that axis: CR 608.2c ("the controller of the spell or ability follows
//! its instructions in the order written") versus CR 603.3 (a triggered ability
//! is put on the stack "the next time a player would receive priority" — a
//! separate object, resolving separately). `is_mana_ability`'s CR 605.1a
//! library criterion is the first consumer.
//!
//! Under [`ResolutionScope::OwnResolutionOnly`] the walk visits the boundary
//! node itself but does **not** descend into payloads that belong to a later or
//! separate resolution: delayed triggers (CR 603.7a), registered replacements
//! (CR 614.1), emblem abilities (CR 114.1), token abilities (CR 111.1), granted
//! statics (CR 611.2), mana-spend grants (CR 603.3), and reflexive "when you do"
//! links (CR 603.12). Every existing public entry point passes
//! [`ResolutionScope::IncludeRegisteredLater`], so their behavior is unchanged.
//!
//! **The boundary has exactly two axes and one authority.** The effect walk
//! ([`visit_ability_def_scoped`]) and the cost walk
//! ([`visit_ability_def_costs_scoped`]) are separate because the effect
//! visitor is `FnMut(&Effect)` and an `AbilityCost::Mill` is not an `Effect` —
//! a type-level gap, not a missing match arm. Both consult
//! `scope_prunes_nested_ability` for the CR 603.12 decision and neither
//! re-implements it.
//!
//! **Scope-reset invariant.** `visit_trigger_scoped`, `visit_replacement_scoped`,
//! `visit_static_scoped`, `visit_continuous_mod_scoped`, and
//! `visit_copiable_values_scoped` are unreachable under `OwnResolutionOnly`
//! today, because every `visit_effect_scoped` arm that reaches them is a gated
//! boundary carrier. That is asserted by a `debug_assert!` at the head of each,
//! but correctness does **not** depend on the assertion: `scope` is propagated
//! into every nested `visit_ability_def_scoped` call, so the invariant holds by
//! construction in release builds too. The assertion is a decision-forcing
//! tripwire, not the mechanism.

use crate::types::ability::{
    AbilityCondition, AbilityCost, AbilityDefinition, ContinuousModification, CopiableValues,
    CounterSourceRider, Effect, ReplacementDefinition, ReplacementMode, StaticDefinition,
    TargetFilter, TriggerDefinition, VoteSubject,
};
use std::ops::ControlFlow;

/// CR 608.2c vs CR 603.3: which nested abilities a traversal attributes to the
/// ability it started from.
///
/// CR 608.2c: "The controller of the spell or ability follows its instructions in
/// the order written" — those instructions are this ability's own resolution.
/// CR 603.3: an ability that *triggers* is put on the stack "the next time a
/// player would receive priority" — a separate object, resolving separately.
/// The two rules are the two sides of one binary, and this enum names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolutionScope {
    /// Visit only what THIS ability does during its own resolution. Stop at any
    /// effect that merely REGISTERS a separate ability, replacement, or
    /// continuous effect to apply later or to another object.
    OwnResolutionOnly,
    /// Visit the entire printed subtree, including separately-registered
    /// payloads. The historical behavior of every existing entry point.
    IncludeRegisteredLater,
}

/// CR 603.12 + CR 603.7 + CR 603.3: a reflexive triggered ability ("when you do")
/// follows the rules for delayed triggered abilities and goes on the stack the
/// next time a player would receive priority. It is a SEPARATE ability, not an
/// instruction this ability follows during its own resolution (CR 608.2c).
///
/// SINGLE AUTHORITY for the reflexive boundary. Both the effect walk
/// ([`visit_nested_ability_def_scoped`]) and the cost walk
/// ([`visit_ability_def_costs_scoped`]) consult this and nothing else.
///
/// Deliberately keys on the root `WhenYouDo` marker alone. Its optional flat
/// `And` guard is still part of the separately-created reflexive ability.
/// `AbilityCondition::EffectOutcome` ("if you do, ...") is CR 608.2c — one
/// instruction conditional on another within the SAME resolution — and must
/// keep being descended. Do not reach for `effects::sub_ability_is_reflexive`,
/// which unions the two because it answers a different question
/// (skip-on-decline), not this one.
fn scope_prunes_nested_ability(def: &AbilityDefinition, scope: ResolutionScope) -> bool {
    scope == ResolutionScope::OwnResolutionOnly
        && def
            .condition
            .as_ref()
            .is_some_and(AbilityCondition::has_when_you_do_marker)
}

/// Scope-reset trapdoor tripwire, shared by the five traversal functions that
/// are reachable only through a CR 603.3 boundary carrier.
///
/// Those five — `visit_trigger_scoped`, `visit_replacement_scoped`,
/// `visit_static_scoped`, `visit_continuous_mod_scoped`, and
/// `visit_copiable_values_scoped` — are unreachable under `OwnResolutionOnly`
/// today, because every `visit_effect_scoped` arm that reaches them is a gated
/// boundary carrier: `GenericEffect`, `AddTargetReplacement`, `Counter`,
/// `Token`, and `CreateEmblem`. (Named by variant, not by line: the arm heads
/// move whenever this file is edited, and a stale number in a shipped comment
/// is worse than no number.)
///
/// Correctness does NOT depend on this assertion: `scope` is propagated into
/// every nested `visit_ability_def_scoped` call, so an un-gated arm would carry
/// `OwnResolutionOnly` correctly rather than silently resetting it. The
/// assertion exists to FORCE A DECISION — reaching here under the narrow scope
/// means someone un-gated a CR 603.3 boundary carrier, and that is a rules
/// judgment a human must make deliberately rather than inherit by accident.
///
/// Accepted residual: `debug_assert!` compiles out of release builds. That is
/// acceptable only because the tripwire is not the mechanism.
fn debug_assert_own_resolution_unreachable(scope: ResolutionScope, fn_name: &str) {
    debug_assert!(
        scope == ResolutionScope::IncludeRegisteredLater,
        "boundary-carrier arm reached {fn_name} under OwnResolutionOnly — a CR 603.3 \
         boundary was un-gated; see the ability_visit module docs before proceeding"
    );
}

/// CR 605.1a "its cost and effect" — the COST axis companion to
/// [`visit_ability_def_scoped`].
///
/// WHY THIS EXISTS AND CANNOT BE FOLDED INTO THE EFFECT WALK: the effect
/// visitor is `FnMut(&Effect)`, and `visit_cost` surfaces an `Effect` for
/// exactly one variant (`EffectCost`). `AbilityCost::Mill`, `Exile`,
/// `ExileWithAggregate`, and `ReturnToHand` carry no nested `Effect` at all, so
/// they are structurally invisible to any effect-shaped visitor. This is a
/// type-level gap, not a missing match arm.
///
/// Yields every `AbilityCost` on the CHAIN-LINK axis of the own-resolution
/// tree: the node's `cost` (CR 602.1a — the activation cost, at the root),
/// the node's `unless_pay.cost` (CR 118.12a -> CR 118.12 — a cost paid when the
/// ability RESOLVES, therefore reached under CR 608.2c as part of "its effect"),
/// and every `sub_ability` / `else_ability` / `mode_abilities` link, recursing
/// through the SAME `scope_prunes_nested_ability` authority so the CR 603.12
/// reflexive boundary holds identically on both axes.
///
/// **KNOWN GAP — inline branch carriers are NOT descended on the cost axis.**
/// `visit_effect_scoped` descends nested `AbilityDefinition`s under
/// `Vote.per_choice_effect` / `VoteSubject.outcome_template`,
/// `SeparateIntoPiles`, `RevealFromHand.on_decline`, `FlipCoins` (win and lose),
/// `ExileTopCardContest.winner_effect`, `RollDie.results`, and
/// `ChooseOneOf.branches` (all via
/// `visit_nested_ability_def_scoped`); this walk does not. A cost sitting on one
/// of those nested defs — e.g. `ChooseOneOf { branches: [AbilityDefinition {
/// cost: AbilityCost::Mill { .. }, .. }] }` — is therefore invisible here, which
/// would leave CR 605.1a's cost criterion unapplied to it. The same is true of
/// `Effect::PayCost`, whose cost is reached only by the one-node delegation in
/// `Effect::moves_card_to_or_from_library`'s `PayCost` arm. **Unreachable
/// today:** `data/card-data.json` carries zero costs of any type on an
/// effect-payload-nested `AbilityDefinition`. Closing it means driving both
/// walks from one shared carrier→nested-def list; that widens cost coverage for
/// every existing `IncludeRegisteredLater` caller too, so it needs its own
/// census rather than riding along here.
///
/// Only TOP-LEVEL cost nodes are yielded; composition (`Composite`, `OneOf`,
/// `PerCounter`) is the consuming predicate's own recursion to do. A second
/// wildcard-free `AbilityCost` match in this module would be a drift hazard
/// against `visit_cost_scoped`, which is exactly the pattern this module's docs
/// reject.
///
/// The alternative — an `AbilityNode<'a> { Effect(..), Cost(..) }` unified
/// visitor — was rejected: it would change the visitor signature for every
/// existing caller of the eight public entry points, for no correctness gain.
/// Recorded here so a future reader does not re-litigate it.
pub(crate) fn visit_ability_def_costs_scoped<F>(
    def: &AbilityDefinition,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: FnMut(&AbilityCost) -> ControlFlow<()>,
{
    // CR 602.1a: the activation cost — "everything before the colon (:)".
    if let Some(cost) = &def.cost {
        visit(cost)?;
    }
    // CR 118.12a -> CR 118.12: an "unless [a player] pays" action is a cost paid
    // WHEN THE ABILITY RESOLVES, so CR 608.2c places it under "its effect".
    if let Some(unless_pay) = &def.unless_pay {
        visit(&unless_pay.cost)?;
    }
    // Chain links, each gated by the ONE CR 603.12 boundary authority. Three
    // explicit blocks, mirroring `visit_ability_def_scoped`'s shape above, so a
    // reviewer diffing the two sibling walkers sees the gate as the only
    // difference.
    if let Some(sub) = &def.sub_ability {
        if !scope_prunes_nested_ability(sub, scope)
            && !(scope == ResolutionScope::OwnResolutionOnly
                && def.effect.sub_ability_resolves_later(sub.sub_link))
        {
            visit_ability_def_costs_scoped(sub, scope, visit)?;
        }
    }
    if let Some(else_ability) = &def.else_ability {
        if !scope_prunes_nested_ability(else_ability, scope) {
            visit_ability_def_costs_scoped(else_ability, scope, visit)?;
        }
    }
    for mode in &def.mode_abilities {
        if !scope_prunes_nested_ability(mode, scope) {
            visit_ability_def_costs_scoped(mode, scope, visit)?;
        }
    }
    ControlFlow::Continue(())
}

/// The effect-axis walk, written once and expanded in two borrow modes so both
/// descend the one carrier list: shared (every `Effect`) at this module's root,
/// exclusive (every definition node) in [`nodes_mut`].
macro_rules! define_walk {
    ([$($mut_:tt)?]; [$($bound:tt)+]; $on_effect:ident; $on_node:ident; $arc_iter:ident; $descend_nested:literal) => {
/// Effect-axis wrapper over [`scope_prunes_nested_ability`]. Every recursion
/// into a nested `AbilityDefinition` that is *not* already behind a boundary
/// gate goes through here.
fn visit_nested_ability_def_scoped<F>(
    def: &$($mut_)? AbilityDefinition,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    if scope_prunes_nested_ability(def, scope) {
        return ControlFlow::Continue(());
    }
    if !$descend_nested {
        $on_node!(visit, Ability, def);
        return ControlFlow::Continue(());
    }
    visit_ability_def_scoped(def, scope, visit)
}

pub fn visit_ability_def<F>(def: &$($mut_)? AbilityDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_ability_def_scoped(def, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_ability_def_scoped<F>(
    def: &$($mut_)? AbilityDefinition,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    $on_node!(visit, Ability, def);
    if !$descend_nested {
        return ControlFlow::Continue(());
    }
    visit_effect_scoped(&$($mut_)? def.effect, scope, visit)?;
    if let Some(cost) = &$($mut_)? def.cost {
        visit_cost_scoped(cost, scope, visit)?;
    }
    if let Some(sub) = &$($mut_)? def.sub_ability {
        if !(scope == ResolutionScope::OwnResolutionOnly
            && def.effect.sub_ability_resolves_later(sub.sub_link)) {
            visit_nested_ability_def_scoped(sub, scope, visit)?;
        }
    }
    if let Some(else_ability) = &$($mut_)? def.else_ability {
        visit_nested_ability_def_scoped(else_ability, scope, visit)?;
    }
    for mode in &$($mut_)? def.mode_abilities {
        visit_nested_ability_def_scoped(mode, scope, visit)?;
    }
    // "unless [player] pays {cost}" — the cost may be an EffectCost that conjures.
    if let Some(unless_pay) = &$($mut_)? def.unless_pay {
        visit_cost_scoped(&$($mut_)? unless_pay.cost, scope, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_trigger<F>(trigger: &$($mut_)? TriggerDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_trigger_scoped(trigger, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_trigger_scoped<F>(
    trigger: &$($mut_)? TriggerDefinition,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    debug_assert_own_resolution_unreachable(scope, "visit_trigger_scoped");
    $on_node!(visit, Trigger, trigger);
    if let Some(execute) = &$($mut_)? trigger.execute {
        visit_ability_def_scoped(execute, scope, visit)?;
    }
    if let Some(unless_pay) = &$($mut_)? trigger.unless_pay {
        visit_cost_scoped(&$($mut_)? unless_pay.cost, scope, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_replacement<F>(replacement: &$($mut_)? ReplacementDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_replacement_scoped(replacement, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_replacement_scoped<F>(
    replacement: &$($mut_)? ReplacementDefinition,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    debug_assert_own_resolution_unreachable(scope, "visit_replacement_scoped");
    $on_node!(visit, Replacement, replacement);
    if let Some(execute) = &$($mut_)? replacement.execute {
        visit_ability_def_scoped(execute, scope, visit)?;
    }
    // The mode carries the decline continuation (and, for MayCost, a cost),
    // either of which may conjure. Descend into both.
    match &$($mut_)? replacement.mode {
        ReplacementMode::MayCost { cost, decline } => {
            visit_cost_scoped(cost, scope, visit)?;
            if let Some(decline) = decline {
                visit_ability_def_scoped(decline, scope, visit)?;
            }
        }
        ReplacementMode::Optional { decline } => {
            if let Some(decline) = decline {
                visit_ability_def_scoped(decline, scope, visit)?;
            }
        }
        ReplacementMode::Mandatory => {}
    }
    // `runtime_execute` holds a resolution-time continuation that is never
    // present on a printed/static `CardFace`; skipped intentionally.
    ControlFlow::Continue(())
}

pub fn visit_static<F>(static_def: &$($mut_)? StaticDefinition, visit: &mut F) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_static_scoped(static_def, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_static_scoped<F>(
    static_def: &$($mut_)? StaticDefinition,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    debug_assert_own_resolution_unreachable(scope, "visit_static_scoped");
    $on_node!(visit, Static, static_def);
    for modification in &$($mut_)? static_def.modifications {
        visit_continuous_mod_scoped(modification, scope, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_continuous_mod<F>(
    modification: &$($mut_)? ContinuousModification,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_continuous_mod_scoped(modification, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_continuous_mod_scoped<F>(
    modification: &$($mut_)? ContinuousModification,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    debug_assert_own_resolution_unreachable(scope, "visit_continuous_mod_scoped");
    match modification {
        ContinuousModification::GrantAbility { definition } => {
            visit_ability_def_scoped(definition, scope, visit)?
        }
        ContinuousModification::GrantTrigger { trigger } => {
            visit_trigger_scoped(trigger, scope, visit)?
        }
        ContinuousModification::GrantReplacement { replacement } => {
            visit_replacement_scoped(replacement, scope, visit)?
        }
        ContinuousModification::GrantStaticAbility { definition } => {
            visit_static_scoped(definition, scope, visit)?
        }
        ContinuousModification::CopyValues { values, .. } => {
            visit_copiable_values_scoped(values, scope, visit)?
        }
        // Remaining modifications carry no nested ability/effect carriers.
        // GrantAllActivatedAbilitiesOf / GrantAllTriggeredAbilitiesOf only
        // hold a source `TargetFilter`; the granted abilities/triggers are
        // pulled live from the provider objects at layer collection time,
        // not nested here.
        ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        // CR 707.2c (Metamorphic Alteration): inert parse-time copy marker — no
        // nested ability/effect carrier to walk (the copy grant is the runtime TCE).
        | ContinuousModification::CopyChosen
        | ContinuousModification::SetName { .. }
        | ContinuousModification::SetTextName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::SetDynamicPower { .. }
        | ContinuousModification::SetDynamicToughness { .. }
        | ContinuousModification::SetPowerDynamic { .. }
        | ContinuousModification::SetToughnessDynamic { .. }
        | ContinuousModification::AddDynamicPower { .. }
        | ContinuousModification::AddDynamicToughness { .. }
        | ContinuousModification::AddDynamicKeyword { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::SubstituteTextWord { .. }
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::RetainAllOtherAbilitiesFromSource
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
    ControlFlow::Continue(())
}

pub fn visit_copiable_values<F>(values: &$($mut_)? CopiableValues, visit: &mut F) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_copiable_values_scoped(values, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_copiable_values_scoped<F>(
    values: &$($mut_)? CopiableValues,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    debug_assert_own_resolution_unreachable(scope, "visit_copiable_values_scoped");
    for ability in $arc_iter!(values.abilities) {
        visit_ability_def_scoped(ability, scope, visit)?;
    }
    for trigger in $arc_iter!(values.trigger_definitions) {
        visit_trigger_scoped(trigger, scope, visit)?;
    }
    for static_def in $arc_iter!(values.static_definitions) {
        visit_static_scoped(static_def, scope, visit)?;
    }
    for replacement in $arc_iter!(values.replacement_definitions) {
        visit_replacement_scoped(replacement, scope, visit)?;
    }
    ControlFlow::Continue(())
}

pub fn visit_cost<F>(cost: &$($mut_)? AbilityCost, visit: &mut F) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_cost_scoped(cost, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_cost_scoped<F>(
    cost: &$($mut_)? AbilityCost,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    match cost {
        AbilityCost::EffectCost { effect } => visit_effect_scoped(effect, scope, visit)?,
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for sub in costs {
                visit_cost_scoped(sub, scope, visit)?;
            }
        }
        AbilityCost::PerCounter { base, .. } => visit_cost_scoped(base, scope, visit)?,
        // Remaining costs carry no nested effect/cost carriers.
        AbilityCost::Mana { .. }
        | AbilityCost::ManaDynamic { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Sacrifice(_)
        | AbilityCost::PayLife { .. }
        | AbilityCost::Discard { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        // CR 118.9: a borrowed keyword cost carries no nested effect/cost carrier.
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::GetPlayerCounters { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
    ControlFlow::Continue(())
}

/// Visit `effect` and every effect reachable from its nested ability/effect
/// carriers, pre-order, stopping early on `ControlFlow::Break`. The match is
/// wildcard-free, so a new `Effect` variant forces a decision here (compile
/// error until handled). That guarantee is necessary but not sufficient: a
/// variant wrongly added to the leaf arm, or a new nested *struct field* (which
/// is field access, not a match arm), compiles silently.
/// `printed_cards::tests::walker_covers_every_nested_carrier` and
/// `ai_support::targeted_exchange::tests::predicate_sees_a_fight_in_every_nested_carrier`
/// are the complementary safety nets for those cases — extend both whenever a
/// carrier is added.
pub fn visit_effect<F>(effect: &$($mut_)? Effect, visit: &mut F) -> ControlFlow<()>
where
    F: $($bound)+,
{
    visit_effect_scoped(effect, ResolutionScope::IncludeRegisteredLater, visit)
}

pub(crate) fn visit_effect_scoped<F>(
    effect: &$($mut_)? Effect,
    scope: ResolutionScope,
    visit: &mut F,
) -> ControlFlow<()>
where
    F: $($bound)+,
{
    $on_effect!(visit, effect);
    match effect {
        Effect::Intensify { .. } => {}
        Effect::ApplyPerpetual { .. } => {}
        // CR 614.11: A one-shot draw replacement nests its substitute Effect
        // (Words of Worship/Wilding). Walk it so any conjure name it carries is
        // surfaced (GainLife/Token carry none today, but it is a nested carrier).
        //
        // BOUNDARY CARRIER (CR 614.1 primary / CR 614.15 secondary): this
        // registers a replacement that applies to a LATER event. CR 614.1:
        // replacement effects "watch for a particular event that would happen"
        // and "aren't locked in ahead of time" — they never trigger and never
        // use the stack, so CR 603.3 is NOT the authority here. It is therefore
        // NOT a self-replacement effect (CR 614.15, which scopes those to an
        // effect of a resolving spell or ability replacing "that spell or
        // ability's own effect(s)"), so CR 605.1a's closing carve-out does not
        // reach it and the substitute effect is not part of THIS resolution.
        Effect::CreateDrawReplacement { replacement_effect } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                visit_effect_scoped(replacement_effect, scope, visit)?
            }
        }
        // CR 614.1a: A planeswalk replacement nests its substitute Effect (Fixed
        // Point in Time: chaos ensues). Walk it so any conjure name it carries is
        // surfaced (ChaosEnsues carries none today, but it is a nested carrier).
        //
        // BOUNDARY CARRIER — same reason as `CreateDrawReplacement` above:
        // CR 614.1 primary, not a CR 614.15 self-replacement.
        Effect::CreatePlaneswalkReplacement { replacement_effect } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                visit_effect_scoped(replacement_effect, scope, visit)?
            }
        }
        // Heist exiles a card from an opponent's library at random; it does not
        // name a conjure card, so there is no static face to preload.
        Effect::Heist { .. } | Effect::HeistExile => {}
        // Carries no nested ability/effect carrier. Only named-conjure has a
        // static card name to extract, and that extraction now lives in the
        // caller's visitor closure (`printed_cards::collect_conjure_names`).
        Effect::Conjure { .. } => {}
        // CR 701.42 / CR 712.4b: the melded permanent presents the `result`
        // card's characteristics, but `result` is an outside-the-game third card.
        // Its name is extracted by the caller's visitor closure
        // (`printed_cards::collect_conjure_names`), which seeds it so
        // `build_conjure_registry` preloads its `CardFace` into
        // `card_face_registry`. `source` and `partner` are live battlefield
        // objects the resolver finds by printed identity — they need no registry
        // seeding, and neither field is a nested ability/effect carrier.
        Effect::Meld { .. } => {}
        // A spellbook draft conjures the chosen card, but the list lives on the
        // card face (`metadata.spellbook`), not in the effect — the registry
        // seed collects it directly from the face (see
        // `collect_conjure_names_from_face`), so nothing to gather here.
        Effect::DraftFromSpellbook { .. } => {}
        Effect::TurnFaceUp { .. } => {}
        Effect::TurnFaceDown { .. } => {}
        // Nested-ability carriers — descend.
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            for sub in per_choice_effect {
                visit_nested_ability_def_scoped(sub, scope, visit)?;
            }
            // CR 701.38b: object-pool votes (Council's Judgment, Prime
            // Minister's Cabinet Room) leave `per_choice_effect` empty and
            // carry the sole nested AbilityDefinition in `outcome_template`.
            // Walk it so any conjure name a future object-vote outcome names is
            // surfaced (the current exile-only class carries none).
            if let VoteSubject::Objects {
                outcome_template, ..
            } = subject
            {
                visit_nested_ability_def_scoped(outcome_template, scope, visit)?;
            }
        }
        Effect::ExileTopCardContest { winner_effect } => {
            visit_nested_ability_def_scoped(winner_effect, scope, visit)?;
        }
        Effect::SeparateIntoPiles {
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            visit_nested_ability_def_scoped(chosen_pile_effect, scope, visit)?;
            if let Some(unchosen) = unchosen_pile_effect {
                visit_nested_ability_def_scoped(unchosen, scope, visit)?;
            }
        }
        Effect::RevealFromHand { on_decline, .. } => {
            if let Some(sub) = on_decline {
                visit_nested_ability_def_scoped(sub, scope, visit)?;
            }
        }
        // Only the delayed `effect` is walked; the `condition`'s embedded
        // TriggerDefinition has `execute: None` by construction (it is a matcher,
        // not a payload), so it carries no conjure name.
        //
        // BOUNDARY CARRIER (CR 603.7a): a delayed triggered ability is created
        // now and resolves later as its own ability on the stack (CR 603.3). Its
        // payload is not an instruction THIS ability follows during its own
        // resolution (CR 608.2c), so `OwnResolutionOnly` stops here.
        Effect::CreateDelayedTrigger { effect, .. } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                visit_ability_def_scoped(effect, scope, visit)?
            }
        }
        // BOUNDARY CARRIER (digital-only Alchemy, no CR entry): a boon
        // installs a delayed trigger for its holder that resolves later as
        // its own ability on the stack (CR 603.3 via CR 603.7). Its payload
        // is not an instruction THIS ability follows during its own
        // resolution (CR 608.2c), so `OwnResolutionOnly` stops here — the
        // same treatment as `CreateDelayedTrigger`.
        Effect::CreateBoon { trigger, .. } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                visit_trigger_scoped(trigger, scope, visit)?
            }
        }
        Effect::FlipCoins {
            win_effect,
            lose_effect,
            ..
        } => {
            if let Some(sub) = win_effect {
                visit_nested_ability_def_scoped(sub, scope, visit)?;
            }
            if let Some(sub) = lose_effect {
                visit_nested_ability_def_scoped(sub, scope, visit)?;
            }
        }
        Effect::RollDie { results, .. } => {
            for branch in results {
                visit_nested_ability_def_scoped(&$($mut_)? branch.effect, scope, visit)?;
            }
        }
        Effect::ChooseOneOf { branches, .. } => {
            for branch in branches {
                visit_nested_ability_def_scoped(branch, scope, visit)?;
            }
        }
        // GenericEffect applies static abilities at resolution; their
        // modifications can grant abilities/triggers that themselves conjure.
        // Descend into the granted definitions rather than treating it as a leaf.
        //
        // BOUNDARY CARRIER (CR 611.2): "A continuous effect may be generated by
        // the resolution of a spell or ability." Any `GrantAbility` inside
        // belongs to the AFFECTED object, not to this ability's own resolution.
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                for static_def in static_abilities {
                    visit_static_scoped(static_def, scope, visit)?;
                }
            }
        }
        // Carries a nested ReplacementDefinition whose execute/decline/cost may conjure.
        //
        // BOUNDARY CARRIER (CR 614.1 primary / CR 614.15 secondary): registers a
        // replacement that applies to a later event AND to another object, so it
        // is not a CR 614.15 self-replacement effect of this ability and falls
        // outside CR 605.1a's carve-out.
        Effect::AddTargetReplacement { replacement, .. } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                visit_replacement_scoped(replacement, scope, visit)?
            }
        }
        // Counter's `source_rider` may apply a static to the countered source
        // (LosesAbilities) that grants an ability that conjures. The Destroy
        // rider carries no static.
        //
        // BOUNDARY CARRIER (CR 611.2): the rider is a continuous effect applied
        // to the countered source — another object. NOTE this gates only
        // `source_rider`; `Counter`'s OWN `countered_spell_zone` field is a
        // CR 614.15 self-replacement effect read directly by
        // `Effect::moves_card_to_or_from_library`, not by this walk.
        Effect::Counter { source_rider, .. } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                if let Some(CounterSourceRider::LosesAbilities { static_def, .. }) = source_rider {
                    visit_static_scoped(static_def, scope, visit)?;
                }
            }
        }
        // Tokens and emblems can host granted static/triggered abilities that conjure.
        //
        // BOUNDARY CARRIER (CR 111.1): "A token is a marker used to represent any
        // permanent that isn't represented by a card." The token is a distinct
        // permanent and its granted abilities are the token's, not this one's.
        Effect::Token {
            static_abilities, ..
        } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                for static_def in static_abilities {
                    visit_static_scoped(static_def, scope, visit)?;
                }
            }
        }
        // BOUNDARY CARRIER (CR 114.1): an emblem is a distinct object in the
        // command zone; its abilities are the emblem's, not this ability's.
        Effect::CreateEmblem { statics, triggers } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                for static_def in statics {
                    visit_static_scoped(static_def, scope, visit)?;
                }
                for trigger in triggers {
                    visit_trigger_scoped(trigger, scope, visit)?;
                }
            }
        }
        // Installed Aura grants belong to the returned permanent's later
        // activations/triggers, under the same registered-body boundary as other grants.
        Effect::ReturnAsAura { grants, .. } => {
            if scope == ResolutionScope::IncludeRegisteredLater {
                for modification in grants { visit_continuous_mod_scoped(modification, scope, visit)?; }
            }
        }
        // Leaf effects with no nested ability/effect carrier.
        Effect::StartYourEngines { .. }
        | Effect::ChangeSpeed { .. }
        | Effect::DealDamage { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        // CR 120.1: leaf effect — the source/recipient filters carry no nested
        // ability or effect to walk.
        | Effect::EachDealsDamageEqualToPower { .. }
        | Effect::EachSourceDealsDamage { .. }
        | Effect::Draw { .. }
        | Effect::Pump { .. }
        | Effect::PairWith { .. }
        | Effect::Destroy { .. }
        | Effect::Regenerate { .. }
        | Effect::RemoveAllDamage { .. }
        | Effect::CounterAll { .. }
        | Effect::GainLife { .. }
        | Effect::LoseLife { .. }
        | Effect::ExchangeLifeWithStat { .. }
        | Effect::ExchangeLifeTotals { .. }
        // CR 701.26a/b: all tap/untap scopes are leaf effects here.
        | Effect::SetTapState { .. }
        | Effect::RemoveCounter { .. }
        | Effect::Sacrifice { .. }
        | Effect::DiscardCard { .. }
        | Effect::Mill { .. }
        | Effect::Scry { .. }
        | Effect::PumpAll { .. }
        | Effect::DamageAll { .. }
        | Effect::DamageEachPlayer { .. }
        | Effect::DestroyAll { .. }
        | Effect::ChangeZone { .. }
        | Effect::ChangeZoneAll { .. }
        | Effect::Dig { .. }
        | Effect::GainControl { .. }
        | Effect::GainControlAll { .. }
        | Effect::ControlNextTurn { .. }
        | Effect::Attach { .. }
        | Effect::UnattachAll { .. }
        | Effect::Surveil { .. }
        | Effect::Fight { .. }
        | Effect::Bounce { .. }
        | Effect::BounceAll { .. }
        | Effect::Explore
        | Effect::ExploreAll { .. }
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch { .. }
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::ProliferateTarget { .. }
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Populate
        | Effect::Clash
        | Effect::PhysicalContest { .. }
        | Effect::Behold { .. }
        | Effect::SwitchPT { .. }
        | Effect::CopySpell { .. }
        | Effect::EpicCopy { .. }
        | Effect::CastCopyOfCard { .. }
        | Effect::CopyTokenOf { .. }
        // owner/type_filter are TargetFilters; no nested ability carrier and the
        // copy source comes from the format pool, so this is a leaf for conjure
        // collection.
        | Effect::CreateTokenCopyFromPool { .. }
        | Effect::Myriad
        | Effect::Encore
        | Effect::ExileHaunting { .. }
        | Effect::HideawayConceal { .. }
        | Effect::CopyTokenBlockingAttacker { .. }
        | Effect::BecomeCopy { .. }
        // CR 707.2c (Metamorphic Alteration): filter-only copy choice; no nested
        // ability carrier to walk — a leaf for printed-card collection.
        | Effect::ChoosePermanent { .. }
        | Effect::GainActivatedAbilitiesOfTarget { .. }
        | Effect::ChooseCard { .. }
        | Effect::PutCounter { .. }
        | Effect::PutCounterAll { .. }
        | Effect::MultiplyCounter { .. }
        // Builds its PutCounter/RemoveCounter branches at resolution — carries no
        // static conjure name to preload.
        | Effect::ChooseCounterAdjustment { .. }
        | Effect::DoublePT { .. }
        | Effect::DoublePTAll { .. }
        | Effect::MoveCounters { .. }
        | Effect::Animate { .. }
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        // `Effect::Mana { grants }` is a DELIBERATE no-descend, not an accident
        // of leaf-ness, and all three `ManaSpellGrant` variants are swept:
        //  - `TriggerOnSpend { filter, ability }` is the only one carrying a
        //    nested `AbilityDefinition`. CR 603.3: it is a separate triggered
        //    ability that goes on the stack when the mana is LATER spent, in a
        //    different resolution entirely (Gilanra's `Draw` lives here).
        //  - `AddKeywordUntilEndOfTurn { .. }` is CR 611.2 — a continuous effect
        //    granting a keyword to ANOTHER object (the spell the mana is spent
        //    on) for a duration. It carries no nested ability.
        //  - `CantBeCountered` is a fieldless leaf.
        // Both CR reasons are recorded because a future reader who sees only
        // CR 603.3 will not know why the other two are safe. A "helpful" descent
        // here would break `parser::oracle_tests`'s Path of Ancestry guard,
        // which asserts the delayed-trigger rider does not disqualify the mana
        // ability under CR 605.1a.
        | Effect::Mana { .. }
        | Effect::Discard { .. }
        | Effect::Shuffle { .. }
        | Effect::Transform { .. }
        // CR 710.4: no nested ability carrier and no conjured card name.
        | Effect::FlipPermanent { .. }
        | Effect::SearchLibrary { .. }
        | Effect::SearchOutsideGame { .. }
        // CR 400.11b: brings cards in from outside the game; carries no nested
        // ability and no statically-named card (the pack is generated at
        // resolution), so there is nothing for the conjure walker to seed.
        | Effect::OpenBoosterPack { .. }
        | Effect::RevealHand { .. }
        | Effect::Reveal { .. }
        | Effect::RevealTop { .. }
        | Effect::ExileTop { .. }
        | Effect::ExileFaceDownPile { .. }
        | Effect::TargetOnly { .. }
        | Effect::Choose { .. }
        | Effect::OpponentGuess { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::RevealChosenNumbers { .. }
        | Effect::ChooseDamageSource { .. }
        | Effect::Suspect { .. }
        | Effect::Unsuspect { .. }
        | Effect::Connive { .. }
        | Effect::PhaseOut { .. }
        | Effect::PhaseIn { .. }
        | Effect::ForceBlock { .. }
        | Effect::ForceAttack { .. }
        | Effect::SolveCase
        | Effect::BecomePrepared { .. }
        | Effect::BecomeUnprepared { .. }
        | Effect::BecomeSaddled { .. }
        | Effect::BecomeBlocked { .. }
        | Effect::SetClassLevel { .. }
        | Effect::AddRestriction { .. }
        | Effect::ReduceNextSpellCost { .. }
        | Effect::GrantNextSpellAbility { .. }
        | Effect::AddPendingETBCounters { .. }
        | Effect::AddPendingEntersModifications { .. }
        | Effect::PayCost { .. }
        | Effect::CastFromZone { .. }
        | Effect::FreeCastFromZones { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::PreventDamage { .. }
        | Effect::LoseTheGame { .. }
        | Effect::WinTheGame { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::ArrangePlanarDeckTop { .. }
        | Effect::Planeswalk
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptions { .. }
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::CrankContraptions { .. }
        | Effect::ReassembleContraption { .. }
        | Effect::AssembleContraptionOnSprocket { .. }
        | Effect::ReassembleContraptionOnSprocket { .. }
        | Effect::PutSticker { .. }
        | Effect::ApplySticker { .. }
        | Effect::ProcessRadCounters
        | Effect::GrantCastingPermission { .. }
        | Effect::ChooseFromZone { .. }
        | Effect::RememberCard { .. }
        | Effect::NoteManaSpent
        | Effect::NoteNumber { .. }
        | Effect::ForEachCategory { .. }
        | Effect::ChooseObjectsIntoTrackedSet { .. }
        | Effect::ChooseAndSacrificeRest { .. }
        | Effect::EachPlayerCopyChosen { .. }
        | Effect::Exploit { .. }
        | Effect::GainEnergy { .. }
        | Effect::GivePlayerCounter { .. }
        | Effect::LoseAllPlayerCounters { .. }
        | Effect::ExileFromTopUntil { .. }
        | Effect::RevealUntil { .. }
        | Effect::Discover { .. }
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::PutAtLibraryPosition { .. }
        | Effect::ChooseDrawnThisTurnPayOrTopdeck { .. }
        | Effect::PutOnTopOrBottom { .. }
        | Effect::GiftDelivery { .. }
        | Effect::Goad { .. }
        | Effect::GoadAll { .. }
        | Effect::Detain { .. }
        | Effect::SetRoomDoorLock { .. }
        | Effect::ExchangeControl { .. }
        | Effect::ChangeTargets { .. }
        | Effect::Manifest { .. }
        | Effect::ManifestDread
        | Effect::Cloak { .. }
        | Effect::ExtraTurn { .. }
        | Effect::GrantExtraLoyaltyActivations { .. }
        | Effect::SkipNextTurn { .. }
        | Effect::SkipNextStep { .. }
        | Effect::AdditionalPhase { .. }
        | Effect::Double { .. }
        | Effect::ModifyAnnouncedX { .. }
        | Effect::RuntimeHandled { .. }
        | Effect::Incubate { .. }
        | Effect::Amass { .. }
        | Effect::EmpowerJace { .. }
        | Effect::Monstrosity { .. }
        | Effect::Renown { .. }
        | Effect::Bolster { .. }
        | Effect::Adapt { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::CompletePlayerAction { .. }
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::Endure { .. }
        | Effect::BlightEffect { .. }
        | Effect::Seek { .. }
        | Effect::SetLifeTotal { .. }
        | Effect::SetDayNight { .. }
        | Effect::GiveControl { .. }
        | Effect::RemoveFromCombat { .. }
        | Effect::CreateDamageReplacement { .. }
        | Effect::CombineHost { .. }
        | Effect::ChooseAugmentAndCombineWithHost { .. }
        | Effect::Specialize
        // CR 608.2d + CR 122.1: counter-kind choice / consume carry no conjure names.
        | Effect::ChooseCounterKind { .. }
        | Effect::PutChosenCounter { .. }
        | Effect::ReproduceEventCounters { .. }
        | Effect::Unimplemented { .. } => {}
    }
    ControlFlow::Continue(())
}
    };
}

macro_rules! visit_each_effect {
    ($visit:ident, $effect:ident) => {
        $visit($effect)?
    };
}
macro_rules! skip_effects {
    ($visit:ident, $effect:ident) => {};
}
macro_rules! skip_nodes {
    ($visit:ident, $kind:ident, $node:ident) => {};
}
macro_rules! visit_each_node {
    ($visit:ident, $kind:ident, $node:ident) => {
        $visit(DefinitionNodeMut::$kind(&mut *$node))?
    };
}
macro_rules! shared_iter {
    ($vec:expr) => {
        $vec.iter()
    };
}
macro_rules! exclusive_iter {
    ($vec:expr) => {
        std::sync::Arc::make_mut(&mut $vec).iter_mut()
    };
}

define_walk!([]; [FnMut(&Effect) -> ControlFlow<()>]; visit_each_effect; skip_nodes; shared_iter; true);

macro_rules! visit_each_effect_mut {
    ($visit:ident, $effect:ident) => {
        $visit(&mut *$effect)?
    };
}

/// Exclusive effect-axis twin; uses the same typed carrier and scope decisions.
#[allow(dead_code)]
pub(crate) mod effects_mut {
    use super::*;
    define_walk!([mut]; [FnMut(&mut Effect) -> ControlFlow<()>]; visit_each_effect_mut; skip_nodes; exclusive_iter; true);
}

/// A definition node surfaced by the exclusive walk.
pub enum DefinitionNodeMut<'a> {
    Ability(&'a mut AbilityDefinition),
    Trigger(&'a mut TriggerDefinition),
    Static(&'a mut StaticDefinition),
    Replacement(&'a mut ReplacementDefinition),
}

/// The exclusive walk: visits every definition node the shared walk descends.
pub mod nodes_mut {
    use super::*;

    define_walk!([mut]; [FnMut(DefinitionNodeMut<'_>) -> ControlFlow<()>]; skip_effects; visit_each_node; exclusive_iter; true);
}

macro_rules! visit_each_node_ref {
    ($visit:ident, $kind:ident, $node:ident) => {
        $visit(DefinitionNode::$kind(&*$node))?
    };
}

/// A definition node surfaced by the shared node walk.
pub enum DefinitionNode<'a> {
    Ability(&'a AbilityDefinition),
    Trigger(&'a TriggerDefinition),
    Static(&'a StaticDefinition),
    Replacement(&'a ReplacementDefinition),
}

/// The shared twin of [`nodes_mut`].
pub mod nodes {
    use super::*;

    define_walk!([]; [FnMut(DefinitionNode<'_>) -> ControlFlow<()>]; skip_effects; visit_each_node_ref; shared_iter; true);
}

/// The same carrier list, stopping at each invoked definition instead of
/// flattening its descendants. Result-process validation uses this boundary
/// to distinguish transparent readers from independently producing children.
#[allow(dead_code)] // The shared generator also emits sibling traversal entry points.
pub(crate) mod direct_nodes {
    use super::*;
    define_walk!([]; [FnMut(DefinitionNode<'_>) -> ControlFlow<()>]; skip_effects; visit_each_node_ref; shared_iter; false);
}

/// Exclusive twin of the direct-node walk, preserving the same scope gates.
#[allow(dead_code)] // The generator emits the same sibling entry points.
pub(crate) mod direct_nodes_mut {
    use super::*;
    define_walk!([mut]; [FnMut(DefinitionNodeMut<'_>) -> ControlFlow<()>]; skip_effects; visit_each_node; exclusive_iter; false);
}

/// State-owned programs keep arena handles in definitions, including definitions
/// embedded in costs and frozen executable input. The callback deliberately has
/// no access to GameState; the caller owns lookup and graph validation.
pub(crate) fn visit_resolved_replacements(
    root: &crate::types::ability::ResolvedAbility,
    visit: &mut impl FnMut(&ReplacementDefinition),
) {
    use crate::types::ability::{
        AdditionalCost, CostPaidObjectRecord, ParsedCondition, QuantityRef,
    };
    use crate::types::keywords::*;

    fn keyword(value: &Keyword, visit: &mut impl FnMut(&ReplacementDefinition)) {
        let cost = match value {
            Keyword::CumulativeUpkeep(cost)
            | Keyword::Escalate(cost)
            | Keyword::Bestow(BestowCost::NonMana(cost))
            | Keyword::Embalm(EmbalmCost::NonMana(cost))
            | Keyword::Eternalize(EternalizeCost::NonMana(cost))
            | Keyword::Cycling(CyclingCost::NonMana(cost))
            | Keyword::Flashback(FlashbackCost::NonMana(cost))
            | Keyword::Escape(EscapeCost::NonMana(cost))
            | Keyword::Evoke(EvokeCost::NonMana(cost))
            | Keyword::Blitz(BlitzCost::NonMana(cost))
            | Keyword::Buyback(BuybackCost::NonMana(cost))
            | Keyword::Echo(EchoCost::NonMana(cost))
            | Keyword::Morph(MorphCost::NonMana(cost))
            | Keyword::Entwine(EntwineCost::NonMana(cost)) => Some(cost),
            _ => None,
        };
        if let Some(cost) = cost {
            walk_cost(cost, visit);
        }
        match value {
            Keyword::Enchant(value)
            | Keyword::Craft {
                materials: value, ..
            }
            | Keyword::Protection(ProtectionTarget::Filter(value)) => filter(value, visit),
            Keyword::Affinity(value) => {
                for property in &value.properties {
                    property.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
                }
            }
            Keyword::Emerge(value) => filter(&value.sacrifice_filter, visit),
            Keyword::Mobilize(value) | Keyword::Firebending(value) => quantity(value, visit),
            Keyword::Crew {
                once_per_turn: Some(value),
                ..
            } => activation(value, visit),
            Keyword::Disguise(DisguiseCost::Reduced { reduction, .. }) => {
                quantity(&reduction.count, visit);
                if let Some(value) = &reduction.condition {
                    parsed_condition(value, visit);
                }
            }
            Keyword::Ward(value) => ward(value, visit),
            _ => {}
        }
    }
    fn ward(value: &WardCost, visit: &mut impl FnMut(&ReplacementDefinition)) {
        match value {
            WardCost::Sacrifice { filter: value, .. }
            | WardCost::Discard {
                filter: Some(value),
                ..
            } => filter(value, visit),
            WardCost::Compound(values) => {
                for value in values {
                    ward(value, visit);
                }
            }
            _ => {}
        }
    }
    fn activation(
        value: &crate::types::ability::ActivationRestriction,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        if let crate::types::ability::ActivationRestriction::RequiresCondition {
            condition: Some(value),
        } = value
        {
            parsed_condition(value, visit);
        }
    }
    fn parsed_condition(value: &ParsedCondition, visit: &mut impl FnMut(&ReplacementDefinition)) {
        match value {
            ParsedCondition::QuantityComparison { lhs, rhs, .. } => {
                quantity(lhs, visit);
                quantity(rhs, visit);
            }
            ParsedCondition::QuantityVsEachOpponent { lhs, rhs, .. } => {
                reference(lhs, visit);
                reference(rhs, visit);
            }
            ParsedCondition::SourceLacksKeyword { keyword: value }
            | ParsedCondition::ControlsCreatureWithKeyword { keyword: value, .. } => {
                keyword(value, visit)
            }
            ParsedCondition::YouAttackedWithAtLeast {
                filter: Some(value),
                ..
            }
            | ParsedCondition::YouCastSpellThisTurn {
                filter: Some(value),
            }
            | ParsedCondition::BattlefieldEntriesThisTurn { filter: value, .. }
            | ParsedCondition::SpellTargetsFilter { filter: value } => filter(value, visit),
            ParsedCondition::PlayerCountAtLeast { filter: value, .. } => {
                value.for_each_nested_keyword_payload(&mut |value| keyword(value, visit))
            }
            ParsedCondition::Not { condition } => parsed_condition(condition, visit),
            ParsedCondition::And { conditions } | ParsedCondition::Or { conditions } => {
                for value in conditions {
                    parsed_condition(value, visit);
                }
            }
            _ => {}
        }
    }
    fn reference(reference: &QuantityRef, visit: &mut impl FnMut(&ReplacementDefinition)) {
        reference.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
    }
    fn filter(value: &TargetFilter, visit: &mut impl FnMut(&ReplacementDefinition)) {
        value.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
    }
    fn quantity(
        value: &crate::types::ability::QuantityExpr,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        value.for_each_ref(&mut |value| reference(value, visit));
    }
    fn cost_leaves(value: &AbilityCost, visit: &mut impl FnMut(&ReplacementDefinition)) {
        value.for_each_predicate_keyword_payload(&mut |value| keyword(value, visit));
        value.for_each_resolution_quantity_expr(&mut |value| quantity(value, visit));
    }
    // Keep both axes of the existing shared walk: node callbacks expose the
    // definitions, and effect callbacks expose its deliberately opaque leaves.
    fn walk_cost(value: &AbilityCost, visit: &mut impl FnMut(&ReplacementDefinition)) {
        cost_leaves(value, visit);
        let _ = nodes::visit_cost(value, &mut |node| replacement_node(node, visit));
        let _ = visit_cost(value, &mut |value| {
            effect_leaves(value, visit);
            ControlFlow::Continue(())
        });
    }
    fn walk_ability(value: &AbilityDefinition, visit: &mut impl FnMut(&ReplacementDefinition)) {
        let _ = nodes::visit_ability_def(value, &mut |node| replacement_node(node, visit));
        let _ = visit_ability_def(value, &mut |value| {
            effect_leaves(value, visit);
            ControlFlow::Continue(())
        });
    }
    fn walk_trigger(value: &TriggerDefinition, visit: &mut impl FnMut(&ReplacementDefinition)) {
        let _ = nodes::visit_trigger(value, &mut |node| replacement_node(node, visit));
        let _ = visit_trigger(value, &mut |value| {
            effect_leaves(value, visit);
            ControlFlow::Continue(())
        });
    }
    fn walk_static(value: &StaticDefinition, visit: &mut impl FnMut(&ReplacementDefinition)) {
        let _ = nodes::visit_static(value, &mut |node| replacement_node(node, visit));
        let _ = visit_static(value, &mut |value| {
            effect_leaves(value, visit);
            ControlFlow::Continue(())
        });
    }
    fn walk_replacement(
        value: &ReplacementDefinition,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        let _ = nodes::visit_replacement(value, &mut |node| replacement_node(node, visit));
        let _ = visit_replacement(value, &mut |value| {
            effect_leaves(value, visit);
            ControlFlow::Continue(())
        });
    }
    fn walk_modification(
        value: &ContinuousModification,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        let _ = nodes::visit_continuous_mod(value, &mut |node| replacement_node(node, visit));
        let _ = visit_continuous_mod(value, &mut |value| {
            effect_leaves(value, visit);
            ControlFlow::Continue(())
        });
        modification_leaves(value, visit);
    }
    fn trigger_source(
        value: &crate::types::game_state::TriggerSourceContext,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        for value in value.lki.keywords.iter().chain(&value.cast_spell_keywords) {
            keyword(value, visit);
        }
        for entry in &value.trigger_entries {
            walk_trigger(&entry.definition, visit);
        }
        if let Some(value) = &value.cast_cost_paid_object {
            for value in &value.lki.keywords {
                keyword(value, visit);
            }
        }
        for value in &value.mana_spent_source_snapshots {
            for value in &value.lki.keywords {
                keyword(value, visit);
            }
        }
    }
    fn event(
        value: &crate::types::events::GameEvent,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        use crate::types::events::{BoundActivationCost, GameEvent};
        match value {
            GameEvent::AbilityActivated {
                departed_source_lki,
                activation_cost,
                ..
            } => {
                if let Some(value) = departed_source_lki {
                    for value in &value.keywords {
                        keyword(value, visit);
                    }
                }
                if let BoundActivationCost::Cost(value) = activation_cost {
                    walk_cost(value, visit);
                }
            }
            GameEvent::ZoneChanged { record, .. } => {
                for value in &record.keywords {
                    keyword(value, visit);
                }
                for value in &record.trigger_definitions {
                    walk_trigger(&value.definition, visit);
                }
                if let Some(value) = &record.trigger_source_context {
                    trigger_source(value, visit);
                }
            }
            GameEvent::CreatureEnlisted {
                tapped_snapshot, ..
            } => {
                for value in &tapped_snapshot.lki.keywords {
                    keyword(value, visit);
                }
            }
            GameEvent::SagaChapterAbilityResolved { saga, .. } => trigger_source(saga, visit),
            GameEvent::EffectResolved {
                subject: Some(value),
                ..
            } => {
                for value in &value.keywords {
                    keyword(value, visit);
                }
            }
            _ => {}
        }
    }
    fn delayed_condition(
        value: &crate::types::ability::DelayedTriggerCondition,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        use crate::types::ability::DelayedTriggerCondition;
        match value {
            DelayedTriggerCondition::WhenDies { filter: value }
            | DelayedTriggerCondition::WhenLeavesPlayFiltered { filter: value }
            | DelayedTriggerCondition::WhenEntersBattlefield { filter: value }
            | DelayedTriggerCondition::WhenDiesOrExiled { filter: value } => filter(value, visit),
            DelayedTriggerCondition::WheneverEvent { trigger, .. } => walk_trigger(trigger, visit),
            DelayedTriggerCondition::WhenNextEvent {
                trigger,
                or_trigger,
                ..
            } => {
                walk_trigger(trigger, visit);
                if let Some(trigger) = or_trigger {
                    walk_trigger(trigger, visit);
                }
            }
            _ => {}
        }
    }
    fn trigger_condition(
        value: &crate::types::ability::TriggerCondition,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        use crate::types::ability::TriggerCondition;
        match value {
            TriggerCondition::QuantityComparison { lhs, rhs, .. } => {
                quantity(lhs, visit);
                quantity(rhs, visit);
            }
            TriggerCondition::QuantityQuality {
                quantity: value, ..
            } => quantity(value, visit),
            TriggerCondition::ControlsType { filter: value }
            | TriggerCondition::ControlCount { filter: value, .. }
            | TriggerCondition::ControlsNone { filter: value }
            | TriggerCondition::DefendingPlayerControlsNone { filter: value }
            | TriggerCondition::ZoneChangeObjectMatchesFilter { filter: value, .. }
            | TriggerCondition::SourceMatchesFilter { filter: value }
            | TriggerCondition::EventDamageSourceMatchesFilter { filter: value }
            | TriggerCondition::EventObjectMatchesFilter { filter: value }
            | TriggerCondition::TriggeringSpellTargetsFilter { filter: value }
            | TriggerCondition::TriggeringSpellMatchesFilter { filter: value }
            | TriggerCondition::DealtDamageThisTurnBySource { source: value }
            | TriggerCondition::MinCoAttackers {
                filter: Some(value),
                ..
            }
            | TriggerCondition::CastSpellThisTurn {
                filter: Some(value),
            } => filter(value, visit),
            TriggerCondition::DuringPlayersTurn { player } => {
                player.for_each_nested_keyword_payload(&mut |value| keyword(value, visit))
            }
            TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
                for value in conditions {
                    trigger_condition(value, visit);
                }
            }
            TriggerCondition::Not { condition } | TriggerCondition::EventTime { condition } => {
                trigger_condition(condition, visit)
            }
            _ => {}
        }
    }
    fn static_condition(
        value: &crate::types::ability::StaticCondition,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        use crate::types::ability::StaticCondition;
        match value {
            StaticCondition::QuantityComparison { lhs, rhs, .. } => {
                quantity(lhs, visit);
                quantity(rhs, visit);
            }
            StaticCondition::QuantityQuality {
                quantity: value, ..
            } => quantity(value, visit),
            StaticCondition::IsPresent {
                filter: Some(value),
            }
            | StaticCondition::DefendingPlayerControls { filter: value }
            | StaticCondition::SourceMatchesFilter { filter: value }
            | StaticCondition::TopOfLibraryMatches { filter: value }
            | StaticCondition::RecipientMatchesFilter { filter: value } => filter(value, visit),
            StaticCondition::Not { condition } => static_condition(condition, visit),
            StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
                for value in conditions {
                    static_condition(value, visit);
                }
            }
            StaticCondition::UnlessPay {
                scaling:
                    crate::types::ability::UnlessPayScaling::PerQuantityRef { quantity: value }
                    | crate::types::ability::UnlessPayScaling::PerAffectedAndQuantityRef {
                        quantity: value,
                    }
                    | crate::types::ability::UnlessPayScaling::PerAffectedWithRef { quantity: value },
                ..
            } => reference(value, visit),
            _ => {}
        }
    }
    fn replacement_condition(
        value: &crate::types::ability::ReplacementCondition,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        use crate::types::ability::ReplacementCondition;
        match value {
            ReplacementCondition::OnlyIfQuantity { lhs, rhs, .. }
            | ReplacementCondition::UnlessQuantity { lhs, rhs, .. } => {
                quantity(lhs, visit);
                quantity(rhs, visit);
            }
            ReplacementCondition::UnlessControlsOtherLeq { filter: value, .. } => {
                filter(&TargetFilter::Typed(value.clone()), visit)
            }
            ReplacementCondition::UnlessControlsMatching { filter: value }
            | ReplacementCondition::UnlessControlsCountMatching { filter: value, .. }
            | ReplacementCondition::IfControlsMatching { filter: value, .. }
            | ReplacementCondition::DealtDamageThisTurnBySource { source: value } => {
                filter(value, visit)
            }
            ReplacementCondition::And { conditions } => {
                for value in conditions {
                    replacement_condition(value, visit);
                }
            }
            _ => {}
        }
    }
    fn replacement_node(
        node: DefinitionNode<'_>,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) -> ControlFlow<()> {
        match node {
            DefinitionNode::Replacement(definition) => {
                visit(definition);
                if let Some(value) = &definition.selected_damage_source {
                    trigger_source(value, visit);
                }
                if let Some(value) = &definition.entry_payment_max {
                    quantity(value, visit);
                }
                match &definition.damage_modification {
                    Some(crate::types::ability::DamageModification::Plus { value })
                    | Some(crate::types::ability::DamageModification::PreventionMinus {
                        value:
                            crate::types::ability::PreventionFormula::Quantity { quantity: value },
                    }) => quantity(value, visit),
                    _ => {}
                }
                for value in [
                    &definition.valid_card,
                    &definition.damage_source_filter,
                    &definition.redirect_target,
                ]
                .into_iter()
                .flatten()
                {
                    filter(value, visit);
                }
                if let Some(value) = &definition.condition {
                    replacement_condition(value, visit);
                }
                if let ReplacementMode::MayCost { cost, .. } = &definition.mode {
                    cost_leaves(cost, visit);
                }
                for spec in definition
                    .additional_token_spec
                    .as_deref()
                    .into_iter()
                    .chain(definition.ensure_token_specs.iter().flatten())
                {
                    for value in &spec.characteristics.keywords {
                        keyword(value, visit);
                    }
                    for definition in &spec.static_abilities {
                        walk_static(definition, visit);
                    }
                    if let Some(window) = &spec.sacrifice_at {
                        duration(window, visit);
                    }
                }
            }
            DefinitionNode::Ability(definition) => {
                definition.for_each_direct_quantity_ref(&mut |value| reference(value, visit));
                if let Some(condition) = &definition.condition {
                    condition.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
                }
                if let Some(window) = &definition.duration {
                    duration(window, visit);
                }
                if let Some(value) = &definition.cost {
                    cost_leaves(value, visit);
                }
                if let Some(value) = &definition.unless_pay {
                    cost_leaves(&value.cost, visit);
                    filter(&value.payer, visit);
                }
                for value in [&definition.player_scope].into_iter().flatten() {
                    value.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
                }
                for value in [&definition.target_chooser, &definition.optional_player]
                    .into_iter()
                    .flatten()
                {
                    filter(value, visit);
                }
                for value in &definition.activation_restrictions {
                    activation(value, visit);
                }
                if let Some(value) = &definition.activator_filter {
                    value.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
                }
                if let Some(value) = &definition.cost_reduction {
                    quantity(&value.count, visit);
                    if let Some(value) = &value.condition {
                        parsed_condition(value, visit);
                    }
                }
                if let Some(value) = &definition.multi_target {
                    quantity(&value.min, visit);
                    if let Some(value) = &value.max {
                        quantity(value, visit);
                    }
                }
                if let Some(value) = &definition.announced_x {
                    quantity(value, visit);
                }
                for value in &definition.target_constraints {
                    if let crate::types::game_state::TargetSelectionConstraint::TotalManaValue {
                        value,
                        ..
                    } = value
                    {
                        quantity(value, visit);
                    }
                }
                if let Some((_, value)) = &definition.reads_return_result {
                    filter(&value.noun, visit);
                }
                if let Some(crate::types::ability::RepeatContinuation::WhileCondition {
                    condition,
                    ..
                }) = &definition.repeat_until
                {
                    condition.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
                }
            }
            DefinitionNode::Static(definition) => {
                mode(&definition.mode, visit);
                if let Some(value) = &definition.affected {
                    filter(value, visit);
                }
                if let Some(value) = &definition.condition {
                    static_condition(value, visit);
                }
                if let Some(value) = &definition.per_player_condition {
                    parsed_condition(value, visit);
                }
                for modification in &definition.modifications {
                    modification_leaves(modification, visit);
                }
            }
            DefinitionNode::Trigger(definition) => {
                for value in [
                    &definition.valid_card,
                    &definition.valid_target,
                    &definition.valid_subject_player,
                    &definition.valid_source,
                ]
                .into_iter()
                .flatten()
                {
                    filter(value, visit);
                }
                for clause in &definition.zone_change_clauses {
                    if let Some(value) = &clause.valid_card {
                        filter(value, visit);
                    }
                }
                if let Some(value) = &definition.condition {
                    trigger_condition(value, visit);
                }
                if let Some(value) = &definition.attack_cost_condition {
                    static_condition(value, visit);
                }
                if let Some(crate::types::ability::TriggerConstraint::NthSpellThisTurn {
                    filter: Some(value),
                    ..
                }) = &definition.constraint
                {
                    filter(value, visit);
                }
                if let Some(value) = &definition.unless_pay {
                    filter(&value.payer, visit);
                    cost_leaves(&value.cost, visit);
                }
            }
        }
        ControlFlow::Continue(())
    }
    fn duration(
        value: &crate::types::ability::Duration,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        match value {
            crate::types::ability::Duration::UntilEvent { event } => walk_trigger(event, visit),
            crate::types::ability::Duration::ForAsLongAs { condition } => {
                static_condition(condition, visit)
            }
            crate::types::ability::Duration::UntilNextStepOf { player, .. } => {
                player.for_each_nested_keyword_payload(&mut |value| keyword(value, visit))
            }
            _ => {}
        }
    }
    fn mode(
        value: &crate::types::statics::StaticMode,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        use crate::types::statics::{
            BlockExceptionKind, CostPaymentProhibition, HandSizeModification, RequiredDefender,
            StaticMode,
        };
        let cost = match value {
            StaticMode::CastWithAlternativeCost { cost, .. }
            | StaticMode::AlternativeKeywordCost { cost, .. }
            | StaticMode::TopOfLibraryCastPermission {
                alt_cost: Some(cost),
                ..
            }
            | StaticMode::ImposeAdditionalCost { cost, .. } => Some(cost),
            StaticMode::GraveyardCastPermission {
                extra_cost: Some(cost),
                ..
            }
            | StaticMode::ExileCastPermission {
                extra_cost: Some(cost),
                ..
            } => Some(&cost.cost),
            _ => None,
        };
        if let Some(cost) = cost {
            walk_cost(cost, visit);
        }
        match value {
            StaticMode::CastWithKeyword { keyword: value }
            | StaticMode::CantHaveKeyword { keyword: value } => keyword(value, visit),
            StaticMode::CantBeActivated {
                source_filter: value,
                ..
            }
            | StaticMode::CantBeBlockedBy { filter: value }
            | StaticMode::AttachmentRestriction { filter: value }
            | StaticMode::BlockRestriction { filter: value }
            | StaticMode::MaxUntapPerType { filter: value, .. }
            | StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::Quality(value),
            }
            | StaticMode::PlayerProtection(ProtectionTarget::Filter(value))
            | StaticMode::CantPayCost {
                cost: CostPaymentProhibition::Sacrifice { filter: value },
                ..
            }
            | StaticMode::MustBeBlocked { by: Some(value) }
            | StaticMode::MustBeBlockedByAll {
                blockers: Some(value),
            }
            | StaticMode::PerTurnCastLimit {
                spell_filter: Some(value),
                ..
            } => filter(value, visit),
            StaticMode::MustAttackDefender {
                defender: RequiredDefender::Matching { filter: value },
            } => value.for_each_nested_keyword_payload(&mut |value| keyword(value, visit)),
            StaticMode::ModifyCost {
                spell_filter,
                dynamic_count,
                ..
            } => {
                if let Some(value) = spell_filter {
                    filter(value, visit);
                }
                if let Some(value) = dynamic_count {
                    reference(value, visit);
                }
            }
            StaticMode::ReduceAbilityCost {
                dynamic_count,
                targets,
                activator,
                ..
            } => {
                if let Some(value) = targets {
                    filter(value, visit);
                }
                if let Some(value) = dynamic_count {
                    reference(value, visit);
                }
                if let Some(value) = activator {
                    value.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
                }
            }
            StaticMode::ImposeAdditionalCost {
                spell_filter: Some(value),
                ..
            } => filter(value, visit),
            StaticMode::SuppressTriggers {
                source_filter,
                trigger_source_filter,
                ..
            } => {
                filter(source_filter, visit);
                if let Some(value) = trigger_source_filter {
                    filter(value, visit);
                }
            }
            StaticMode::SpendManaAsAnyColor {
                spell_filter,
                activation_source_filter,
                ..
            } => {
                for value in [spell_filter, activation_source_filter]
                    .into_iter()
                    .flatten()
                {
                    filter(value, visit);
                }
            }
            StaticMode::MaximumHandSize {
                modification: HandSizeModification::EqualTo(value),
            } => quantity(value, visit),
            _ => {}
        }
    }
    fn modification_leaves(
        value: &ContinuousModification,
        visit: &mut impl FnMut(&ReplacementDefinition),
    ) {
        match value {
            ContinuousModification::AddKeyword { keyword: value }
            | ContinuousModification::RemoveKeyword { keyword: value } => keyword(value, visit),
            ContinuousModification::AddStaticMode { mode: value } => mode(value, visit),
            ContinuousModification::SetDynamicPower { value }
            | ContinuousModification::SetDynamicToughness { value }
            | ContinuousModification::SetPowerDynamic { value }
            | ContinuousModification::SetToughnessDynamic { value }
            | ContinuousModification::AddDynamicPower { value }
            | ContinuousModification::AddDynamicToughness { value }
            | ContinuousModification::AddDynamicKeyword { value, .. }
            | ContinuousModification::AddCounterOnEnter { count: value, .. } => {
                quantity(value, visit)
            }
            ContinuousModification::GrantAllActivatedAbilitiesOf { source, cap, .. } => {
                filter(source, visit);
                if let Some(value) = cap {
                    activation(value, visit);
                }
            }
            ContinuousModification::GrantAllTriggeredAbilitiesOf { source } => {
                filter(source, visit)
            }
            ContinuousModification::CopyValues { values, .. } => {
                for value in &values.keywords {
                    keyword(value, visit);
                }
                if let Some(face) = &values.prepare_face {
                    for replacement in face.replacement_definitions.iter_all() {
                        walk_replacement(replacement, visit);
                    }
                    for definition in &face.abilities {
                        walk_ability(definition, visit);
                    }
                    for definition in face.static_definitions.iter_all() {
                        walk_static(definition, visit);
                    }
                    for definition in face.trigger_definitions.iter_all() {
                        walk_trigger(definition, visit);
                    }
                    for value in &face.keywords {
                        keyword(value, visit);
                    }
                    if let Some(value) = &face.additional_cost {
                        match value {
                            AdditionalCost::Optional { cost, .. }
                            | AdditionalCost::Required(cost) => walk_cost(cost, visit),
                            AdditionalCost::Kicker { costs, .. } => {
                                for value in costs {
                                    walk_cost(value, visit);
                                }
                            }
                            AdditionalCost::Choice(first, second) => {
                                walk_cost(first, visit);
                                walk_cost(second, visit);
                            }
                        }
                    }
                    for option in &face.casting_options {
                        if let Some(value) = &option.cost {
                            walk_cost(value, visit);
                        }
                        if let Some(value) = &option.condition {
                            parsed_condition(value, visit);
                        }
                    }
                    for value in &face.casting_restrictions {
                        if let crate::types::ability::CastingRestriction::RequiresCondition {
                            condition: Some(value),
                        } = value
                        {
                            parsed_condition(value, visit);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    // These are the existing typed leaves of the shared effect walk, rather
    // than a second list of its ordinary ability/trigger/cost carriers.
    fn effect_leaves(effect: &Effect, visit: &mut impl FnMut(&ReplacementDefinition)) {
        use crate::types::ability::{PerpetualGrantModification, PerpetualModification};
        match effect {
            Effect::CopySpell {
                additional_modifications,
                ..
            }
            | Effect::BecomeCopy {
                additional_modifications,
                ..
            }
            | Effect::CopyTokenOf {
                additional_modifications,
                ..
            }
            | Effect::EachPlayerCopyChosen {
                copy_modifications: additional_modifications,
                ..
            } => {
                for modification in additional_modifications {
                    walk_modification(modification, visit);
                }
            }
            Effect::ApplyPerpetual {
                modification: PerpetualModification::GrantAbility { modifications },
                ..
            } => {
                for modification in modifications {
                    match modification {
                        PerpetualGrantModification::GrantAbility { definition } => {
                            walk_ability(definition, visit)
                        }
                        PerpetualGrantModification::GrantTrigger { trigger } => {
                            walk_trigger(trigger, visit)
                        }
                        PerpetualGrantModification::AddStaticMode { mode: value } => {
                            mode(value, visit)
                        }
                        PerpetualGrantModification::AddKeyword { keyword: value } => {
                            keyword(value, visit)
                        }
                    }
                }
            }
            Effect::Mana { grants, .. } => {
                for grant in grants {
                    match grant {
                        crate::types::mana::ManaSpellGrant::TriggerOnSpend {
                            filter: value,
                            ability,
                        } => {
                            filter(value, visit);
                            walk_ability(ability, visit);
                        }
                        crate::types::mana::ManaSpellGrant::AddKeywordUntilEndOfTurn {
                            keyword: value,
                            duration: window,
                            ..
                        } => {
                            keyword(value, visit);
                            duration(window, visit);
                        }
                        crate::types::mana::ManaSpellGrant::CantBeCountered { filter: value } => {
                            filter(value, visit)
                        }
                        crate::types::mana::ManaSpellGrant::EntersWithCounters {
                            filter: value,
                            count,
                            ..
                        } => {
                            filter(value, visit);
                            quantity(count, visit);
                        }
                    }
                }
            }
            Effect::ApplyPerpetual {
                modification:
                    PerpetualModification::ModifyPowerToughness { keywords, .. }
                    | PerpetualModification::GrantKeywords { keywords }
                    | PerpetualModification::Become { keywords, .. },
                ..
            } => {
                for value in keywords {
                    keyword(value, visit);
                }
            }
            Effect::AddPendingEntersModifications { rider } => {
                for modification in &rider.modifications {
                    walk_modification(modification, visit);
                }
                if let Some(value) = &rider.cast_filter {
                    filter(value, visit);
                }
            }
            Effect::GrantNextSpellAbility {
                modifier: crate::types::game_state::NextSpellModifier::HasKeyword { keyword: value },
                ..
            } => keyword(value, visit),
            Effect::ReturnAsAura {
                grants,
                enchant_filter,
            } => {
                filter(enchant_filter, visit);
                for value in grants {
                    modification_leaves(value, visit);
                }
            }
            Effect::CreateDelayedTrigger { condition, .. }
            | Effect::ExileResolvingSpellInsteadOfGraveyard {
                on_exile:
                    Some(crate::types::ability::ExiledSpellRider::ReturnTo {
                        timing: condition, ..
                    }),
            } => delayed_condition(condition, visit),
            Effect::PayCost { cost, .. } => walk_cost(cost, visit),
            Effect::CastFromZone {
                alt_ability_cost,
                constraint,
                ..
            } => {
                if let Some(value) = alt_ability_cost {
                    walk_cost(value, visit);
                }
                if let Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                    value,
                    ..
                }) = constraint
                {
                    quantity(value, visit);
                }
            }
            Effect::Choose {
                choice_type: crate::types::ability::ChoiceType::Keyword { options, .. },
                ..
            } => {
                for value in options {
                    keyword(value, visit);
                }
            }
            Effect::AddRestriction { restriction } => {
                use crate::types::ability::{GameRestriction, ProhibitedActivity};
                match restriction {
                    GameRestriction::CantEnterBattlefieldFrom { filter: value, .. }
                    | GameRestriction::ProhibitActivity {
                        activity:
                            ProhibitedActivity::CastSpells {
                                spell_filter: Some(value),
                            },
                        ..
                    }
                    | GameRestriction::ProhibitActivity {
                        activity:
                            ProhibitedActivity::PlayLands {
                                land_filter: Some(value),
                            },
                        ..
                    } => filter(value, visit),
                    _ => {}
                }
            }
            Effect::GrantCastingPermission { permission, .. } => {
                use crate::types::ability::CastingPermission;
                if let Some(value) = permission.lifetime().duration {
                    duration(value, visit);
                }
                match permission {
                    CastingPermission::ExileWithAltAbilityCost {
                        cost, constraint, ..
                    } => {
                        walk_cost(cost, visit);
                        if let Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                            value,
                            ..
                        }) = constraint
                        {
                            quantity(value, visit);
                        }
                    }
                    CastingPermission::ExileWithAltCost {
                        constraint,
                        enters_with_modifications,
                        ..
                    } => {
                        if let Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                            value,
                            ..
                        }) = constraint
                        {
                            quantity(value, visit);
                        }
                        for value in &enters_with_modifications.modifications {
                            walk_modification(value, visit);
                        }
                        if let Some(value) = &enters_with_modifications.cast_filter {
                            filter(value, visit);
                        }
                    }
                    CastingPermission::PlayFromExile {
                        card_filter,
                        alt_ability_cost,
                        ..
                    } => {
                        if let Some(value) = card_filter {
                            filter(value, visit);
                        }
                        if let Some(value) = alt_ability_cost {
                            walk_cost(value, visit);
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        if let Effect::Token { keywords, .. }
        | Effect::Animate { keywords, .. }
        | Effect::CopyTokenOf {
            extra_keywords: keywords,
            ..
        } = effect
        {
            for value in keywords {
                keyword(value, visit);
            }
        }
        match effect {
            Effect::GenericEffect {
                duration: Some(window),
                ..
            }
            | Effect::BecomeCopy {
                duration: Some(window),
                ..
            }
            | Effect::GainActivatedAbilitiesOfTarget {
                duration: Some(window),
                ..
            }
            | Effect::CastFromZone {
                duration: Some(window),
                ..
            }
            | Effect::PreventDamage {
                prevention_duration: Some(window),
                ..
            }
            | Effect::ForceBlock {
                duration: window, ..
            }
            | Effect::ForceAttack {
                duration: window, ..
            } => duration(window, visit),
            _ => {}
        }
        effect.for_each_quantity_ref(&mut |value| reference(value, visit));
        effect.for_each_predicate_keyword_payload(&mut |value| keyword(value, visit));
    }
    let _ = nodes::visit_effect(&root.effect, &mut |node| replacement_node(node, visit));
    let _ = visit_effect(&root.effect, &mut |effect| {
        effect_leaves(effect, visit);
        ControlFlow::Continue(())
    });
    if let Some(payment) = &root.unless_pay {
        walk_cost(&payment.cost, visit);
        filter(&payment.payer, visit);
    }
    for definition in &root.mode_abilities {
        walk_ability(definition, visit);
    }
    root.for_each_direct_quantity_ref(&mut |value| reference(value, visit));
    if let Some(condition) = &root.condition {
        condition.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
    }
    if let Some(window) = &root.duration {
        duration(window, visit);
    }
    for value in [&root.optional_player, &root.target_chooser]
        .into_iter()
        .flatten()
    {
        filter(value, visit);
    }
    if let Some(value) = &root.player_scope {
        value.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
    }
    if let Some(value) = &root.activation_cost_reduction {
        quantity(&value.count, visit);
        if let Some(value) = &value.condition {
            parsed_condition(value, visit);
        }
    }
    if let Some(value) = &root.multi_target {
        quantity(&value.min, visit);
        if let Some(value) = &value.max {
            quantity(value, visit);
        }
    }
    if let Some(value) = &root.announced_x {
        quantity(value, visit);
    }
    for value in &root.target_constraints {
        if let crate::types::game_state::TargetSelectionConstraint::TotalManaValue {
            value, ..
        } = value
        {
            quantity(value, visit);
        }
    }
    if let Some(crate::types::ability::RepeatContinuation::WhileCondition { condition, .. }) =
        &root.repeat_until
    {
        condition.for_each_nested_keyword_payload(&mut |value| keyword(value, visit));
    }
    if let Some(source) = &root.trigger_source {
        trigger_source(source, visit);
    }
    if let Some(value) = &root.delayed_origin {
        delayed_condition(&value.condition, visit);
        let _ = nodes::visit_effect(&value.effect, &mut |node| replacement_node(node, visit));
        let _ = visit_effect(&value.effect, &mut |effect| {
            effect_leaves(effect, visit);
            ControlFlow::Continue(())
        });
    }
    if let Some(value) = &root.activation_record {
        for value in &value.source_lki.keywords {
            keyword(value, visit);
        }
        if let crate::types::events::BoundActivationCost::Cost(value) = &value.activation_cost {
            walk_cost(value, visit);
        }
    }
    if let Some((_, value)) = &root.reads_return_result {
        filter(&value.noun, visit);
    }
    for value in &root.context.controller_controlled_as_cast {
        filter(value, visit);
    }
    if let Some(value) = &root.context.graveyard_permission_latch {
        if let Some(value) = &value.extra_cost {
            walk_cost(&value.cost, visit);
        }
    }
    if let Some(value) = &root.context.creation_lookback_event {
        event(value, visit);
    }
    if let Some(value) = &root.context.damage_opportunity {
        for (_, value) in value
            .definitions
            .iter()
            .chain(&value.prevention_definitions)
        {
            walk_replacement(value, visit);
        }
    }
    if let Some(value) = &root.context.damage_source_choice {
        filter(&value.candidate_filter, visit);
        if let Some(value) = &value.source {
            trigger_source(value, visit);
        }
    }
    for snapshot in [
        &root.cost_paid_object,
        &root.effect_context_object,
        &root.amassed_army_object,
    ]
    .into_iter()
    .flatten()
    {
        for value in &snapshot.lki.keywords {
            keyword(value, visit);
        }
    }
    for record in &root.cost_paid_objects {
        if let CostPaidObjectRecord::Captured(snapshot) = record {
            for value in &snapshot.lki.keywords {
                keyword(value, visit);
            }
        }
    }
    if let Some(result) = &root.context.direct_discard_result {
        for value in &result.lki.keywords {
            keyword(value, visit);
        }
    }
    for child in root.sub_ability.iter().chain(&root.else_ability) {
        visit_resolved_replacements(child, visit);
    }
    for (_, child) in &root.coin_branch_target_abilities {
        visit_resolved_replacements(child, visit);
    }
}

#[cfg(test)]
mod replacement_program_tests {
    use super::*;
    use crate::types::ability::{
        AbilityKind, AdditionalCost, Comparator, Duration, FilterProp, ParsedCondition,
        QuantityExpr, QuantityRef, ResolvedAbility, SpellCastingOption, StaticCondition,
        TypedFilter, UnlessPayModifier,
    };
    use crate::types::format::FormatConfig;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId, ReplacementContinuationId};
    use crate::types::keywords::Keyword;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    fn root(effect: Effect) -> ResolvedAbility {
        ResolvedAbility::new(effect, vec![], ObjectId(41), PlayerId(0))
    }
    fn marker(id: ReplacementContinuationId) -> ReplacementDefinition {
        let mut definition = ReplacementDefinition::new(ReplacementEvent::DamageDone);
        definition.runtime_execute = Some(id);
        definition
    }
    fn payment(id: ReplacementContinuationId) -> AbilityCost {
        AbilityCost::EffectCost {
            effect: Box::new(Effect::AddTargetReplacement {
                replacement: Box::new(marker(id)),
                target: TargetFilter::None,
            }),
        }
    }
    fn keyword(id: ReplacementContinuationId) -> Keyword {
        Keyword::CumulativeUpkeep(payment(id))
    }
    fn predicate(id: ReplacementContinuationId) -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::creature()
                .properties(vec![FilterProp::WithKeyword { value: keyword(id) }]),
        )
    }
    fn quantity(id: ReplacementContinuationId) -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: predicate(id),
            },
        }
    }
    fn static_root(definition: StaticDefinition) -> ResolvedAbility {
        root(Effect::GenericEffect {
            static_abilities: vec![definition],
            duration: None,
            target: None,
            end_cost: None,
        })
    }
    fn carriers(id: ReplacementContinuationId) -> Vec<(&'static str, ResolvedAbility)> {
        let mut unless = root(Effect::NoOp);
        unless.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::Composite {
                costs: vec![AbilityCost::Tap, payment(id)],
            },
            payer: TargetFilter::Controller,
        });
        let mut payer = root(Effect::NoOp);
        payer.unless_pay = Some(UnlessPayModifier {
            cost: AbilityCost::Tap,
            payer: predicate(id),
        });
        let mut mode = root(Effect::NoOp);
        mode.mode_abilities.push(
            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp).cost(
                AbilityCost::PerCounter {
                    counter: crate::types::counter::CounterType::Plus1Plus1,
                    target: TargetFilter::SelfRef,
                    base: Box::new(AbilityCost::OneOf {
                        costs: vec![AbilityCost::Tap, payment(id)],
                    }),
                },
            ),
        );
        let mut per_player = StaticDefinition::continuous();
        per_player.per_player_condition = Some(ParsedCondition::SourceLacksKeyword {
            keyword: keyword(id),
        });
        let mut condition = StaticDefinition::continuous();
        condition.condition = Some(StaticCondition::QuantityComparison {
            lhs: quantity(id),
            rhs: QuantityExpr::Fixed { value: 1 },
            comparator: Comparator::GE,
        });
        let mut copy_values = CopiableValues {
            name: "Supplemental visitor copy".into(),
            mana_cost: Default::default(),
            color: vec![],
            card_types: Default::default(),
            power: None,
            toughness: None,
            loyalty: None,
            printed_loyalty: None,
            keywords: vec![],
            abilities: Arc::new(vec![]),
            trigger_definitions: Arc::new(vec![]),
            trigger_printed_origins: Arc::new(vec![]),
            replacement_definitions: Arc::new(vec![]),
            static_definitions: Arc::new(vec![]),
            room_halves: None,
            prepare_face: None,
            name_origin: Default::default(),
        };
        copy_values.prepare_face = Some(Arc::new(crate::game::game_object::BackFaceData {
            name: "Supplemental prepared visitor face".into(),
            power: None,
            toughness: None,
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: Default::default(),
            mana_cost: Default::default(),
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: Default::default(),
            trigger_printed_origins: vec![],
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![],
            printed_ref: None,
            modal: None,
            additional_cost: Some(AdditionalCost::Choice(AbilityCost::Tap, payment(id))),
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![SpellCastingOption::alternative_cost(payment(id))],
            parse_warnings: vec![],
            layout_kind: None,
            is_swap_snapshot: false,
        }));
        let mut alternative_values = copy_values.clone();
        Arc::make_mut(alternative_values.prepare_face.as_mut().unwrap()).additional_cost = None;
        Arc::make_mut(copy_values.prepare_face.as_mut().unwrap())
            .casting_options
            .clear();
        let alternative_copy = static_root(StaticDefinition::continuous().modifications(vec![
            ContinuousModification::CopyValues {
                values: Box::new(alternative_values),
                display: crate::game::game_object::DisplayIdentity {
                    display_source: Default::default(),
                    printed_ref: None,
                    token_image_ref: None,
                    token_art: None,
                },
            },
        ]));
        let copy = static_root(StaticDefinition::continuous().modifications(vec![
            ContinuousModification::CopyValues {
                values: Box::new(copy_values),
                display: crate::game::game_object::DisplayIdentity {
                    display_source: Default::default(),
                    printed_ref: None,
                    token_image_ref: None,
                    token_art: None,
                },
            },
        ]));
        let mut snapshot = crate::game::game_object::GameObject::new(
            ObjectId(41),
            CardId(1),
            PlayerId(0),
            "Supplemental visitor LKI".into(),
            Zone::Battlefield,
        )
        .snapshot_public_characteristics();
        snapshot.keywords.push(keyword(id));
        let mut envelope = root(Effect::NoOp);
        envelope.effect_context_object = Some(crate::types::ability::CostPaidObjectSnapshot {
            object_id: ObjectId(41),
            lki: snapshot,
            incarnation: 1,
            binding: None,
        });
        let mut else_root = root(Effect::NoOp);
        else_root.else_ability = Some(Box::new(root(Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: predicate(id),
        })));
        vec![
            ("unless composite cost", unless),
            ("unless payer", payer),
            ("nested mode cost containers", mode),
            (
                "effect filter keyword cost",
                root(Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: predicate(id),
                }),
            ),
            (
                "static mode filter",
                static_root(StaticDefinition::new(StaticMode::CantBeBlockedBy {
                    filter: predicate(id),
                })),
            ),
            (
                "static alternative cost",
                static_root(StaticDefinition::new(StaticMode::AlternativeKeywordCost {
                    keyword: crate::types::keywords::KeywordKind::Cycling,
                    cost: payment(id),
                    frequency: None,
                })),
            ),
            (
                "static per-player parsed condition",
                static_root(per_player),
            ),
            ("static quantity condition", static_root(condition)),
            (
                "continuous modification quantity",
                static_root(StaticDefinition::continuous().modifications(vec![
                    ContinuousModification::SetDynamicPower {
                        value: quantity(id),
                    },
                ])),
            ),
            ("prepare face additional cost", copy),
            ("prepare face alternative cost", alternative_copy),
            ("frozen effect-context LKI keyword", envelope),
            ("resolved else payload", else_root),
        ]
    }

    #[test]
    fn arena_visitor_reaches_typed_leaf_carriers_and_keeps_definitions_intact() {
        let id = ReplacementContinuationId(37);
        for (label, program) in carriers(id) {
            let before = program.clone();
            let mut handles = vec![];
            visit_resolved_replacements(&program, &mut |definition| {
                if let Some(handle) = definition.runtime_execute {
                    handles.push(handle);
                }
            });
            assert!(
                !handles.is_empty(),
                "the marker is reachable through {label}"
            );
            assert!(
                handles.iter().all(|handle| *handle == id),
                "{label}: {handles:?}"
            );
            assert_eq!(
                program, before,
                "visitation must preserve the full {label} definition"
            );
        }
    }

    #[test]
    fn arena_visitor_reaches_all_nonmana_keyword_costs_and_registered_boundaries() {
        use crate::types::keywords::*;
        let id = ReplacementContinuationId(19);
        let keywords = [
            Keyword::CumulativeUpkeep(payment(id)),
            Keyword::Escalate(payment(id)),
            Keyword::Bestow(BestowCost::NonMana(payment(id))),
            Keyword::Embalm(EmbalmCost::NonMana(payment(id))),
            Keyword::Eternalize(EternalizeCost::NonMana(payment(id))),
            Keyword::Cycling(CyclingCost::NonMana(payment(id))),
            Keyword::Flashback(FlashbackCost::NonMana(payment(id))),
            Keyword::Escape(EscapeCost::NonMana(payment(id))),
            Keyword::Evoke(EvokeCost::NonMana(payment(id))),
            Keyword::Blitz(BlitzCost::NonMana(payment(id))),
            Keyword::Buyback(BuybackCost::NonMana(payment(id))),
            Keyword::Echo(EchoCost::NonMana(payment(id))),
            Keyword::Morph(MorphCost::NonMana(payment(id))),
            Keyword::Entwine(EntwineCost::NonMana(payment(id))),
        ];
        for value in keywords {
            let program = static_root(StaticDefinition::continuous().modifications(vec![
                ContinuousModification::AddKeyword {
                    keyword: value.clone(),
                },
            ]));
            let mut found = false;
            visit_resolved_replacements(&program, &mut |definition| {
                found |= definition.runtime_execute == Some(id);
            });
            assert!(found, "nested non-mana keyword cost: {value:?}");
        }
        let program = root(Effect::CreateBoon {
            recipient: TargetFilter::Controller,
            trigger: Box::new(
                TriggerDefinition::new(crate::types::triggers::TriggerMode::SpellCast).execute(
                    AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::AddTargetReplacement {
                            replacement: Box::new(marker(id)),
                            target: TargetFilter::None,
                        },
                    ),
                ),
            ),
        });
        let mut direct_count = 0;
        let _ = nodes::visit_effect_scoped(
            &program.effect,
            ResolutionScope::OwnResolutionOnly,
            &mut |_| {
                direct_count += 1;
                ControlFlow::Continue(())
            },
        );
        assert_eq!(
            direct_count, 0,
            "registered definitions stay outside the installing process"
        );
        let mut handles = vec![];
        visit_resolved_replacements(&program, &mut |definition| {
            handles.push(definition.runtime_execute);
        });
        assert_eq!(
            handles,
            vec![Some(id)],
            "state admission still sees the separate registered body"
        );
        let mut count = 0;
        visit_resolved_replacements(&root(Effect::NoOp), &mut |_| count += 1);
        assert_eq!(count, 0, "a plain leaf invents no replacement");
    }

    #[test]
    fn arena_visitor_drives_state_admission_for_valid_foreign_and_dangling_leaf_programs() {
        let mut state = GameState::new(FormatConfig::standard(), 2, 7);
        let valid = state
            .install_replacement_continuation(root(Effect::NoOp))
            .unwrap();
        for (label, program) in carriers(valid) {
            assert!(
                state.validate_replacement_program(&program).is_ok(),
                "valid independent rider through {label}"
            );
        }
        let before = state.clone();
        for (label, program) in carriers(ReplacementContinuationId(999)) {
            assert!(
                state.validate_replacement_program(&program).is_err(),
                "dangling rider through {label} must refuse"
            );
        }
        assert_eq!(
            state, before,
            "rejected graph validation cannot mutate its owner"
        );
        let read = Effect::GainLife {
            player: TargetFilter::Controller,
            amount: QuantityExpr::Ref {
                qty: QuantityRef::ZoneChangeAggregate {
                    source: crate::types::ability::ZoneChangeRecordSource::InstructionResult {
                        result: crate::types::ability::ReturnResultId(7),
                    },
                    from: Some(Zone::Battlefield),
                    to: Some(Zone::Exile),
                    filter: TargetFilter::Typed(TypedFilter::creature()),
                    function: crate::types::ability::AggregateFunction::Sum,
                    property: crate::types::ability::ObjectProperty::Power,
                },
            },
        };
        assert!(
            state
                .install_replacement_continuation(root(read.clone()))
                .is_err(),
            "a new registered reader cannot import its installer's result"
        );
        assert_eq!(
            state, before,
            "failed install must not append an invalid body"
        );
        assert_eq!(
            state.replacement_continuations.len(),
            before.replacement_continuations.len(),
            "failed installation leaves append-only storage unchanged"
        );
        let mut local = AbilityDefinition::new(AbilityKind::Spell, serde_json::from_value(
            serde_json::json!({"type":"ChangeZoneAll","origin":"Battlefield","destination":"Exile"})
        ).unwrap());
        local.declares_return_result = Some(crate::types::ability::ReturnResultId(7));
        local.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            read.clone(),
        )));
        let local =
            crate::game::ability_utils::build_resolved_from_def(&local, ObjectId(41), PlayerId(0));
        let local_handle = state
            .install_replacement_continuation(local.clone())
            .unwrap();
        for (label, program) in carriers(local_handle) {
            assert!(
                state.validate_replacement_program(&program).is_ok(),
                "a complete local producer/reader remains independent through {label}"
            );
        }
        let mut cyclic = state.clone();
        let cycle = ReplacementContinuationId(cyclic.replacement_continuations.len() as u64);
        cyclic
            .replacement_continuations
            .push_back(root(Effect::AddTargetReplacement {
                replacement: Box::new(marker(cycle)),
                target: TargetFilter::None,
            }));
        let cycle_root = root(Effect::AddTargetReplacement {
            replacement: Box::new(marker(cycle)),
            target: TargetFilter::None,
        });
        assert!(
            cyclic.validate_replacement_program(&cycle_root).is_err(),
            "a saved self-reference must refuse without an unchecked arena lookup"
        );
        assert!(
            cyclic.validate_replacement_continuation_arena().is_err(),
            "whole-arena validation sees the same invalid graph"
        );
        let foreign = ReplacementContinuationId(state.replacement_continuations.len() as u64);
        state.replacement_continuations.push_back(root(read));
        for (label, program) in carriers(foreign) {
            let mut outer = local.clone();
            outer.sub_ability = Some(Box::new(program));
            assert!(
                state.validate_replacement_program(&outer).is_err(),
                "foreign independent rider through {label} must refuse despite the outer local R7 producer"
            );
        }
    }
}

/// CR 201.5a: the positions in one definition node where a granter symbol is bound.
///
/// The parser can place a symbol elsewhere; every entry that parses masked text then refuses
/// that definition (`parser::oracle::granter_reference_unreached`).
macro_rules! define_granter_walk {
    ([$($mut_:tt)?]; $Node:ident; $nodes:ident) => {
use crate::types::ability::{
    AbilityCondition, AbilityCost, ActivationRestriction, CardTypeSetSource,
    ContinuousModification, Duration, Effect, FilterProp, ObjectScope, ParsedCondition,
    PlayerFilter, PtValue, QuantityExpr, QuantityRef, ReplacementCondition, StaticCondition,
    TargetFilter, TriggerCondition,
};
use crate::types::identifiers::ObjectIncarnationRef;
use crate::types::statics::{BlockExceptionKind, StaticMode};
use super::{$Node, $nodes};
use std::ops::ControlFlow;

/// A granter symbol: the incarnation slot of a filter, an object scope, or the caster
/// player filter.
pub(crate) enum Symbol<'a> {
    Filter(&'a $($mut_)? Option<ObjectIncarnationRef>),
    Scope(&'a $($mut_)? ObjectScope),
    Caster(&'a $($mut_)? TargetFilter),
}

/// Visits `root` and every definition node nested in it — the nodes whose fields
/// [`node_fields`] reads.
pub(crate) fn each_node(root: $Node<'_>, v: &mut impl FnMut($Node<'_>)) {
    let mut on = |node: $Node<'_>| on_node(node, v);
    let _ = match root {
        $Node::Ability(def) => $nodes::visit_ability_def(def, &mut on),
        $Node::Trigger(trigger) => $nodes::visit_trigger(trigger, &mut on),
        $Node::Static(def) => $nodes::visit_static(def, &mut on),
        $Node::Replacement(def) => $nodes::visit_replacement(def, &mut on),
    };
}

/// [`each_node`] over the definitions a modification grants.
pub(crate) fn each_node_in(m: &$($mut_)? ContinuousModification, v: &mut impl FnMut($Node<'_>)) {
    let _ = $nodes::visit_continuous_mod(m, &mut |node| on_node(node, &mut *v));
}

/// The "player who cast <granter>" filters in the definitions `m` grants.
pub(crate) fn each_caster_in(m: &$($mut_)? ContinuousModification, v: &mut impl FnMut(&$($mut_)? TargetFilter)) {
    each_node_in(m, &mut |node| node_fields(node, &mut |symbol| {
        if let Symbol::Caster(f) = symbol {
            v(f);
        }
    }));
}

// `BecomeCopy` is a leaf of the node walk, but its extra modifications carry grants.
fn on_node($($mut_)? node: $Node<'_>, v: &mut impl FnMut($Node<'_>)) -> ControlFlow<()> {
    if let $Node::Ability(def) = &$($mut_)? node {
        if let Effect::BecomeCopy { additional_modifications, .. } = &$($mut_)? *def.effect {
            for m in additional_modifications {
                each_node_in(m, v);
            }
        }
    }
    v(node);
    ControlFlow::Continue(())
}

/// Visits the symbols in `node`'s own fields; nested definition nodes are not entered.
pub(crate) fn node_fields(node: $Node<'_>, v: &mut impl FnMut(Symbol<'_>)) {
    match node {
        $Node::Ability(def) => {
            effect(&$($mut_)? def.effect, v);
            if let Some(cost) = &$($mut_)? def.cost {
                ability_cost(cost, v);
            }
            if let Some(condition) = &$($mut_)? def.condition {
                ability_condition(condition, v);
            }
            if let Some(d) = &$($mut_)? def.duration {
                duration(d, v);
            }
            for restriction in &$($mut_)? def.activation_restrictions {
                if let ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::QuantityComparison { lhs, rhs, .. }),
                } = restriction
                {
                    quantity(lhs, v);
                    quantity(rhs, v);
                }
            }
        }
        $Node::Trigger(trigger) => {
            for f in [&$($mut_)? trigger.valid_card, &$($mut_)? trigger.valid_target, &$($mut_)? trigger.valid_source]
                .into_iter()
                .flatten()
            {
                filter(f, v);
            }
            if let Some(condition) = &$($mut_)? trigger.condition {
                trigger_condition(condition, v);
            }
            if let Some(condition) = &$($mut_)? trigger.attack_cost_condition {
                static_condition(condition, v);
            }
        }
        $Node::Static(def) => {
            if let Some(affected) = &$($mut_)? def.affected {
                filter(affected, v);
            }
            if let Some(condition) = &$($mut_)? def.condition {
                static_condition(condition, v);
            }
            static_mode(&$($mut_)? def.mode, v);
            for m in &$($mut_)? def.modifications {
                modification(m, v);
            }
        }
        $Node::Replacement(def) => {
            if let Some(f) = &$($mut_)? def.valid_card {
                filter(f, v);
            }
            if let Some(condition) = &$($mut_)? def.condition {
                replacement_condition(condition, v);
            }
        }
    }
}

fn effect(e: &$($mut_)? Effect, v: &mut impl FnMut(Symbol<'_>)) {
    match e {
        Effect::Sacrifice { target, count, .. }
        | Effect::PutCounter { target, count, .. }
        | Effect::RemoveCounter { target, count, .. } => {
            filter(target, v);
            quantity(count, v);
        }
        Effect::ChangeZone { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::Destroy { target, .. }
        | Effect::Fight { target, .. }
        | Effect::SetTapState { target, .. }
        | Effect::Attach { attachment: target, .. } => filter(target, v),
        Effect::DealDamage { amount, target, .. } => {
            quantity(amount, v);
            filter(target, v);
        }
        Effect::GainLife { amount, .. } => quantity(amount, v),
        Effect::Pump { power, toughness, target, .. } => {
            pt(power, v);
            pt(toughness, v);
            filter(target, v);
        }
        Effect::Token { power, toughness, .. } => {
            pt(power, v);
            pt(toughness, v);
        }
        Effect::Animate { power, toughness, .. } => {
            for p in [power, toughness].into_iter().flatten() {
                pt(p, v);
            }
        }
        Effect::BecomeCopy { duration: Some(d), .. }
        | Effect::CastFromZone { duration: Some(d), .. }
        | Effect::GenericEffect { duration: Some(d), .. } => duration(d, v),
        _ => {}
    }
}

fn ability_cost(cost: &$($mut_)? AbilityCost, v: &mut impl FnMut(Symbol<'_>)) {
    match cost {
        AbilityCost::Sacrifice(sacrifice) => filter(&$($mut_)? sacrifice.target, v),
        AbilityCost::Exile { filter: Some(f), .. }
        | AbilityCost::RemoveCounter { target: Some(f), .. }
        | AbilityCost::ReturnToHand { filter: Some(f), .. }
        | AbilityCost::Reveal { filter: Some(f), .. } => filter(f, v),
        AbilityCost::EffectCost { effect: e } => effect(e, v),
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for c in costs {
                ability_cost(c, v);
            }
        }
        _ => {}
    }
}

fn ability_condition(condition: &$($mut_)? AbilityCondition, v: &mut impl FnMut(Symbol<'_>)) {
    match condition {
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            quantity(lhs, v);
            quantity(rhs, v);
        }
        AbilityCondition::QuantityQuality {
            quantity: q,
            quality: _,
        } => quantity(q, v),
        AbilityCondition::PreviousEffectAmount { rhs, .. } => quantity(rhs, v),
        AbilityCondition::ConditionInstead { inner: c } | AbilityCondition::Not { condition: c } => {
            ability_condition(c, v)
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for c in conditions {
                ability_condition(c, v);
            }
        }
        _ => {}
    }
}

fn trigger_condition(condition: &$($mut_)? TriggerCondition, v: &mut impl FnMut(Symbol<'_>)) {
    match condition {
        TriggerCondition::ControlsType { filter: f } | TriggerCondition::ControlsNone { filter: f } => {
            filter(f, v)
        }
        TriggerCondition::QuantityComparison { lhs, rhs, .. } => {
            quantity(lhs, v);
            quantity(rhs, v);
        }
        TriggerCondition::QuantityQuality {
            quantity: q,
            quality: _,
        } => quantity(q, v),
        // CR 201.5a: an `EventTime` condition (a state trigger's head, CR 603.8,
        // or an event trigger's "while" gate, CR 508.1m) is the wrapped
        // condition read only when the trigger fires, so its granter symbols
        // are the wrapped condition's.
        TriggerCondition::Not { condition: c } | TriggerCondition::EventTime { condition: c } => {
            trigger_condition(c, v)
        }
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            for c in conditions {
                trigger_condition(c, v);
            }
        }
        _ => {}
    }
}

pub(crate) fn static_condition(condition: &$($mut_)? StaticCondition, v: &mut impl FnMut(Symbol<'_>)) {
    match condition {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            quantity(lhs, v);
            quantity(rhs, v);
        }
        StaticCondition::QuantityQuality {
            quantity: q,
            quality: _,
        } => quantity(q, v),
        StaticCondition::IsTapped { scope } => object_scope(scope, v),
        StaticCondition::Not { condition: c } => static_condition(c, v),
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for c in conditions {
                static_condition(c, v);
            }
        }
        _ => {}
    }
}

fn replacement_condition(condition: &$($mut_)? ReplacementCondition, v: &mut impl FnMut(Symbol<'_>)) {
    match condition {
        ReplacementCondition::OnlyIfQuantity { lhs, rhs, .. }
        | ReplacementCondition::UnlessQuantity { lhs, rhs, .. } => {
            quantity(lhs, v);
            quantity(rhs, v);
        }
        ReplacementCondition::UnlessControlsCountMatching { filter: f, .. } => filter(f, v),
        ReplacementCondition::And { conditions } => {
            for c in conditions {
                replacement_condition(c, v);
            }
        }
        _ => {}
    }
}

fn static_mode(mode: &$($mut_)? StaticMode, v: &mut impl FnMut(Symbol<'_>)) {
    match mode {
        StaticMode::CantBeBlockedBy { filter: f }
        | StaticMode::CantBeBlockedExceptBy { kind: BlockExceptionKind::Quality(f) } => filter(f, v),
        _ => {}
    }
}

/// Only a modification's own values; a grant's definition is a node of its own.
pub(crate) fn modification(m: &$($mut_)? ContinuousModification, v: &mut impl FnMut(Symbol<'_>)) {
    match m {
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value } => quantity(value, v),
        ContinuousModification::AddStaticMode { mode } => static_mode(mode, v),
        _ => {}
    }
}

fn duration(d: &$($mut_)? Duration, v: &mut impl FnMut(Symbol<'_>)) {
    if let Duration::ForAsLongAs { condition } = d {
        static_condition(condition, v);
    }
}

pub(crate) fn filter(f: &$($mut_)? TargetFilter, v: &mut impl FnMut(Symbol<'_>)) {
    // Checked before the `match`: an arm there would still borrow `f` while handing it off.
    if matches!(f, TargetFilter::PlayerMatching { player } if **player == PlayerFilter::GrantingObjectCaster) {
        v(Symbol::Caster(f));
        return;
    }
    match f {
        TargetFilter::GrantingObject { bound } => v(Symbol::Filter(bound)),
        TargetFilter::Typed(typed) => {
            for prop in &$($mut_)? typed.properties {
                match prop {
                    FilterProp::DistinctFrom { reference } => filter(reference, v),
                    FilterProp::Cmc { value, .. } | FilterProp::PtComparison { value, .. } => {
                        quantity(value, v)
                    }
                    _ => {}
                }
            }
        }
        TargetFilter::Not { filter: inner } => filter(inner, v),
        TargetFilter::And { filters } | TargetFilter::Or { filters } => {
            for inner in filters {
                filter(inner, v);
            }
        }
        _ => {}
    }
}

fn pt(p: &$($mut_)? PtValue, v: &mut impl FnMut(Symbol<'_>)) {
    if let PtValue::Quantity(q) = p {
        quantity(q, v);
    }
}

fn quantity(q: &$($mut_)? QuantityExpr, v: &mut impl FnMut(Symbol<'_>)) {
    match q {
        QuantityExpr::Ref { qty } => quantity_ref(qty, v),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power { exponent: inner, .. } => quantity(inner, v),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for inner in exprs {
                quantity(inner, v);
            }
        }
        QuantityExpr::Difference { left, right } => {
            quantity(left, v);
            quantity(right, v);
        }
        QuantityExpr::Fixed { .. } => {}
    }
}

fn quantity_ref(r: &$($mut_)? QuantityRef, v: &mut impl FnMut(Symbol<'_>)) {
    match r {
        QuantityRef::CountersOn { scope, .. } => object_scope(scope, v),
        QuantityRef::ObjectCount { filter: f }
        | QuantityRef::ZoneChangeAggregate { filter: f, .. } => filter(f, v),
        QuantityRef::PropertyAggregate(aggregate) => {
            if let CardTypeSetSource::Objects { filter: f } = define_granter_walk!(@source aggregate $($mut_)?) {
                filter(f, v);
            }
        }
        _ => {}
    }
}

fn object_scope(scope: &$($mut_)? ObjectScope, v: &mut impl FnMut(Symbol<'_>)) {
    if matches!(scope, ObjectScope::GrantingObject) {
        v(Symbol::Scope(scope));
    }
}
    };
    (@source $aggregate:ident) => { $aggregate.source() };
    (@source $aggregate:ident mut) => { $aggregate.source_mut() };
}

/// Shared granter-symbol walk.
pub(crate) mod granter_symbols {
    define_granter_walk!([]; DefinitionNode; nodes);
}

/// Exclusive granter-symbol walk.
pub(crate) mod granter_symbols_mut {
    define_granter_walk!([mut]; DefinitionNodeMut; nodes_mut);

    /// CR 201.5a + CR 400.7: binds a symbol to `granter` — a scope to that incarnation
    /// whether current or not, a filter to match that incarnation and no later one.
    pub(crate) fn bind(granter: ObjectIncarnationRef) -> impl FnMut(Symbol<'_>) {
        move |symbol| match symbol {
            Symbol::Filter(bound) => {
                bound.get_or_insert(granter);
            }
            Symbol::Scope(scope) => *scope = ObjectScope::SpecificObject { object: granter },
            Symbol::Caster(_) => {}
        }
    }
}

/// Visits every granter symbol in `root` and the definition nodes nested in it.
pub(crate) fn each_granter_symbol(
    root: DefinitionNode<'_>,
    v: &mut impl FnMut(granter_symbols::Symbol<'_>),
) {
    granter_symbols::each_node(root, &mut |node| granter_symbols::node_fields(node, v));
}

/// CR 201.5a: whether a granter symbol still names an unbound granter.
pub(crate) fn names_unbound_granter(symbol: &granter_symbols::Symbol<'_>) -> bool {
    match symbol {
        granter_symbols::Symbol::Filter(bound) => bound.is_none(),
        granter_symbols::Symbol::Scope(scope) => {
            matches!(scope, crate::types::ability::ObjectScope::GrantingObject)
        }
        granter_symbols::Symbol::Caster(_) => false,
    }
}

/// CR 201.5a: whether a static's own fields name an unbound granter. Nested definition nodes
/// are not entered; [`nested_definitions_name_granter`] covers them.
pub(crate) fn own_fields_name_granter(def: &StaticDefinition) -> bool {
    let mut names = false;
    granter_symbols::node_fields(DefinitionNode::Static(def), &mut |symbol| {
        names |= names_unbound_granter(&symbol);
    });
    names
}

/// CR 201.5a: whether a definition nested in `def`'s modifications names an unbound
/// granter — together with [`own_fields_name_granter`], the whole of
/// [`each_granter_symbol`]'s walk over a static.
pub(crate) fn nested_definitions_name_granter(def: &StaticDefinition) -> bool {
    let mut names = false;
    for m in &def.modifications {
        granter_symbols::each_node_in(m, &mut |node| {
            granter_symbols::node_fields(node, &mut |symbol| {
                names |= names_unbound_granter(&symbol);
            })
        });
    }
    names
}

/// CR 111.3 + CR 201.5a: whether a created token's static grants its nested definitions
/// to the token itself. Those definitions are the token's own quoted abilities — its
/// text, written by the creating effect — so a by-name reference in them names the
/// creator and is bound at creation, as one in the static's own fields is. Any other
/// static grants its nested definitions to other objects; the token is that grant's
/// source, so a by-name reference in them names the token and binds at layer 6
/// (CR 613.1f).
pub(crate) fn token_static_grants_to_itself(def: &StaticDefinition) -> bool {
    matches!(def.affected.as_ref(), Some(TargetFilter::SelfRef))
}
