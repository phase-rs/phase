//! Graveyard card-type-diversity feature — the delirium / descend / Goyf axis.
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `QuantityRef::DistinctCardTypes { source: CardTypeSetSource }` at
//!   `crates/engine/src/types/ability.rs:5565` (CR 205.2a card types).
//! - `CardTypeSetSource::Zone { zone: ZoneRef, scope: CountScope }` at
//!   `crates/engine/src/types/ability.rs:5272` (CR 109.2a + CR 400.1) — the
//!   graveyard scoping used by every card in this class.
//! - Threshold payoffs carry `StaticCondition::QuantityComparison { lhs, comparator,
//!   rhs }` whose `lhs` is that quantity (Backwoods Survivalists, Autumnal Gloom).
//! - Scaling payoffs read the same quantity as a dynamic magnitude with no
//!   threshold at all (Consuming Blob's `SetDynamicPower`).
//! - Enablers: `Effect::Mill { target, destination }` (`ability.rs:10102`),
//!   `Effect::DiscardCard { target }` (`:10096`), `Effect::Discard { target }`
//!   (`:11134`), `Effect::Surveil { target }` (`:10377`).
//!
//! No parser remediation required.
//!
//! ## Why this axis exists
//!
//! CR 207.2c lists delirium, descend, threshold and undergrowth as **ability
//! words** — they have no rules meaning, so the mechanical content is entirely
//! the underlying "N or more card types among cards in your graveyard"
//! condition. 95 cards in the corpus read that quantity, and nothing in the AI
//! modelled graveyard type-diversity as a resource: a self-mill that turns a
//! delirium payoff on scored exactly the same as one that did not.
//!
//! ## Boundary with `reanimator`
//!
//! `reanimator` detects graveyard *recursion targets* — what is worth bringing
//! back. This axis measures the *type spread* of the graveyard, which is a
//! different resource: a graveyard of four creatures is excellent for
//! reanimator and useless for delirium. The axes are independent by design.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, CardTypeSetSource, ControllerRef, CountScope, Effect, QuantityExpr,
    QuantityRef, StaticCondition, TargetFilter, TriggerCondition, ZoneRef,
};
use engine::types::card_type::CoreType;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment at or above which graveyard type-diversity is a real plan rather
/// than an incidental Goyf. Gates `GraveyardTypesPolicy::activation`.
pub const GRAVEYARD_TYPES_FLOOR: f32 = 0.35;

/// CR 205.2a: the delirium threshold printed on essentially every card in this
/// class. Used only as the fallback when a payoff's threshold cannot be read.
pub const DEFAULT_DELIRIUM_THRESHOLD: u32 = 4;

/// CR 207.2c + CR 205.2a: per-deck graveyard type-diversity classification.
///
/// Detection is structural over `CardFace.static_abilities`, `.triggers` and
/// `.abilities` — never by card name.
#[derive(Debug, Clone, Default)]
pub struct GraveyardTypesFeature {
    /// Payoffs gated on a threshold ("four or more card types among cards in
    /// your graveyard") — delirium, descend N, threshold.
    pub threshold_payoff_count: u32,
    /// Payoffs that scale continuously with the count and have no threshold
    /// (Consuming Blob, Tarmogoyf-likes).
    pub scaling_payoff_count: u32,
    /// Cards that put cards into the controller's own graveyard — self-mill,
    /// self-discard, surveil (CR 701.17 / CR 701.25).
    pub enabler_count: u32,
    /// The highest threshold any payoff in the deck asks for. A descend 8 deck
    /// must not think it is finished at four card types.
    pub highest_threshold: u32,
    /// `0.0..=1.0` — how central the axis is. Consumed by
    /// `GraveyardTypesPolicy::activation` as the single scaling knob.
    pub commitment: f32,
    /// Names of detected payoffs. NOT used for classification — that already
    /// happened against the AST. Identity lookup only.
    pub payoff_names: Vec<String>,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> GraveyardTypesFeature {
    if deck.is_empty() {
        return GraveyardTypesFeature::default();
    }

    let mut threshold_payoff_count = 0u32;
    let mut scaling_payoff_count = 0u32;
    let mut enabler_count = 0u32;
    let mut highest_threshold = 0u32;
    let mut total_nonland = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // `StaticDefinition.condition` and `TriggerDefinition.condition` are
        // DISTINCT enums that happen to share the `QuantityComparison` shape,
        // so each gets its own extractor rather than a forced conversion.
        let threshold = face
            .static_abilities
            .iter()
            .filter_map(|def| def.condition.as_ref())
            .filter_map(static_graveyard_type_threshold)
            .chain(
                face.triggers
                    .iter()
                    .filter_map(|t| t.condition.as_ref())
                    .filter_map(trigger_graveyard_type_threshold),
            )
            .max();

        let scales = face_reads_graveyard_types(face);

        if let Some(threshold) = threshold {
            threshold_payoff_count = threshold_payoff_count.saturating_add(entry.count);
            highest_threshold = highest_threshold.max(threshold);
        } else if scales {
            // Only a payoff with NO threshold is a scaling payoff — otherwise a
            // delirium card would be counted on both axes.
            scaling_payoff_count = scaling_payoff_count.saturating_add(entry.count);
        }
        // One push per UNIQUE face, and once even when both axes could fire.
        if threshold.is_some() || scales {
            payoff_names.push(face.name.clone());
        }

        if fills_own_graveyard_parts(&face.abilities) {
            enabler_count = enabler_count.saturating_add(entry.count);
        }
    }

