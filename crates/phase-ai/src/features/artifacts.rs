//! Artifacts-matter feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `CoreType::Artifact` on `CardFace.card_type.core_types` (CR 301.1).
//! - `Keyword::Affinity(TypedFilter)` at `keywords.rs:699` (CR 702.41a):
//!   "Affinity for [text]" costs {1} less to cast for each [text] you control;
//!   the `TypedFilter.type_filters` reveal whether it is "for artifacts".
//! - `Keyword::Improvise` at `keywords.rs:720` (CR 702.126a): tap untapped
//!   artifacts to pay generic mana.
//! - `QuantityRef::ObjectCount { filter: TargetFilter }` at `ability.rs:3434`
//!   — "for each / equal to the number of [filter] you control"; an
//!   artifact-referencing filter is an artifact-count payoff (metalcraft and
//!   "for each artifact you control" shapes).
//! - `Effect::Token { types: Vec<String>, .. }` at `ability.rs:6747` (CR 111.1):
//!   an artifact-typed token (Treasure/Clue/Food/Servo/Thopter, …) grows the
//!   artifact board that the payoffs feed on.
//!
//! No parser remediation required — artifacts-matter cards classify structurally
//! using the existing typed AST; never by card name.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, Effect, QuantityExpr, QuantityRef, TargetFilter, TriggerDefinition,
    TypeFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment floor below which `ArtifactSynergyPolicy` opts out.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// CR 301 / CR 702.41a / CR 702.126a: Per-deck artifacts-matter classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.card_type`, `CardFace.keywords`, `CardFace.abilities`,
/// `CardFace.triggers`, and `CardFace.static_abilities` — never by card name.
/// Policies consume this feature to weight deploying artifacts and casting
/// affinity/improvise spells.
#[derive(Debug, Clone, Default)]
pub struct ArtifactsFeature {
    /// Cards that are artifacts — the raw density that affinity, improvise, and
    /// metalcraft-style payoffs feed on. CR 301.1.
    pub artifact_count: u32,
    /// Cards that REWARD a high artifact count: affinity-for-artifacts,
    /// improvise, or an artifact-count reference (`ObjectCount` over an
    /// artifact filter). CR 702.41a / CR 702.126a.
    pub payoff_count: u32,
    /// Cards that GROW the artifact board: artifact-token generators
    /// (Treasure/Clue/Food/Servo/Thopter, …). CR 111.1.
    pub enabler_count: u32,
    /// `0.0..=1.0` — how central the artifacts-matter plan is to this deck.
    /// Consumed by `ArtifactSynergyPolicy::activation` as the scaling knob.
    /// Driven by payoff + enabler density (a pile of artifacts with no payoff
    /// is not an artifacts-MATTER deck), so payoffs dominate.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// artifacts, artifact payoffs, and artifact-token enablers.
pub fn detect(deck: &[DeckEntry]) -> ArtifactsFeature {
    if deck.is_empty() {
        return ArtifactsFeature::default();
    }

    let mut artifact_count = 0u32;
    let mut payoff_count = 0u32;
    let mut enabler_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        let is_land = face.card_type.core_types.contains(&CoreType::Land);
        if !is_land {
            total_nonland = total_nonland.saturating_add(entry.count);
        }
        if face.card_type.core_types.contains(&CoreType::Artifact) {
            artifact_count = artifact_count.saturating_add(entry.count);
        }
        if is_artifact_payoff(face) {
            payoff_count = payoff_count.saturating_add(entry.count);
        }
        if is_artifact_enabler(face) {
            enabler_count = enabler_count.saturating_add(entry.count);
        }
    }

    // Payoffs are the intent signal; token enablers are supporting fuel. The
    // raw `artifact_count` is intentionally NOT a commitment pillar — an
    // artifact-heavy deck without payoffs (e.g., a Vehicles aggro deck) is not
    // an artifacts-MATTER deck. The policy reads `artifact_count` separately at
    // decision time.
    //
    // Weights are calibrated so a *sparse incidental* artifact package does NOT
    // cross `COMMITMENT_FLOOR` (0.30) and therefore stays inert — only a real
    // artifacts-matter commitment activates the policy:
    //   - payoff 0.12 → a single affinity/improvise/metalcraft card in a
    //     ~40–60-nonland deck yields ≈0.12–0.18 (< floor, inert); it takes
    //     roughly three payoff-equivalents to reach the floor.
    //   - enabler 0.03 → Treasure/Clue/Food makers are common in non-artifact
    //     decks, so they only add support and cannot activate the policy on
    //     their own at any realistic count.
    // See the positive/anti calibration anchors in `features/tests/artifacts.rs`.
    let commitment = commitment::weighted_sum(&[
        (
            0.12,
            commitment::density_per_60(payoff_count, total_nonland),
        ),
        (
            0.03,
            commitment::density_per_60(enabler_count, total_nonland),
        ),
    ]);

    ArtifactsFeature {
        artifact_count,
        payoff_count,
        enabler_count,
        commitment,
    }
}

