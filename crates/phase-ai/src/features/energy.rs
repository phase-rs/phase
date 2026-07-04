//! Energy-economy feature — structural detection over a deck's typed AST.
//!
//! Recognizes a deck built around the energy reserve (CR 107.14 / CR 122.1):
//! **producers** (`Effect::GainEnergy`) feeding **sinks/payoffs**
//! (`AbilityCost::PayEnergy`). Energy is a two-part economy — a deck only
//! "matters" on the energy axis when it can both generate *and* spend the
//! reserve — so commitment is the geometric mean of producer and sink density,
//! not a single-pillar count. This mirrors `AristocratsFeature`'s geometric-mean
//! shape (enabler × payoff), not `MillFeature`'s single-pillar density.
//!
//! Parser AST verification — VERIFIED (no parser remediation required; every
//! axis classifies from the existing typed AST, never by card name):
//!
//! | Axis | AST type | Location |
//! |---|---|---|
//! | Energy producer effect | `Effect::GainEnergy { amount: QuantityExpr }` | `ability.rs:8717` |
//! | Energy sink cost | `AbilityCost::PayEnergy { amount }` via `cost_categories() → CostCategory::PaysEnergy` | `ability.rs:5888` |
//! | Cost category (single authority) | `ability.cost_categories() -> Vec<CostCategory>` (never destructure `AbilityCost`) | `ability.rs` |
//! | Ability chain walk | `crate::ability_chain::collect_chain_effects(ability)` | `phase-ai` |
//! | Trigger-execute chain | `TriggerDefinition.execute: Option<Box<AbilityDefinition>>` | `ability.rs` |
//! | Player energy reserve | `state.players[player.0 as usize].energy: u32` | `player.rs` |
//!
//! Concrete verified parse shapes (from `card-data.json`):
//! - **Attune with Aether** / **Aetherworks Marvel**: spell/trigger `GainEnergy`
//!   → detected as a producer ✅
//! - **Rogue Refiner**: ETB trigger `execute = Draw`, `sub_ability = GainEnergy`
//!   — only caught because `collect_chain_effects` walks the sub-ability chain
//!   (a flat single-level walk misses it) ✅
//! - **Bristling Hydra** / **Longtusk Cub** / **Whirler Virtuoso**: ability
//!   `cost: PayEnergy` (sink) **and** a trigger `GainEnergy` (producer) →
//!   detected as both, the true energy-engine cards ✅
//! - **Aether Hub** (land): ETB `GainEnergy`, but lands are excluded from the
//!   density denominator and from producer/sink counts (see `detect`) ✅
//!
//! Each ability/trigger chain is checked in isolation (per-chain `any()`) so a
//! cross-ability false positive — one ability gains energy and a separate,
//! unrelated ability does something else — cannot combine into a spurious
//! producer hit.

use engine::game::DeckEntry;
use engine::types::ability::{AbilityCost, AbilityDefinition, CostCategory, Effect};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment floor below which `EnergyPayoffPolicy` opts out.
/// Calibration: a dedicated energy-engine deck (≥~12 producers and ≥~8 sinks
/// per 60 nonland) saturates to 1.0; a light splash (4 producers + 2 sinks /
/// 36 nonland) gives ≈ 0.19; incidental-energy aggro with no activated sinks
/// (e.g. Boros Energy: producers but `AbilityCost::PayEnergy` = 0) gives 0.0.
pub const COMMITMENT_FLOOR: f32 = 0.35;

/// Per-60-nonland producer density at which the producer side saturates. A
/// committed energy shell runs ~24–30 producer cards per 60 nonland (Attune × 4,
/// Rogue Refiner × 4, Servant × 4, Hydra/Cub/Virtuoso × 4 each, …); this divisor
/// places saturation just below that ceiling.
const PRODUCER_FULL_DENSITY: f32 = 30.0;

/// Per-60-nonland sink density at which the sink side saturates. Energy decks
/// run ~12–20 cards with an `AbilityCost::PayEnergy` activation (Hydra, Cub,
/// Virtuoso, Servant, Marvel, Coup, …); this divisor places saturation around a
/// dense engine.
const SINK_FULL_DENSITY: f32 = 20.0;

/// Per-deck energy-economy classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.{abilities,triggers}` — never by card name. The companion
/// `EnergyPayoffPolicy` consumes `commitment` to value energy-relevant casts
/// when the deck is energy-committed and the live reserve makes it matter.
#[derive(Debug, Clone, Default)]
pub struct EnergyFeature {
    /// Non-land cards whose ability or trigger-executed chain contains
    /// `Effect::GainEnergy` (CR 107.14: the casting player gains energy).
    pub producer_count: u32,
    /// Non-land cards with at least one `AbilityCost::PayEnergy` activation
    /// (CR 107.14: paying {E} removes one energy counter from the player).
    pub sink_count: u32,
    /// Non-land cards that are BOTH a producer and a sink — the true
    /// energy-engine bodies (Bristling Hydra, Longtusk Cub, …). Diagnostic;
    /// commitment is derived from producer × sink density, not this count.
    pub payoff_count: u32,
    /// `0.0..=1.0` — geometric mean of normalized producer and sink density.
    /// High only when the deck can both generate *and* spend energy. Consumed
    /// by `EnergyPayoffPolicy::activation` as the gate-and-scale knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// energy producers and sinks among non-land cards.
///
/// Lands (including energy-fixing lands like Aether Hub) are excluded from both
/// the counts and the density denominator: the energy archetype is defined by
/// its non-land engine, and counting lands would inflate density for "free"
/// producers. CR 107.14 / CR 122.1: energy is a player reserve built and spent
/// by spells and activated abilities.
pub fn detect(deck: &[DeckEntry]) -> EnergyFeature {
    if deck.is_empty() {
        return EnergyFeature::default();
    }

    let mut producer_count = 0u32;
    let mut sink_count = 0u32;
    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if face.card_type.core_types.contains(&CoreType::Land) {
            continue;
        }
        total_nonland = total_nonland.saturating_add(entry.count);

        let is_producer = is_energy_producer(face);
        let is_sink = is_energy_sink(face);
        if is_producer {
            producer_count = producer_count.saturating_add(entry.count);
        }
        if is_sink {
            sink_count = sink_count.saturating_add(entry.count);
        }
        if is_producer && is_sink {
            payoff_count = payoff_count.saturating_add(entry.count);
        }
    }

