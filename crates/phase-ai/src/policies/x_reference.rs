//! X-reference detection — the single authority for "does this spell/ability's
//! payoff depend on the chosen X?", shared by the ramp policy (`x_value.rs`,
//! which prefers max X) and the no-op gate (`x_cast_gate.rs`, which rejects a
//! cast whose only affordable X is 0).
//!
//! All detectors walk an ability/effect/static tree and delegate the per-`QuantityExpr`
//! test to the engine's single authority `QuantityExpr::contains_x`
//! (`QuantityRef::Variable { name: "X" }`), extended here for two cast-context
//! references the ramp/gate also treat as X-scaled:
//!
//! - `CostXPaid` (CR 107.3m) — the announced X after payment, carried on the
//!   spell object. Dynamic P/T statics granted by `{X}` abilities (Mirror
//!   Entity) reference X this way once on the stack.
//! - the engine's `QuantityRef::PreviousEffectAmount` — the numeric result of
//!   the immediately preceding chain effect. When that predecessor is itself
//!   X-scaled, this is transitively 0 at X=0 (Exsanguinate's "gain life equal to
//!   the life lost this way").
//!
//! Building for the class: the gate keys on these structural detectors, never a
//! card name, so every {X}-cost spell/ability whose payoff scales with X is
//! covered.

use engine::types::ability::{
    AbilityDefinition, ContinuousModification, Effect, FilterProp, QuantityExpr, QuantityRef,
    ReplacementDefinition, StaticDefinition, TargetFilter, TriggerDefinition,
};
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::statics::{HandSizeModification, StaticMode};

/// True when the spell-object-on-stack's printed triggers or replacement
/// effects reference X. Covers X-cost creature spells whose X is consumed by
/// cast triggers or ETB replacements rather than the resolving spell effect
/// itself — Hydroid Krasis ("when you cast this spell, you gain X life and
/// draw X cards" + "enters as an X/X"), Genesis Hydra ("when you cast … look
/// at the top X cards"), Hooded Hydra / Hangarback Walker (ETB-with-X-counter
/// replacement on the creature itself). Without this, the AI would still pick
/// X=0 for the entire Hydra / X-counter-ETB class because their X reference
/// is structurally outside the resolving spell ability.
pub(crate) fn spell_object_references_x(state: &GameState, object_id: ObjectId) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    // Spell-cast triggers / dies / etc. on the stack object.
    for trigger in obj.trigger_definitions.iter_unchecked() {
        if let Some(exec) = &trigger.execute {
            if ability_definition_references_x(exec) {
                return true;
            }
        }
    }
    // ETB-X-counter and similar self-replacements stamped on the spell
    // object (consumed when the permanent enters the battlefield).
    for replacement in obj.replacement_definitions.iter_unchecked() {
        if let Some(exec) = &replacement.execute {
            if ability_definition_references_x(exec) {
                return true;
            }
        }
    }
    // The printed abilities themselves may reference X via repeat_for /
    // sub_ability chains (rare but cheap to scan).
    for ability in obj.abilities.iter() {
        if ability_definition_references_x(ability) {
            return true;
        }
    }
    // X-cost creatures can carry their payoff as static definitions instead
    // of spell/trigger/replacement effects, e.g. dynamic P/T or granted
    // ability/static payloads. Scan both live and printed baselines: live
    // definitions may be layer-filtered, while base definitions are the
    // authoritative printed shape for the stack object.
    for static_def in obj.static_definitions.iter_unchecked() {
        if static_definition_references_x(static_def) {
            return true;
        }
    }
    for static_def in obj.base_static_definitions.iter() {
        if static_definition_references_x(static_def) {
            return true;
        }
    }
    false
}

pub(crate) fn ability_definition_references_x(ability: &AbilityDefinition) -> bool {
    if effect_references_x(&ability.effect) {
        return true;
    }
    if let Some(expr) = &ability.repeat_for {
        if expr.contains_x() {
            return true;
        }
    }
    if let Some(sub) = &ability.sub_ability {
        if ability_definition_references_x(sub) {
            return true;
        }
    }
    if let Some(else_branch) = &ability.else_ability {
        if ability_definition_references_x(else_branch) {
            return true;
        }
    }
    false
}

