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
//! `AddPendingEntersModifications`, `EachPlayerCopyChosen`, and `ReturnAsAura`,
//! all leaves here. So a carrier added there is not necessarily a carrier here,
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
    TriggerDefinition, VoteSubject,
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
/// `SeparateIntoPiles`, `RevealFromHand.on_decline`, `FlipCoin` / `FlipCoins`,
/// `FlipCoinUntilLose`, `RollDie.results`, and `ChooseOneOf.branches` (all via
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
        if !scope_prunes_nested_ability(sub, scope) {
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
    ([$($mut_:tt)?]; [$($bound:tt)+]; $on_effect:ident; $on_node:ident; $arc_iter:ident) => {
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
    visit_effect_scoped(&$($mut_)? def.effect, scope, visit)?;
    if let Some(cost) = &$($mut_)? def.cost {
        visit_cost_scoped(cost, scope, visit)?;
    }
    if let Some(sub) = &$($mut_)? def.sub_ability {
        visit_nested_ability_def_scoped(sub, scope, visit)?;
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
        // GrantAllActivatedAbilitiesOf / GrantAllTriggeredAbilitiesOf only hold a
        // source `TargetFilter`; the granted abilities/triggers are pulled live
        // from the provider objects at layer collection time, not nested here.
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
            if let Some(sub) = win_effect {
                visit_nested_ability_def_scoped(sub, scope, visit)?;
            }
            if let Some(sub) = lose_effect {
                visit_nested_ability_def_scoped(sub, scope, visit)?;
            }
        }
        Effect::FlipCoinUntilLose { win_effect } => {
            visit_nested_ability_def_scoped(win_effect, scope, visit)?
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
        // CR 614.12 + CR 303.4: ReturnAsAura.grants carry typed
        // ContinuousModifications, never conjured card names.
        | Effect::ReturnAsAura { .. }
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

define_walk!([]; [FnMut(&Effect) -> ControlFlow<()>]; visit_each_effect; skip_nodes; shared_iter);

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

    define_walk!([mut]; [FnMut(DefinitionNodeMut<'_>) -> ControlFlow<()>]; skip_effects; visit_each_node; exclusive_iter);
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

    define_walk!([]; [FnMut(DefinitionNode<'_>) -> ControlFlow<()>]; skip_effects; visit_each_node_ref; shared_iter);
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
        TriggerCondition::Not { condition: c } => trigger_condition(c, v),
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
        QuantityRef::ObjectCount { filter: f } => filter(f, v),
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
