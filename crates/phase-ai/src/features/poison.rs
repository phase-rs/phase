//! Poison feature — structural detection of the alternate-win poison clock.
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `Keyword::Toxic(u32)` at `crates/engine/src/types/keywords.rs:912`
//!   (CR 702.164: "Toxic N — when this creature deals combat damage to a
//!   player, that player gets N poison counters").
//! - `Keyword::Infect` at `crates/engine/src/types/keywords.rs:612`
//!   (CR 702.90: damage to players is dealt as poison counters).
//! - `Keyword::Poisonous(u32)` at `crates/engine/src/types/keywords.rs:898`
//!   (CR 702.70).
//! - `Effect::GivePlayerCounter { counter_kind, count, target }` at
//!   `crates/engine/src/types/ability.rs:12230`; `PlayerCounterKind::Poison` at
//!   `crates/engine/src/types/player.rs:56` (CR 122.1f). Note this effect scopes
//!   by `TargetFilter`, NOT by `CountScope` — the sibling `QuantityRef` arm at
//!   `ability.rs:5443` is a counter *reader*, not a granter.
//! - `Effect::Proliferate` / `Effect::ProliferateTarget { target }` at
//!   `crates/engine/src/types/ability.rs:10450` and `:10457` (CR 701.34a).
//!
//! No parser remediation required — every axis is expressible over existing
//! typed AST.
//!
//! ## Why this axis exists
//!
//! CR 104.3d makes ten poison counters a loss condition entirely independent of
//! life total, and `Player.poison_counters` is a dedicated engine field
//! (`crates/engine/src/types/player.rs:117`). The AI's evaluation is
//! life-total-centric, so a deck whose whole clock is poison reads as doing
//! nothing. This axis lets a policy see the second clock.
//!
//! ## Boundary with `plus_one_counters`
//!
//! `plus_one_counters` already counts `Effect::Proliferate` as a +1/+1 counter
//! enabler. That overlap is intentional and the axes stay independent: one
//! proliferate card genuinely serves both decks, and a Hardened Scales deck
//! scores high there while scoring ~0 here (no poison sources).

use engine::game::DeckEntry;
use engine::types::ability::{AbilityDefinition, ControllerRef, Effect, TargetFilter};
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::player::PlayerCounterKind;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment at or above which the poison clock is a real plan for this deck
/// rather than an incidental splash. Gates `PoisonClockPolicy::activation`.
pub const POISON_CLOCK_FLOOR: f32 = 0.35;

/// CR 104.3d: a player with ten or more poison counters loses the game.
pub const LETHAL_POISON: u32 = 10;

/// CR 104.3d + CR 122.1f: per-deck poison-clock classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.keywords` and `CardFace.abilities` — never by card name.
#[derive(Debug, Clone, Default)]
pub struct PoisonFeature {
    /// Creatures that convert combat damage into poison counters — CR 702.164
    /// Toxic, CR 702.90 Infect, CR 702.70 Poisonous.
    pub source_count: u32,
    /// Cards whose effect chain gives poison counters to opponents outright,
    /// without needing combat damage (CR 122.1f).
    pub direct_count: u32,
    /// Cards that proliferate (CR 701.34a) — the accelerant on an established
    /// poison clock.
    pub proliferate_count: u32,
    /// `0.0..=1.0` — how central the poison clock is to this deck. Consumed by
    /// `PoisonClockPolicy::activation` as the single scaling knob.
    pub commitment: f32,
    /// Names of detected poison sources. NOT used for classification — that
    /// already happened against the AST. Used as battlefield identifiers at
    /// decision time (identity lookup, exempt from the name-matching lint).
    pub source_names: Vec<String>,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> PoisonFeature {
    if deck.is_empty() {
        return PoisonFeature::default();
    }

    let mut source_count = 0u32;
    let mut direct_count = 0u32;
    let mut proliferate_count = 0u32;
    let mut total_nonland = 0u32;
    let mut source_names: Vec<String> = Vec::new();

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        let is_source = is_poison_source_parts(&face.card_type.core_types, &face.keywords);
        let is_direct = gives_opponents_poison_parts(&face.abilities);

        if is_source {
            source_count = source_count.saturating_add(entry.count);
        }
        if is_direct {
            direct_count = direct_count.saturating_add(entry.count);
        }
        // One push per UNIQUE face, and once even when both axes fire —
        // guards the per-copy and double-push traps.
        if is_source || is_direct {
            source_names.push(face.name.clone());
        }
        if proliferates_parts(&face.abilities) {
            proliferate_count = proliferate_count.saturating_add(entry.count);
        }
    }