pub(crate) fn static_definition_references_x(static_def: &StaticDefinition) -> bool {
    static_mode_references_x(&static_def.mode)
        || static_def
            .modifications
            .iter()
            .any(continuous_modification_references_x)
        // RC1: a granted static whose AFFECTED subject filter references X (Day
        // of Black Sun — "each creature with mana value X or less") scales with
        // X even though its modifications (RemoveAllAbilities) do not.
        || static_def
            .affected
            .as_ref()
            .is_some_and(target_filter_references_x)
}

fn static_mode_references_x(mode: &StaticMode) -> bool {
    match mode {
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::EqualTo(expr),
        } => expr.contains_x(),
        StaticMode::ModifyCost {
            dynamic_count: Some(qty),
            ..
        }
        | StaticMode::ReduceAbilityCost {
            dynamic_count: Some(qty),
            ..
        } => quantity_ref_references_x(qty),
        _ => false,
    }
}

fn continuous_modification_references_x(modification: &ContinuousModification) -> bool {
    match modification {
        ContinuousModification::CopyValues { values, .. } => {
            values.abilities.iter().any(ability_definition_references_x)
                || values
                    .trigger_definitions
                    .iter()
                    .any(trigger_definition_references_x)
                || values
                    .replacement_definitions
                    .iter()
                    .any(replacement_definition_references_x)
                || values
                    .static_definitions
                    .iter()
                    .any(static_definition_references_x)
        }
        ContinuousModification::GrantAbility { definition } => {
            ability_definition_references_x(definition)
        }
        ContinuousModification::GrantTrigger { trigger } => {
            trigger_definition_references_x(trigger)
        }
        ContinuousModification::GrantStaticAbility { definition } => {
            static_definition_references_x(definition)
        }
        // RC1: dynamic P/T, keyword, and enter-counter magnitudes may reference
        // the chosen X either directly (`Variable "X"`) or via `CostXPaid` (the
        // announced X carried on the granting object — Mirror Entity's
        // `SetPowerDynamic { CostXPaid }`). Route through `expr_references_chosen_x`
        // so both forms are detected without touching the engine's `contains_x`.
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value }
        | ContinuousModification::AddDynamicKeyword { value, .. }
        | ContinuousModification::AddCounterOnEnter { count: value, .. } => {
            expr_references_chosen_x(value)
        }
        ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor
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
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => false,
    }
}

fn trigger_definition_references_x(trigger: &TriggerDefinition) -> bool {
    trigger
        .execute
        .as_ref()
        .is_some_and(|exec| ability_definition_references_x(exec))
}

fn replacement_definition_references_x(replacement: &ReplacementDefinition) -> bool {
    replacement
        .execute
        .as_ref()
        .is_some_and(|exec| ability_definition_references_x(exec))
}

fn quantity_ref_references_x(qty: &QuantityRef) -> bool {
    matches!(qty, QuantityRef::Variable { name } if name == "X")
}

/// Walk every `QuantityExpr` reachable from `effect` and return true if any
/// resolves through `QuantityRef::Variable { name: "X" }` (directly, wrapped,
/// or — for granted statics — via `CostXPaid`) or through a subject/target
/// filter whose `Cmc`/`Counters` threshold references X. Delegates the
/// per-expression test to `QuantityExpr::contains_x`, the engine's single
/// authority, so the AI scores X exactly as the engine evaluates it.
pub(crate) fn effect_references_x(effect: &Effect) -> bool {
    match effect {
        Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. } => amount.contains_x(),
        Effect::Draw { count, .. }
        | Effect::Mill { count, .. }
        | Effect::Discard { count, .. }
        | Effect::Scry { count, .. }
        | Effect::Surveil { count, .. }
        | Effect::Sacrifice { count, .. }
        | Effect::Dig { count, .. }
        | Effect::ExileTop { count, .. }
        | Effect::PutAtLibraryPosition { count, .. }
        | Effect::PutCounter { count, .. }
        | Effect::PutCounterAll { count, .. }
        | Effect::CopyTokenOf { count, .. }
        | Effect::SearchLibrary { count, .. } => count.contains_x(),
        Effect::Token {
            count,
            enter_with_counters,
            ..
        } => count.contains_x() || enter_with_counters.iter().any(|(_, qty)| qty.contains_x()),
        // RC1: `GenericEffect` carries its X payoff inside granted static
        // definitions (Mirror Entity's dynamic P/T via `CostXPaid`; Day of
        // Black Sun's `RemoveAllAbilities` over an `X`-filtered subject) or in a
        // top-level `Cmc`/`Counters`-X subject filter.
        Effect::GenericEffect {
            static_abilities,
            target,
            ..
        } => {
            static_abilities.iter().any(static_definition_references_x)
                || target.as_ref().is_some_and(target_filter_references_x)
        }
        _ => false,
    }
}

