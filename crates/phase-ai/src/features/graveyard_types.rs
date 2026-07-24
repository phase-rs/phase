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
//! - Enablers: `Effect::Mill`, `Effect::Discard`, `Effect::DiscardCard`,
//!   `Effect::Surveil`, and `Effect::Dig { rest_destination: Graveyard }` —
//!   classified by the shared `reanimator::effect_fills_own_graveyard`
//!   authority, and read from BOTH `abilities` and `triggers[*].execute`.
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
    AbilityCondition, AbilityDefinition, CardTypeSetSource, Comparator, CountScope, QuantityExpr,
    QuantityRef, StaticCondition, TriggerCondition, TriggerDefinition, ZoneRef,
};
use engine::types::card_type::CoreType;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment at or above which graveyard type-diversity is a real plan rather
/// than an incidental Goyf. Gates `GraveyardTypesPolicy::activation`.
pub const GRAVEYARD_TYPES_FLOOR: f32 = 0.35;

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
    /// (Consuming Blob-likes).
    pub scaling_payoff_count: u32,
    /// Cards that put cards into the controller's own graveyard — self-mill,
    /// self-discard, surveil, or a rest-to-graveyard dig (CR 701.17 / CR 701.25
    /// / CR 701.20e). Counted from ability chains AND trigger bodies.
    pub enabler_count: u32,
    /// The highest threshold any *threshold* payoff in the deck asks for, or
    /// `None` when the deck has no threshold payoff at all. A descend 8 deck
    /// must not think it is finished at four card types; a scaling-only deck
    /// (Consuming Blob) has no threshold to "finish" and must keep
    /// being rewarded for diversity — so absence is modelled distinctly from a
    /// concrete four, never invented.
    pub highest_threshold: Option<u32>,
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
    let mut highest_threshold: Option<u32> = None;
    let mut total_nonland = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        // A graveyard-type gate can ride any of THREE carriers, and they are
        // DISTINCT enums that merely share the same comparison shape, so each
        // gets its own extractor rather than a forced conversion:
        //   * `StaticDefinition.condition`  (Backwoods Survivalists)
        //   * `TriggerDefinition.condition` (Autumnal Gloom)
        //   * `AbilityDefinition.condition` (Traverse the Ulvenwald) — walked
        //     down the sub_ability/else_ability chain, where the gate usually
        //     sits rather than at the ability root.
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
            .chain(
                face.abilities
                    .iter()
                    .filter_map(ability_chain_graveyard_type_threshold),
            )
            .max();

        let scales = face_reads_graveyard_types(face);

        if let Some(threshold) = threshold {
            threshold_payoff_count = threshold_payoff_count.saturating_add(entry.count);
            // `Option<u32>` orders `None < Some(_)`, so `max` keeps the highest
            // real threshold and never regresses to a fabricated default.
            highest_threshold = highest_threshold.max(Some(threshold));
        } else if scales {
            // Only a payoff with NO threshold is a scaling payoff — otherwise a
            // delirium card would be counted on both axes.
            scaling_payoff_count = scaling_payoff_count.saturating_add(entry.count);
        }
        // One push per UNIQUE face, and once even when both axes could fire.
        if threshold.is_some() || scales {
            payoff_names.push(face.name.clone());
        }

        if fills_own_graveyard_parts(&face.abilities, &face.triggers) {
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
        // No fabricated fallback: `None` when the deck has no threshold payoff.
        highest_threshold,
        commitment,
        payoff_names,
    }
}

/// Calibration: a Modern delirium shell (8 threshold payoffs + 2 scaling
/// payoffs + 8 enablers over 37 nonland) → commitment ≈ 0.90.
/// Anti-calibration: a deck running one incidental scaling body and no
/// enablers → well below `GRAVEYARD_TYPES_FLOOR`; UW control → 0.0.
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

/// CR 205.2a: read the threshold N out of a `StaticCondition` whose comparison
/// counts distinct card types in the controller's graveyard — but only when the
/// comparison is a genuine positive "N or more types" gate.
///
/// Returns `None` for any other condition shape, for an opponent-scoped count (a
/// card that punishes an OPPONENT's diverse graveyard is not this deck's plan),
/// and for a comparison whose truth semantics reward FEWER or an EXACT number of
/// types (see [`positive_graveyard_threshold`]).
fn static_graveyard_type_threshold(condition: &StaticCondition) -> Option<u32> {
    match condition {
        StaticCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => positive_graveyard_threshold(lhs, *comparator, rhs),
        // CR 109.3: an `And` gates on EVERY constraint, so a delirium conjunct
        // is mandatory — take the highest graveyard threshold present.
        StaticCondition::And { conditions } => conditions
            .iter()
            .filter_map(static_graveyard_type_threshold)
            .max(),
        // An `Or` is satisfied by ANY branch, so a graveyard threshold is a
        // mandatory gate only when EVERY branch is one — then the easiest
        // (minimum) is what self-mill must reach. A single non-graveyard branch
        // means the payoff can fire without delirium, so it is not our plan.
        StaticCondition::Or { conditions } => {
            all_graveyard_thresholds_min(conditions.iter().map(static_graveyard_type_threshold))
        }
        // CR 205.2a: negating "N or more types" is a "fewer than N" condition —
        // it rewards a SMALLER graveyard, the opposite of a delirium payoff.
        StaticCondition::Not { .. } => None,
        _ => None,
    }
}