    let commitment =
        compute_commitment(source_count, direct_count, proliferate_count, total_nonland);

    PoisonFeature {
        source_count,
        direct_count,
        proliferate_count,
        commitment,
        source_names,
    }
}

/// Calibration: Modern Infect (12 infect creatures + 2 proliferate over 37
/// nonland) → commitment ≈ 0.90. Anti-calibration: a superfriends deck running
/// 2 proliferate cards and no poison source → ≈ 0.06, far below
/// `POISON_CLOCK_FLOOR`; UW control → 0.0.
///
/// Weighted sum rather than geometric mean: missing pillars are tolerable here.
/// A dedicated Infect deck runs zero direct-poison spells and often zero
/// proliferate, and must still read as fully committed.
fn compute_commitment(
    source_count: u32,
    direct_count: u32,
    proliferate_count: u32,
    total_nonland: u32,
) -> f32 {
    commitment::weighted_sum(&[
        // Full pillar at ~15 sources per 60 nonland — a dedicated poison deck.
        (
            0.65 / 15.0,
            commitment::density_per_60(source_count, total_nonland),
        ),
        // Direct poison is rarer per deck, so it saturates sooner.
        (
            0.20 / 6.0,
            commitment::density_per_60(direct_count, total_nonland),
        ),
        // Proliferate alone is never a poison plan — it only accelerates one.
        (
            0.15 / 8.0,
            commitment::density_per_60(proliferate_count, total_nonland),
        ),
    ])
}

/// CR 702.164 / CR 702.90 / CR 702.70: a creature whose combat damage becomes
/// poison counters. Non-creature faces never qualify — all three keywords are
/// combat-damage abilities.
pub(crate) fn is_poison_source_parts(core_types: &[CoreType], keywords: &[Keyword]) -> bool {
    if !core_types.contains(&CoreType::Creature) {
        return false;
    }
    keywords.iter().any(|k| {
        matches!(
            k,
            Keyword::Toxic(_) | Keyword::Infect | Keyword::Poisonous(_)
        )
    })
}

/// CR 122.1f: an ability chain that gives poison counters to a player who can
/// be an opponent.
///
/// A `TargetFilter::Controller` / `SelfRef` scope is rejected — a card that
/// poisons ITS OWN controller (a drawback clause, e.g. Phyrexian Vatmother) is
/// not a poison payoff.
pub(crate) fn gives_opponents_poison_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        collect_chain_effects(ability).iter().any(|effect| {
            matches!(
                effect,
                Effect::GivePlayerCounter {
                    counter_kind: PlayerCounterKind::Poison,
                    target,
                    ..
                } if filter_can_hit_opponent(target)
            )
        })
    })
}

/// True when a `TargetFilter` can resolve to a player other than the ability's
/// controller. Mirrors `landfall::filter_matches_land_you_control`'s recursion,
/// including the CR 109.3 conjunction rule for `And`.
fn filter_can_hit_opponent(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Opponent | TargetFilter::Player | TargetFilter::Any => true,
        TargetFilter::Typed(typed) => !matches!(typed.controller, Some(ControllerRef::You)),
        TargetFilter::Or { filters } => filters.iter().any(filter_can_hit_opponent),
        // CR 109.3: every constraint of an `And` must hold for the match.
        TargetFilter::And { filters } => filters.iter().all(filter_can_hit_opponent),
        _ => false,
    }
}

/// CR 701.34a: an ability chain containing either proliferate form.
pub(crate) fn proliferates_parts(abilities: &[AbilityDefinition]) -> bool {
    abilities.iter().any(|ability| {
        collect_chain_effects(ability).iter().any(|effect| {
            matches!(
                effect,
                Effect::Proliferate | Effect::ProliferateTarget { .. }
            )
        })
    })
}