    let commitment = compute_commitment(
        threshold_payoff_count,
        scaling_payoff_count,
        enabler_count,
        total_nonland,
    );

    GraveyardTypesFeature {
        threshold_payoff_count,
        scaling_payoff_count,
        enabler_count,
        highest_threshold: if highest_threshold == 0 {
            DEFAULT_DELIRIUM_THRESHOLD
        } else {
            highest_threshold
        },
        commitment,
        payoff_names,
    }
}

/// Calibration: a Modern delirium shell (8 threshold payoffs + 2 scaling
/// payoffs + 8 enablers over 37 nonland) → commitment ≈ 0.90.
/// Anti-calibration: a deck running one incidental Tarmogoyf and no enablers →
/// well below `GRAVEYARD_TYPES_FLOOR`; UW control → 0.0.
///
/// Geometric mean over (payoff, enabler): unlike poison, BOTH pillars are
/// mandatory here. Payoffs with no enablers never turn on reliably, and
/// enablers with no payoff are just self-mill — neither is this archetype.
fn compute_commitment(
    threshold_payoff_count: u32,
    scaling_payoff_count: u32,
    enabler_count: u32,
    total_nonland: u32,
) -> f32 {
    let payoff_density = commitment::weighted_sum(&[
        (
            1.0 / 8.0,
            commitment::density_per_60(threshold_payoff_count, total_nonland),
        ),
        // A scaling payoff wants a big graveyard but never strands, so it is a
        // weaker signal of intent than a threshold payoff.
        (
            0.5 / 8.0,
            commitment::density_per_60(scaling_payoff_count, total_nonland),
        ),
    ]);
    let enabler_density =
        (commitment::density_per_60(enabler_count, total_nonland) / 10.0).min(1.0);

    commitment::geometric_mean(&[payoff_density, enabler_density])
}

/// CR 205.2a: read the threshold N out of a `QuantityComparison` condition
/// whose left side counts distinct card types in the controller's graveyard.
///
/// Returns `None` for any other condition shape, and for an opponent-scoped
/// count — a card that punishes an OPPONENT's diverse graveyard is not a
/// payoff for this deck's own plan.
fn static_graveyard_type_threshold(condition: &StaticCondition) -> Option<u32> {
    match condition {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            if !quantity_reads_own_graveyard_types(lhs) {
                return None;
            }
            match rhs {
                QuantityExpr::Fixed { value } if *value > 0 => Some(*value as u32),
                _ => None,
            }
        }
        // CR 109.3: a conjunction gates on every constraint, so a delirium
        // clause nested in an `And`/`Or` still identifies the payoff.
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => conditions
            .iter()
            .filter_map(static_graveyard_type_threshold)
            .max(),
        StaticCondition::Not { condition } => static_graveyard_type_threshold(condition),
        _ => None,
    }
}