/// CR 205.2a: the `TriggerCondition` twin of [`static_graveyard_type_threshold`]
/// — Autumnal Gloom carries its delirium clause on the trigger, not the static.
fn trigger_graveyard_type_threshold(condition: &TriggerCondition) -> Option<u32> {
    match condition {
        TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } => positive_graveyard_threshold(lhs, *comparator, rhs),
        TriggerCondition::And { conditions } => conditions
            .iter()
            .filter_map(trigger_graveyard_type_threshold)
            .max(),
        TriggerCondition::Or { conditions } => {
            all_graveyard_thresholds_min(conditions.iter().map(trigger_graveyard_type_threshold))
        }
        TriggerCondition::Not { .. } => None,
        _ => None,
    }
}

/// CR 205.2a: the `AbilityCondition` twin of [`static_graveyard_type_threshold`]
/// — the THIRD condition carrier, `AbilityDefinition.condition`.
///
/// Traverse the Ulvenwald is the shape that forces this: its delirium gate is
/// not on `abilities[0].condition` but on `abilities[0].sub_ability.condition`,
/// because *"Delirium — If there are four or more card types among cards in your
/// graveyard, **instead** search…"* replaces the second clause and so lowers
/// onto the sub-ability, wrapped in `ConditionInstead`. Walking only the top
/// level would still miss it — see [`ability_chain_graveyard_type_threshold`].
fn ability_graveyard_type_threshold(condition: &AbilityCondition) -> Option<u32> {
    match condition {
        AbilityCondition::QuantityCheck {
            lhs,
            comparator,
            rhs,
        } => positive_graveyard_threshold(lhs, *comparator, rhs),
        // CR 608.2c: "instead" wraps the gate without changing its polarity.
        AbilityCondition::ConditionInstead { inner } => ability_graveyard_type_threshold(inner),
        AbilityCondition::And { conditions } => conditions
            .iter()
            .filter_map(ability_graveyard_type_threshold)
            .max(),
        AbilityCondition::Or { conditions } => {
            all_graveyard_thresholds_min(conditions.iter().map(ability_graveyard_type_threshold))
        }
        AbilityCondition::Not { .. } => None,
        _ => None,
    }
}

/// Walk an ability and its `sub_ability` / `else_ability` chain, returning the
/// highest graveyard-type threshold gating any link.
///
/// The chain walk is the load-bearing part: the gate frequently sits on a
/// sub-ability rather than the ability root (Traverse the Ulvenwald), so a
/// top-level-only read returns `None` for the very cards this axis exists for.
fn ability_chain_graveyard_type_threshold(ability: &AbilityDefinition) -> Option<u32> {
    let here = ability
        .condition
        .as_ref()
        .and_then(ability_graveyard_type_threshold);
    let sub = ability
        .sub_ability
        .as_deref()
        .and_then(ability_chain_graveyard_type_threshold);
    let alt = ability
        .else_ability
        .as_deref()
        .and_then(ability_chain_graveyard_type_threshold);
    here.into_iter().chain(sub).chain(alt).max()
}

/// The `Or`-combinator rule shared by both extractors: yield a threshold only
/// when EVERY disjunct is itself a graveyard threshold, and then the minimum —
/// the easiest branch self-mill can satisfy. Any `None` child (a branch that
/// enables the payoff without delirium) collapses the whole `Or` to `None`.
fn all_graveyard_thresholds_min(children: impl Iterator<Item = Option<u32>>) -> Option<u32> {
    children
        .collect::<Option<Vec<u32>>>()
        .and_then(|thresholds| thresholds.into_iter().min())
}

/// CR 205.2a: normalize a single `count CMP N` comparison into the delirium
/// threshold it mandates — the least graveyard-type count that satisfies it — or
/// `None` when the comparison is not a positive "N or more types" gate.
///
/// Handles both orientations (`types >= N`, `N <= types`) and the strict-bound
/// off-by-one (`types > N` ⟺ `types >= N+1`). Rejects `<`, `<=`, `=`, `!=`
/// against the graveyard count: those reward FEWER or an EXACT number of types,
/// so self-milling toward N is not what they want.
fn positive_graveyard_threshold(
    lhs: &QuantityExpr,
    comparator: Comparator,
    rhs: &QuantityExpr,
) -> Option<u32> {
    // Orient so the graveyard-type count is the subject and the constant is the
    // bound; flip the comparator when the count sits on the right.
    let (bound, oriented) = if quantity_reads_own_graveyard_types(lhs) {
        (fixed_quantity_value(rhs)?, comparator)
    } else if quantity_reads_own_graveyard_types(rhs) {
        (fixed_quantity_value(lhs)?, flip_comparator(comparator))
    } else {
        return None;
    };
    // `oriented` now reads `types CMP bound`; a delirium gate is a lower bound.
    match oriented {
        // types >= bound → needs `bound` types (a zero bound gates nothing).
        Comparator::GE if bound > 0 => Some(bound as u32),
        // types > bound  → needs `bound + 1` types.
        Comparator::GT if bound >= 0 => Some((bound + 1) as u32),
        Comparator::GT
        | Comparator::GE
        | Comparator::LT
        | Comparator::LE
        | Comparator::EQ
        | Comparator::NE => None,
    }
}