    let commitment = energy_commitment(producer_count, sink_count, total_nonland);
    EnergyFeature {
        producer_count,
        sink_count,
        payoff_count,
        commitment,
    }
}

/// Geometric mean of normalized producer and sink density.
///
/// Energy is a two-part economy: the axis only matters when the deck can both
/// build the reserve (producers) and drain it for value (sinks). The geometric
/// mean is therefore limited by the scarcer side — a deck of pure producers
/// (Attune for fixing, no sinks) or pure sinks scores 0, exactly matching the
/// `AristocratsFeature` enabler × payoff shape.
///
/// Calibration:
/// - Dedicated engine (36 prod / 12 sink / 36 nonland): density 60 / 20 → both
///   sides saturate → commitment 1.0.
/// - Splash (4 prod / 2 sink / 36 nonland): density 6.7 / 3.3 → normalized
///   0.22 × 0.17 → commitment ≈ 0.19, below `COMMITMENT_FLOOR`.
/// - Producers-only (Boros Energy, 0 activated sinks): commitment 0.0.
fn energy_commitment(producer_count: u32, sink_count: u32, total_nonland: u32) -> f32 {
    if producer_count == 0 || sink_count == 0 || total_nonland == 0 {
        return 0.0;
    }
    let producer_norm = (commitment::density_per_60(producer_count, total_nonland)
        / PRODUCER_FULL_DENSITY)
        .min(1.0);
    let sink_norm =
        (commitment::density_per_60(sink_count, total_nonland) / SINK_FULL_DENSITY).min(1.0);
    (producer_norm * sink_norm).sqrt().min(1.0)
}

/// True if this face contains at least one ability or trigger-executed chain
/// that grants energy to its controller. Each chain is checked in isolation to
/// prevent a cross-ability false positive.
///
/// CR 107.14: the {E} symbol represents one energy counter; `GainEnergy` adds
/// counters to the resolving player's reserve (CR 122.1).
pub fn is_energy_producer(face: &CardFace) -> bool {
    face.abilities.iter().any(chain_includes_energy_gain)
        || face.triggers.iter().any(|trigger| {
            trigger
                .execute
                .as_deref()
                .is_some_and(chain_includes_energy_gain)
        })
}

/// True if this face contains at least one activated, spell, or trigger-executed
/// ability whose cost pays energy, recursing through the ability tree
/// (`sub_ability`, `else_ability`, `mode_abilities`) so energy payments nested
/// in modal or chained abilities are caught.
///
/// CR 107.14: paying {E} removes one energy counter from the player. The single
/// authority for cost classification is `AbilityDefinition::cost_categories()`
/// (never destructure `AbilityCost`).
pub fn is_energy_sink(face: &CardFace) -> bool {
    face.abilities.iter().any(ability_tree_pays_energy)
        || face.triggers.iter().any(|trigger| {
            trigger
                .execute
                .as_deref()
                .is_some_and(ability_tree_pays_energy)
        })
}

/// True if a single ability chain (flattened by `collect_chain_effects`) grants
/// energy.
pub(crate) fn chain_includes_energy_gain(ability: &AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .copied()
        .any(effect_is_energy_gain)
}

/// True if this ability, or any ability nested under it
/// (`sub_ability` / `else_ability` / `mode_abilities`), pays energy. The cost
/// tree is walked explicitly because `cost_categories()` reports only the
/// immediate ability's cost. Shared by the deck-time `CardFace` detector
/// (`is_energy_sink`) and the live-game `EnergyPayoffPolicy`.
pub(crate) fn ability_tree_pays_energy(ability: &AbilityDefinition) -> bool {
    ability
        .cost_categories()
        .contains(&CostCategory::PaysEnergy)
        || collect_chain_effects(ability)
            .iter()
            .copied()
            .any(effect_pays_energy)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(ability_tree_pays_energy)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(ability_tree_pays_energy)
        || ability.mode_abilities.iter().any(ability_tree_pays_energy)
}

/// Single authority — true if this effect grants energy to its controller.
///
/// CR 107.14 / CR 122.1: `GainEnergy` adds energy counters to the resolving
/// player's reserve. Shared by the deck-time `CardFace` detector
/// (`is_energy_producer`) and the live-game `EnergyPayoffPolicy` so the two
/// never drift.
pub(crate) fn effect_is_energy_gain(effect: &Effect) -> bool {
    matches!(effect, Effect::GainEnergy { .. })
}

fn effect_pays_energy(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::PayCost {
            cost: AbilityCost::PayEnergy { .. },
            ..
        }
    )
}
