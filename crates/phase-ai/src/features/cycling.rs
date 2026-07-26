//! Cycling feature — structural detection of a "cycling matters" payoff deck.
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `Keyword::Cycling(CyclingCost)` at `crates/engine/src/types/keywords.rs:694`
//!   and `Keyword::Typecycling { .. }` at `keywords.rs:966` (CR 702.29a/e) — the
//!   cyclable cards (the enablers).
//! - `TriggerMode::Cycled` at `crates/engine/src/types/triggers.rs:397`
//!   (CR 702.29c: "when you cycle this card") and `TriggerMode::CycledOrDiscarded`
//!   at `triggers.rs:400` (CR 702.29d: "whenever you cycle or discard a card") —
//!   the payoffs.
//! - `TriggerDefinition.valid_card` / `.valid_target` (`Option<TargetFilter>`) at
//!   `ability.rs:4522`/`:4539` — used to keep only controller-scoped, non-self
//!   ("whenever you cycle A card") engine payoffs.
//!
//! No parser remediation required — every axis is expressible over existing
//! typed AST.
//!
//! ## Why this axis exists
//!
//! Cycling is card-neutral selection, so the AI's generic priors treat it as
//! marginal — and `CyclingDisciplinePolicy` only adds *patience* (it penalises
//! cycling away a needed land), while `self_cost_value` explicitly defers
//! cycling value (`self_cost_cycling_deferred`). Nothing models the *upside*: in
//! a deck with a "whenever you cycle a card" engine (Astral Drift, Drannith
//! Stinger, New Perspectives), every cycle is a repeatable value trigger, and
//! the AI should cycle eagerly. This axis lets a policy see that engine.
//!
//! ## Boundary with `spellslinger_prowess` / `mill`
//!
//! Spellslinger counts *spell-cast* triggers; cycling triggers on the cycling
//! keyword action (CR 702.29c), a disjoint event. A cycling card that is also an
//! instant/sorcery can read on both axes — that overlap is intentional and the
//! axes stay independent.

use engine::game::DeckEntry;
use engine::types::ability::{TargetFilter, TriggerDefinition};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::triggers::TriggerMode;

use crate::features::commitment;

/// Commitment at or above which "cycling matters" is a real plan for this deck
/// rather than incidental card smoothing. Gates `CyclingPayoffPolicy::activation`.
pub const CYCLING_PAYOFF_FLOOR: f32 = 0.35;

/// CR 702.29: per-deck cycling-payoff classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.keywords` and `CardFace.triggers` — never by card name.
#[derive(Debug, Clone, Default)]
pub struct CyclingFeature {
    /// Cyclable cards — CR 702.29a Cycling / CR 702.29e Typecycling. The
    /// enablers that feed the payoff engine.
    pub source_count: u32,
    /// Permanents carrying a "whenever you cycle a card" engine trigger
    /// (CR 702.29c/d), controller-scoped and not self-referential — the payoffs
    /// that make cycling actively good.
    pub payoff_count: u32,
    /// `0.0..=1.0` — how central cycling-as-a-payoff is to this deck. Consumed by
    /// `CyclingPayoffPolicy::activation` as the single scaling knob.
    pub commitment: f32,
    /// Names of the detected payoff engines. NOT used for classification — that
    /// already happened against the AST. Identity lookup only, so the policy can
    /// re-find a payoff on the battlefield (`GameObject` carries no `triggers`
    /// field). One entry per UNIQUE face, never per playset copy.
    pub payoff_names: Vec<String>,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> CyclingFeature {
    if deck.is_empty() {
        return CyclingFeature::default();
    }

    let mut source_count = 0u32;
    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;
    let mut payoff_names: Vec<String> = Vec::new();

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        if is_cycle_source_parts(&face.keywords) {
            source_count = source_count.saturating_add(entry.count);
        }
        if is_cycle_payoff_parts(&face.triggers) {
            payoff_count = payoff_count.saturating_add(entry.count);
            // Identity list: one push per UNIQUE face, not per copy.
            if !payoff_names.contains(&face.name) {
                payoff_names.push(face.name.clone());
            }
        }
    }

    let commitment = compute_commitment(source_count, payoff_count, total_nonland);

    CyclingFeature {
        source_count,
        payoff_count,
        commitment,
        payoff_names,
    }
}

/// CR 702.29a/e: the face is cyclable — it carries Cycling or Typecycling.
pub(crate) fn is_cycle_source_parts(keywords: &[Keyword]) -> bool {
    keywords
        .iter()
        .any(|k| matches!(k, Keyword::Cycling(_) | Keyword::Typecycling { .. }))
}

/// CR 702.29c/d: the face carries a "whenever you cycle a card" engine trigger —
/// a repeatable payoff, not a one-shot self-cycle bonus.
pub(crate) fn is_cycle_payoff_parts<'a>(
    triggers: impl IntoIterator<Item = &'a TriggerDefinition>,
) -> bool {
    triggers.into_iter().any(trigger_is_cycle_payoff)
}

fn trigger_is_cycle_payoff(t: &TriggerDefinition) -> bool {
    // 1. Mode fires on a cycle event (CR 702.29c/d).
    if !matches!(t.mode, TriggerMode::Cycled | TriggerMode::CycledOrDiscarded) {
        return false;
    }
    // 2. Caster-scoped only: an opponent-scoped "whenever an opponent cycles"
    //    punisher is not your payoff.
    if !matches!(&t.valid_target, None | Some(TargetFilter::Controller)) {
        return false;
    }
    // 3. Exclude the pure self-cycle bonus ("when you cycle THIS card"): that is
    //    a cyclable card with upside (already counted as a source), not a
    //    battlefield engine that rewards cycling other cards.
    !matches!(&t.valid_card, Some(TargetFilter::SelfRef))
}

/// Calibration: a dedicated cycling-payoff deck (e.g. Pioneer/Historic cycling:
/// ~18 cyclers + ~6 engines like Astral Drift / Drannith Stinger over ~36
/// nonland) → commitment ≈ 0.85. Anti-calibration: a control deck that plays two
/// cycling lands and no engine → below `CYCLING_PAYOFF_FLOOR`; a deck with an
/// engine but no cyclers, or cyclers but no engine → 0.0.
///
/// Geometric mean over (source, payoff): BOTH pillars are mandatory. Cyclers
/// with no engine is just card smoothing (`CyclingDisciplinePolicy` governs it);
/// an engine with no cyclers never triggers.
fn compute_commitment(source_count: u32, payoff_count: u32, total_nonland: u32) -> f32 {
    // ~18 cyclers per 60 nonland is a fully-committed cycling shell.
    let source_density = (commitment::density_per_60(source_count, total_nonland) / 18.0).min(1.0);
    // ~6 engine payoffs per 60 nonland is a fully-committed payoff base.
    let payoff_density = (commitment::density_per_60(payoff_count, total_nonland) / 6.0).min(1.0);
    commitment::geometric_mean(&[source_density, payoff_density])
}