/// The `i32` behind a `QuantityExpr::Fixed`, or `None` for any dynamic value.
fn fixed_quantity_value(expr: &QuantityExpr) -> Option<i32> {
    match expr {
        QuantityExpr::Fixed { value } => Some(*value),
        _ => None,
    }
}

/// Reflect a comparator across its operands (`a CMP b` ⟺ `b flip(CMP) a`), so a
/// `constant CMP count` comparison can be re-read as `count CMP constant`.
fn flip_comparator(comparator: Comparator) -> Comparator {
    match comparator {
        Comparator::GT => Comparator::LT,
        Comparator::LT => Comparator::GT,
        Comparator::GE => Comparator::LE,
        Comparator::LE => Comparator::GE,
        Comparator::EQ => Comparator::EQ,
        Comparator::NE => Comparator::NE,
    }
}

/// True when a `QuantityExpr` reads distinct card types in the controller's
/// OWN graveyard, at any nesting depth (Consuming Blob wraps it in `Offset`).
///
/// Only own-graveyard scopes qualify. `CountScope::All` is deliberately
/// excluded: the policy's `distinct_graveyard_types` counts only the AI's owned
/// objects, so classifying an all-graveyards payoff (Tarmogoyf-class) as an
/// own-graveyard plan would let an opponent satisfy it while the policy keeps
/// rewarding self-mill against a different quantity. Opponent- and
/// iterated-player scopes are likewise not this deck's own plan.
fn quantity_reads_own_graveyard_types(expr: &QuantityExpr) -> bool {
    match expr {
        QuantityExpr::Ref { qty } => matches!(
            qty,
            QuantityRef::DistinctCardTypes {
                source: CardTypeSetSource::Zone {
                    zone: ZoneRef::Graveyard,
                    // CR 108.3 + CR 109.5: "your graveyard" — the controller as
                    // owner over a non-battlefield zone.
                    scope: CountScope::Controller | CountScope::Owner,
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

/// CR 701.17 + CR 701.25 + CR 701.20e + CR 404.1: true when any ability chain
/// OR trigger body puts cards into the CONTROLLER's own graveyard — self-mill,
/// self-discard, surveil, or a rest-to-graveyard dig.
///
/// Both carriers matter and missing either zeroes the axis: the archetypal
/// enabler (Stitcher's Supplier) is `abilities: []` with mill *triggers*, and
/// `compute_commitment`'s geometric mean collapses to `0.0` when
/// `enabler_count == 0`, which drops the deck below `GRAVEYARD_TYPES_FLOOR` and
/// switches the policy off entirely. Mirrors `tokens_wide::is_token_generator_parts`.
///
/// The per-effect question delegates to
/// [`crate::features::reanimator::effect_fills_own_graveyard`] — the single
/// authority both graveyard axes share, so they cannot drift on which effects
/// and scopes count. An opponent-scoped mill is excluded there: filling an
/// opponent's graveyard does nothing for this deck's threshold.
pub(crate) fn fills_own_graveyard_parts(
    abilities: &[AbilityDefinition],
    triggers: &[TriggerDefinition],
) -> bool {
    let chain_fills = |ability: &AbilityDefinition| {
        collect_chain_effects(ability)
            .iter()
            .copied()
            .any(crate::features::reanimator::effect_fills_own_graveyard)
    };
    if abilities.iter().any(&chain_fills) {
        return true;
    }
    // CR 603.6a: "when this enters, mill three" lives on the trigger body.
    triggers
        .iter()
        .any(|trigger| trigger.execute.as_deref().is_some_and(&chain_fills))
}

/// The live-policy companion to [`fills_own_graveyard_parts`]: the same
/// single-authority question asked over an already-resolved effect chain (a
/// cast's spell abilities plus its immediate ETB triggers, or one activated
/// ability), where the carrier split has already been made by the caller.
pub(crate) fn abilities_fill_own_graveyard<'a>(
    abilities: impl IntoIterator<Item = &'a AbilityDefinition>,
) -> bool {
    abilities.into_iter().any(|ability| {
        collect_chain_effects(ability)
            .iter()
            .copied()
            .any(crate::features::reanimator::effect_fills_own_graveyard)
    })
}