/// True when `effect`'s numeric amount/count references
/// `QuantityRef::PreviousEffectAmount` — the result of the immediately
/// preceding chain effect. Mirrors [`effect_references_x`]'s
/// per-variant amount/count extraction. Used by the gate's chain-aware no-op
/// walk to treat "gain life equal to the life lost this way" (Exsanguinate) as
/// 0 at X=0 whenever its predecessor is X-scaled.
pub(crate) fn effect_references_previous_amount(effect: &Effect) -> bool {
    match effect {
        Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. } => expr_contains_previous_amount(amount),
        Effect::Draw { count, .. }
        | Effect::Mill { count, .. }
        | Effect::Discard { count, .. }
        | Effect::Scry { count, .. }
        | Effect::Surveil { count, .. }
        | Effect::Sacrifice { count, .. }
        | Effect::Dig { count, .. }
        | Effect::ExileTop { count, .. }
        | Effect::PutAtLibraryPosition { count, .. }
        | Effect::PutCounter { count, .. }
        | Effect::PutCounterAll { count, .. }
        | Effect::CopyTokenOf { count, .. }
        | Effect::SearchLibrary { count, .. } => expr_contains_previous_amount(count),
        Effect::Token {
            count,
            enter_with_counters,
            ..
        } => {
            expr_contains_previous_amount(count)
                || enter_with_counters
                    .iter()
                    .any(|(_, qty)| expr_contains_previous_amount(qty))
        }
        _ => false,
    }
}

/// True when `expr` references the chosen X either as the on-stack
/// `Variable { name: "X" }` (engine authority) or as the post-announcement
/// `CostXPaid` (CR 107.3m) carried on the granting object.
pub(crate) fn expr_references_chosen_x(expr: &QuantityExpr) -> bool {
    expr.contains_x() || expr_matches_ref(expr, is_cost_x_paid)
}

fn expr_contains_previous_amount(expr: &QuantityExpr) -> bool {
    expr_matches_ref(expr, is_previous_amount)
}

fn is_cost_x_paid(qty: &QuantityRef) -> bool {
    matches!(qty, QuantityRef::CostXPaid)
}

fn is_previous_amount(qty: &QuantityRef) -> bool {
    matches!(qty, QuantityRef::PreviousEffectAmount)
}

/// Structural recursion over a `QuantityExpr` tree, returning true if any leaf
/// `Ref { qty }` satisfies `pred`. Mirrors the wrapper set walked by
/// `QuantityExpr::contains_x` so a new `QuantityExpr` variant forces this to be
/// reconsidered rather than silently returning false.
fn expr_matches_ref(expr: &QuantityExpr, pred: fn(&QuantityRef) -> bool) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => pred(qty),
        QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::UpTo { max: inner }
        | QuantityExpr::Power {
            exponent: inner, ..
        } => expr_matches_ref(inner, pred),
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(|e| expr_matches_ref(e, pred))
        }
        QuantityExpr::Difference { left, right } => {
            expr_matches_ref(left, pred) || expr_matches_ref(right, pred)
        }
        QuantityExpr::Fixed { .. } => false,
    }
}

/// True when a subject/target filter's `Cmc` or `Counters` threshold references
/// X (Day of Black Sun's "mana value X or less"). Walks `Not`/`Or`/`And`
/// composition; other structural filters carry no numeric threshold.
pub(crate) fn target_filter_references_x(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.properties.iter().any(filter_prop_references_x),
        TargetFilter::Not { filter } => target_filter_references_x(filter),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_references_x)
        }
        _ => false,
    }
}

fn filter_prop_references_x(prop: &FilterProp) -> bool {
    match prop {
        FilterProp::Cmc { value, .. } => value.contains_x(),
        FilterProp::Counters { count, .. } => count.contains_x(),
        _ => false,
    }
}