/// A payoff rewards controlling many artifacts: affinity-for-artifacts,
/// improvise, or an effect whose quantity references the number of artifacts.
fn is_artifact_payoff(face: &CardFace) -> bool {
    is_artifact_cost_payoff_parts(&face.keywords)
        || face.abilities.iter().any(ability_counts_artifacts)
        || face.triggers.iter().any(trigger_counts_artifacts)
}

/// Single authority for the "this card's *keywords* make it an artifact-cost
/// payoff" classification — affinity-for-artifacts (CR 702.41a) or improvise
/// (CR 702.126a). Shared by deck-time `CardFace` detection
/// ([`is_artifact_payoff`]) and the live-game `ArtifactSynergyPolicy` so the
/// keyword set stays in sync between detector and policy. Operates on a
/// `&[Keyword]` slice so it applies equally to a `CardFace` and a `GameObject`.
pub(crate) fn is_artifact_cost_payoff_parts(keywords: &[Keyword]) -> bool {
    keywords.iter().any(keyword_is_artifact_payoff)
}

fn keyword_is_artifact_payoff(kw: &Keyword) -> bool {
    match kw {
        // CR 702.41a: "Affinity for artifacts" — cheaper per artifact you control.
        Keyword::Affinity(filter) => typed_filter_references_artifact(filter),
        // CR 702.126a: Improvise — tap artifacts to pay generic mana.
        Keyword::Improvise => true,
        _ => false,
    }
}

/// An ability whose effect-chain quantity references `ObjectCount` over an
/// artifact filter — "for each artifact you control" / "equal to the number of
/// artifacts you control".
fn ability_counts_artifacts(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .any(effect_counts_artifacts)
}

/// A trigger whose `execute` chain references an artifact count. CR 603.6a.
fn trigger_counts_artifacts(trigger: &TriggerDefinition) -> bool {
    trigger.execute.as_ref().is_some_and(|exec| {
        collect_chain_effects(exec)
            .iter()
            .any(effect_counts_artifacts)
    })
}

/// True if a count-bearing effect's quantity references the number of
/// artifacts. Covers the common "X = number of artifacts you control" shapes.
fn effect_counts_artifacts(effect: &&Effect) -> bool {
    match effect {
        Effect::Draw { count, .. } => quantity_counts_artifacts(count),
        Effect::DealDamage { amount, .. } => quantity_counts_artifacts(amount),
        Effect::GainLife { amount, .. } => quantity_counts_artifacts(amount),
        Effect::Token { count, .. } => quantity_counts_artifacts(count),
        _ => false,
    }
}

/// Recursively walks a `QuantityExpr`, returning true if any leaf is an
/// `ObjectCount` whose filter references artifacts. CR 702.41a-adjacent
/// count payoffs.
fn quantity_counts_artifacts(qty: &QuantityExpr) -> bool {
    match qty {
        QuantityExpr::Ref { qty } => quantity_ref_counts_artifacts(qty),
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. } => quantity_counts_artifacts(inner),
        QuantityExpr::Sum { exprs } => exprs.iter().any(quantity_counts_artifacts),
        QuantityExpr::UpTo { max } => quantity_counts_artifacts(max),
        _ => false,
    }
}

fn quantity_ref_counts_artifacts(qref: &QuantityRef) -> bool {
    match qref {
        QuantityRef::ObjectCount { filter } => target_filter_references_artifact(filter),
        _ => false,
    }
}

/// An enabler grows the artifact board: an artifact-token generator
/// (Treasure/Clue/Food/Servo/Thopter, …) in a direct ability or a trigger
/// chain. CR 111.1.
fn is_artifact_enabler(face: &CardFace) -> bool {
    face.abilities.iter().any(|a| {
        collect_chain_effects(a)
            .iter()
            .any(effect_is_artifact_token)
    }) || face.triggers.iter().any(|t| {
        t.execute.as_ref().is_some_and(|exec| {
            collect_chain_effects(exec)
                .iter()
                .any(effect_is_artifact_token)
        })
    })
}

/// True if the effect creates an artifact-typed token. Treasure, Clue, Food,
/// Servo, Thopter, etc. all carry `"Artifact"` in `Effect::Token.types`.
/// CR 111.1.
fn effect_is_artifact_token(effect: &&Effect) -> bool {
    matches!(effect, Effect::Token { types, .. } if types.iter().any(|t| t == "Artifact"))
}

// ─── Shared filter helpers (consumed by `ArtifactSynergyPolicy`) ──────────────

/// True if a `TypedFilter` references the Artifact card type. CR 301.1.
pub(crate) fn typed_filter_references_artifact(typed: &TypedFilter) -> bool {
    typed.type_filters.iter().any(type_filter_is_artifact)
}

/// True if a `TargetFilter` references the Artifact card type. Unwraps the
/// boolean `Or`/`And` combinators. CR 301.1.
pub(crate) fn target_filter_references_artifact(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_references_artifact(typed),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_references_artifact)
        }
        _ => false,
    }
}

fn type_filter_is_artifact(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Artifact => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_artifact),
        _ => false,
    }
}