/// CR 205.2a: the `TriggerCondition` twin of [`static_graveyard_type_threshold`]
/// — Autumnal Gloom carries its delirium clause on the trigger, not the static.
fn trigger_graveyard_type_threshold(condition: &TriggerCondition) -> Option<u32> {
    match condition {
        TriggerCondition::QuantityComparison { lhs, rhs, .. } => {
            if !quantity_reads_own_graveyard_types(lhs) {
                return None;
            }
            match rhs {
                QuantityExpr::Fixed { value } if *value > 0 => Some(*value as u32),
                _ => None,
            }
        }
        TriggerCondition::Not { condition } => trigger_graveyard_type_threshold(condition),
        _ => None,
    }
}

/// True when a `QuantityExpr` reads distinct card types in the controller's
/// own graveyard, at any nesting depth (Consuming Blob wraps it in `Offset`).
fn quantity_reads_own_graveyard_types(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => matches!(
            qty,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::Zone {
                    zone: ZoneRef::Graveyard,
                    scope: CountScope::Controller | CountScope::All,
                },
            }
        ),
        QuantityExpr::Fixed { .. } => false,
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_reads_own_graveyard_types(inner),
        QuantityExpr::UpTo { max } => quantity_reads_own_graveyard_types(max),
        QuantityExpr::Power { exponent, .. } => quantity_reads_own_graveyard_types(exponent),
        QuantityExpr::Difference { left, right } => {
            quantity_reads_own_graveyard_types(left) || quantity_reads_own_graveyard_types(right)
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            exprs.iter().any(quantity_reads_own_graveyard_types)
        }
    }
}

/// True when any continuous modification or effect on the face scales off the
/// graveyard type count (Consuming Blob's `SetDynamicPower`).
fn face_reads_graveyard_types(face: &engine::types::card::CardFace) -> bool {
    let in_statics = face.static_abilities.iter().any(|def| {
        def.modifications
            .iter()
            .filter_map(crate::features::graveyard_types::modification_quantity)
            .any(quantity_reads_own_graveyard_types)
    });
    in_statics
}

/// The dynamic magnitude carried by a continuous modification, if any. Mirrors
/// `game::quantity::continuous_modification_dynamic_quantity`.
pub(crate) fn modification_quantity(
    m: &engine::types::ability::ContinuousModification,
) -> Option<&QuantityExpr> {
    use engine::types::ability::ContinuousModification as CM;
    match m {
        CM::SetDynamicPower { value }
        | CM::SetDynamicToughness { value }
        | CM::SetPowerDynamic { value }
        | CM::SetToughnessDynamic { value }
        | CM::AddDynamicPower { value }
        | CM::AddDynamicToughness { value }
        | CM::AddDynamicKeyword { value, .. } => Some(value),
        _ => None,
    }
}

/// CR 701.17 + CR 701.25 + CR 404.1: an ability chain that puts cards into the
/// CONTROLLER's own graveyard — self-mill, self-discard, or surveil.
///
/// An opponent-scoped mill is deliberately excluded: filling an opponent's
/// graveyard does nothing for this deck's threshold (and actively helps a
/// Goyf-style symmetric count, which this axis does not chase).
pub(crate) fn fills_own_graveyard_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        collect_chain_effects(ability)
            .iter()
            .any(|effect| match effect {
                Effect::Mill {
                    target,
                    destination,
                    ..
                } => {
                    *destination == engine::types::zones::Zone::Graveyard
                        && filter_is_controller_scoped(target)
                }
                Effect::DiscardCard { target, .. } => filter_is_controller_scoped(target),
                Effect::Discard { target, .. } => filter_is_controller_scoped(target),
                Effect::Surveil { target, .. } => filter_is_controller_scoped(target),
                _ => false,
            })
    })
}

/// True when a `TargetFilter` resolves to the ability's own controller.
fn filter_is_controller_scoped(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Controller | TargetFilter::SelfRef => true,
        TargetFilter::Typed(typed) => matches!(typed.controller, Some(ControllerRef::You)),
        TargetFilter::Or { filters } => filters.iter().any(filter_is_controller_scoped),
        // CR 109.3: every constraint of a conjunction must hold.
        TargetFilter::And { filters } => filters.iter().all(filter_is_controller_scoped),
        _ => false,
    }
}
