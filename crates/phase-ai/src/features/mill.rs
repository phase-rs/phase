//! Mill (opponent-library-depletion) feature — structural detection over a
//! deck's typed AST.
//!
//! Parser AST verification — VERIFIED (no parser remediation required; every
//! axis classifies from the existing typed AST, never by card name):
//!
//! | Axis | AST type | Location |
//! |---|---|---|
//! | Opponent-mill effect | `Effect::Mill { count: QuantityExpr, target: TargetFilter, destination: Zone }` | `ability.rs:7111` |
//! | Opponent-mill target | `target` is NOT `TargetFilter::Controller` or `TargetFilter::Any` | complement of `reanimator.rs:258` |
//! | Self-mill (excluded) | `target: Controller \| Any`, handled by reanimator axis | `reanimator.rs:258` |
//! | Ability chain walk | `crate::ability_chain::collect_chain_effects(ability)` | `phase-ai` |
//! | Opponent library size | `state.players[opponent.0 as usize].library.len()` | `player.rs:96` |
//!
//! Concrete verified parse shapes (from `card-data.json`):
//! - **Glimpse the Unthinkable** / **Tome Scour** / **Traumatize**:
//!   `Mill { target: Player, destination: Graveyard }` — `Player` is NOT
//!   `Controller | Any`, so it is detected as opponent-mill potential ✅
//! - **Archive Trap** / **Mind Sculpt**:
//!   `Mill { target: Typed { controller: Opponent }, destination: Graveyard }` —
//!   explicitly opponent-scoped ✅
//! - **Hedron Crab**: trigger-executed `Mill { target: Player }` — caught by the
//!   per-chain trigger walk ✅
//! - **Fraying Sanity** / **Fractured Sanity**: `Mill { target: Controller }` —
//!   correctly excluded as self-mill (handled by the reanimator axis) ✅
//!
//! Mill is a **single-pillar** axis: a deck either has enough opponent-mill
//! density to close out a game or it doesn't. Unlike blink (where flicker
//! enabler and ETB payoff are structurally distinct AST shapes), there is no
//! separate "payoff" class — commitment is a density-normalized single-pillar
//! curve, matching the `AggroPressureFeature` shape.
//!
//! Each ability and trigger chain is checked in isolation (per-chain `any()`)
//! to prevent a cross-ability false positive where one ability mills and a
//! separate, unrelated ability does something else that could be
//! misidentified.

use engine::game::DeckEntry;
use engine::types::ability::{Effect, TargetFilter};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment floor below which `MillPayoffPolicy` opts out.
/// Calibration: a splash of 4 mill spells in a 36-nonland deck gives
/// density ≈ 6.7 and commitment ≈ 0.33, which stays below the floor.
/// A dedicated mill deck with 20+ enablers in 36 nonland hits 1.0.
pub const COMMITMENT_FLOOR: f32 = 0.35;

/// Per-60-nonland opponent-mill density at which the axis saturates. A
/// committed Pioneer/Modern mill shell runs 20–28 opponent-mill effects
/// per 60 nonland (Glimpse × 4, Archive Trap × 4, Tome Scour × 4, etc.);
/// this divisor places saturation just below that ceiling.
const MILL_FULL_DENSITY: f32 = 20.0;

/// Per-deck mill (opponent-library-depletion) classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.{abilities,triggers}` — never by card name. The companion
/// `MillPayoffPolicy` consumes this to raise the priority of mill spells when
/// the deck is mill-committed and opponent library size makes it matter.
#[derive(Debug, Clone, Default)]
pub struct MillFeature {
    /// Cards whose ability or trigger-executed chain contains
    /// `Effect::Mill { destination: Graveyard, target != Controller | Any }`.
    /// CR 701.17a: milling puts cards from the top of a library into the graveyard.
    pub mill_count: u32,
    /// `0.0..=1.0` — how central the mill win-condition is to this deck.
    /// A single incidental mill spell gives ≈ 0.08; a dedicated mill shell
    /// reaches 1.0. Consumed by `MillPayoffPolicy::activation` as the
    /// gate-and-scale knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// opponent-mill enablers.
pub fn detect(deck: &[DeckEntry]) -> MillFeature {
    if deck.is_empty() {
        return MillFeature::default();
    }

    let mut mill_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }
        if is_mill_enabler(face) {
            mill_count = mill_count.saturating_add(entry.count);
        }
    }

    let commitment = mill_commitment(mill_count, total_nonland);
    MillFeature {
        mill_count,
        commitment,
    }
}

/// Density-normalized single-pillar commitment.
///
/// Calibration:
/// - Dedicated mill (20+ enablers / 36 nonland): density ≈ 33–50 per 60 →
///   commitment 1.0.
/// - Splash mill (4 enablers / 36 nonland): density ≈ 6.7 per 60 →
///   commitment ≈ 0.33, below `COMMITMENT_FLOOR`.
/// - 1 incidental (1 / 36 nonland): density ≈ 1.7 → commitment ≈ 0.08,
///   well below floor.
fn mill_commitment(mill_count: u32, total_nonland: u32) -> f32 {
    if mill_count == 0 || total_nonland == 0 {
        return 0.0;
    }
    (commitment::density_per_60(mill_count, total_nonland) / MILL_FULL_DENSITY).min(1.0)
}

/// True if this face contains at least one ability or trigger-executed chain
/// that mills an opponent. Each chain is checked in isolation to prevent a
/// cross-ability false positive.
///
/// CR 701.17a: to mill N means to put the top N cards of a library into the
/// graveyard. The opponent-scope check (target NOT `Controller | Any`) ensures
/// self-mill is excluded and handled by the reanimator axis instead.
#[allow(clippy::redundant_closure)]
pub fn is_mill_enabler(face: &CardFace) -> bool {
    face.abilities
        .iter()
        .any(|ability| chain_includes_opponent_mill(ability))
        || face.triggers.iter().any(|trigger| {
            trigger
                .execute
                .as_deref()
                .is_some_and(chain_includes_opponent_mill)
        })
}

/// True if a single ability chain (flattened by `collect_chain_effects`)
/// contains an opponent-mill step.
fn chain_includes_opponent_mill(ability: &engine::types::ability::AbilityDefinition) -> bool {
    collect_chain_effects(ability)
        .iter()
        .copied()
        .any(effect_is_opponent_mill)
}

/// Single authority — true if this effect mills an opponent (destination is
/// the graveyard AND target is not the casting player's own library).
///
/// CR 701.17a: mill sends cards to the graveyard. `Controller` and `Any`
/// targets are self-mill (reanimator enablers, CR 701.9a); `Player`,
/// `Opponent`, and opponent-`Typed` filters are opponent-facing.
///
/// Shared by the deck-time `CardFace` detector (`is_mill_enabler`) and the
/// live-game `MillPayoffPolicy` so the two never drift.
pub(crate) fn effect_is_opponent_mill(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Mill {
            destination: Zone::Graveyard,
            target,
            ..
        } if !matches!(target, TargetFilter::Controller | TargetFilter::Any)
    )
}
