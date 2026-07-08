//! Blink / flicker (ETB-value reuse) feature — structural detection over a
//! deck's typed AST.
//!
//! Parser AST verification — VERIFIED (no parser remediation required; every
//! axis classifies from the existing typed AST, never by card name):
//! - Flicker enabler: an ability/trigger effect chain that BOTH exiles a
//!   friendly permanent AND returns it to the battlefield in the same chain.
//!   Real flicker cards parse as a two-step `ChangeZone` chain — Ephemerate
//!   (`card-data.json`) is
//!   `ChangeZone { destination: Exile, target: Typed(Creature, controller: You) }`
//!   followed by a `sub_ability`
//!   `ChangeZone { destination: Battlefield, target: TrackedSet { .. } }`.
//!   `crate::ability_chain::collect_chain_effects` flattens the `sub_ability`
//!   chain, so both steps land in one slice. CR 603.7 (the tracked set is the
//!   "it"/"that card" anaphor referring back to the exiled object) + CR 110.1
//!   (the returned card becomes a new permanent as it re-enters). The
//!   battlefield-return step targets a `TrackedSet`/`TrackedSetFiltered`, which
//!   is what distinguishes a flicker-return from a graveyard reanimation
//!   (`origin: Graveyard, target: Typed(Creature)`, the reanimator axis) and
//!   from a one-way removal exile (no return step at all).
//! - ETB-value payoff: a creature whose `TriggerMode::ChangesZone` trigger fires
//!   on itself or a friendly creature entering the battlefield (CR 603.6a) and
//!   whose executed chain produces card-advantage / board / removal value worth
//!   re-triggering — Mulldrifter parses as `mode: ChangesZone`,
//!   `valid_card: SelfRef`, `destination: Battlefield`, executing `Draw`.
//!
//! Why this is not redundant with existing handling: `policies/etb_value.rs`
//! scores an ETB trigger's value at *cast time only* (one-shot), with no notion
//! of re-triggering it; `aristocrats` keys on sacrifice/death and `tokens_wide`
//! on going wide — none recognizes a deck whose plan is "flicker a creature to
//! re-use its ETB." This axis fills that gap; the companion `BlinkPayoffPolicy`
//! is payoff-gated so non-blink decks (and the general `etb_value` scoring) are
//! unaffected.

use engine::game::DeckEntry;
use engine::types::ability::{ControllerRef, Effect, TargetFilter, TriggerDefinition, TypeFilter};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment floor below which `BlinkPayoffPolicy` opts out. Matches the
/// reanimator / equipment / enchantments payoff-axis convention.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// Per-60-nonland flicker-enabler density at which the flicker pillar saturates.
/// Dedicated blink shells run ~6–10 flicker enablers (Ephemerate, Cloudshift,
/// Ghostly Flicker, Soulherder, Restoration Angel) per 60 nonland; the divisor
/// is set so a single incidental flicker spell stays below the floor.
const FLICKER_FULL_DENSITY: f32 = 7.0;

/// Per-60-nonland ETB-payoff density at which the payoff pillar saturates. A
/// committed blink deck runs ~12–18 value-ETB creatures (Mulldrifter, Elvish
/// Visionary, Wall of Omens, Solemn Simulacrum, Flametongue Kavu, Ravenous
/// Chupacabra) per 60 nonland.
const ETB_PAYOFF_FULL_DENSITY: f32 = 12.0;

/// Per-deck blink / flicker classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.{abilities,triggers,card_type}` — never by card name. The companion
/// `BlinkPayoffPolicy` consumes this to value deploying flicker enablers and
/// casting value-ETB creatures when the deck is blink-committed.
#[derive(Debug, Clone, Default)]
pub struct BlinkFeature {
    /// Cards whose effect chain exiles a friendly permanent and immediately
    /// returns it to the battlefield (CR 603.7 + CR 110.1). The flicker engine.
    pub flicker_count: u32,
    /// Value-ETB creatures — a `ChangesZone`→battlefield self/friendly trigger
    /// producing card-advantage / board / removal value (CR 603.6a). The payoff
    /// being re-triggered. Without flicker there is nothing to re-trigger, and
    /// without payoffs flicker has nothing worth re-using, so both are required.
    pub etb_payoff_count: u32,
    /// `0.0..=1.0` — how central the blink plan is to this deck. Requires both
    /// flicker density and ETB-payoff density; missing either collapses to
    /// inert. Consumed by `BlinkPayoffPolicy::activation` as the scaling knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// flicker enablers and value-ETB payoffs.
pub fn detect(deck: &[DeckEntry]) -> BlinkFeature {
    if deck.is_empty() {
        return BlinkFeature::default();
    }

    let mut flicker_count = 0u32;
    let mut etb_payoff_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }
        if is_flicker_enabler(face) {
            flicker_count = flicker_count.saturating_add(entry.count);
        }
        if is_etb_payoff(face) {
            etb_payoff_count = etb_payoff_count.saturating_add(entry.count);
        }
    }

    let commitment = blink_commitment(flicker_count, etb_payoff_count, total_nonland);

    BlinkFeature {
        flicker_count,
        etb_payoff_count,
        commitment,
    }
}

/// Geometric-mean commitment over the two required pillars (flicker density and
/// ETB-payoff density).
///
/// A blink deck needs BOTH a way to flicker AND ETBs worth re-triggering;
/// missing either pillar means it is not the blink plan, so commitment collapses
/// to `0.0` (which keeps the policy opted out for decks that merely run one
/// incidental flicker spell or a couple of value creatures).
///
/// Calibration — WX Blink (≈38 nonland: 8 flicker enablers, 14 value-ETB
/// creatures): flicker density ≈12.6 and ETB density ≈22.1 both saturate their
/// pillars → geometric mean 1.0, well above the floor.
///
/// Anti-calibration — one incidental flicker spell + two value-ETB creatures
/// (≈36 nonland) gives flicker density ≈1.67 and ETB density ≈3.33 → geometric
/// mean ≈0.26, below the floor, so the policy stays inert.
fn blink_commitment(flicker_count: u32, etb_payoff_count: u32, total_nonland: u32) -> f32 {
    if flicker_count == 0 || etb_payoff_count == 0 || total_nonland == 0 {
        return 0.0;
    }

    let flicker =
        (commitment::density_per_60(flicker_count, total_nonland) / FLICKER_FULL_DENSITY).min(1.0);
    let payoff = (commitment::density_per_60(etb_payoff_count, total_nonland)
        / ETB_PAYOFF_FULL_DENSITY)
        .min(1.0);

    commitment::geometric_mean(&[flicker, payoff]).min(1.0)
}

/// True if this face is a flicker enabler — at least one of its ability or
/// trigger effect chains BOTH exiles a friendly permanent AND returns it to
/// the battlefield within that same chain. CR 603.7.
///
/// Each chain is checked in isolation so that two independent, unrelated
/// abilities (e.g. one that exiles and one that puts something onto the
/// battlefield for a different reason) cannot combine to produce a false
/// positive.
pub fn is_flicker_enabler(face: &CardFace) -> bool {
    face.abilities
        .iter()
        .any(|ability| effects_include_flicker(&collect_chain_effects(ability)))
        || face.triggers.iter().any(|trigger| {
            trigger
                .execute
                .as_deref()
                .is_some_and(|execute| effects_include_flicker(&collect_chain_effects(execute)))
        })
}

/// True if this face is a value-ETB payoff: a creature with a self/friendly
/// `ChangesZone`→battlefield trigger producing card-advantage / board / removal
/// value worth re-triggering. CR 603.6a.
pub fn is_etb_payoff(face: &CardFace) -> bool {
    face_is_creature(face) && face.triggers.iter().any(trigger_is_value_etb)
}

/// Single authority — true if the effect slice both exiles a friendly permanent
/// AND returns the exiled object to the battlefield (the flicker signature).
/// Shared by deck-time `CardFace` detection ([`is_flicker_enabler`]) and the
/// live-game `BlinkPayoffPolicy` so the two never drift.
pub(crate) fn effects_include_flicker(effects: &[&Effect]) -> bool {
    effects.iter().copied().any(effect_is_friendly_exile)
        && effects.iter().copied().any(effect_is_flicker_return)
}

/// Single authority — true if this trigger is a self/friendly value ETB. Shared
/// by the detector and the live policy (which classifies a live `GameObject`'s
/// `trigger_definitions` without reconstructing a `CardFace`).
pub(crate) fn trigger_is_value_etb(trigger: &TriggerDefinition) -> bool {
    if trigger.mode != TriggerMode::ChangesZone {
        return false;
    }
    // CR 603.6a: "enters the battlefield". A trigger whose origin is the
    // battlefield is a "leaves" trigger masquerading as ChangesZone — exclude it.
    if trigger.destination != Some(Zone::Battlefield)
        || matches!(trigger.origin, Some(Zone::Battlefield))
    {
        return false;
    }
    let Some(valid_card) = trigger.valid_card.as_ref() else {
        return false;
    };
    if !etb_filter_is_self_or_friendly_creature(valid_card) {
        return false;
    }
    trigger.execute.as_deref().is_some_and(|execute| {
        collect_chain_effects(execute)
            .iter()
            .copied()
            .any(effect_is_etb_value)
    })
}

/// CR 603.7 + CR 110.1: the exile half of a flicker — a `ChangeZone` to exile of
/// a friendly (or unscoped) permanent. An opponent-scoped exile is removal/tempo
/// disruption, not a value flicker, so it is rejected.
fn effect_is_friendly_exile(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            destination: Zone::Exile,
            target,
            ..
        } if target_is_not_opponent_scoped(target)
    )
}

/// CR 603.7: the return half of a flicker — a `ChangeZone` to the battlefield
/// referring back to the just-exiled object via one of the two anaphors real
/// flicker cards use: a `TrackedSet`/`TrackedSetFiltered` (Ephemerate, Ghostly
/// Flicker, Eldrazi Displacer) or the parent ability's target `ParentTarget`
/// ("exile target creature you control, then return *that card*" — Cloudshift,
/// Soulherder, Restoration Angel, Felidar Guardian, Conjurer's Closet).
/// Referring back to the exiled object (rather than a `Typed` graveyard creature)
/// is what separates a flicker-return from a reanimation; pairing it with the
/// exile step (the `&&` in `effects_include_flicker`) is what separates it from a
/// one-way "put a creature onto the battlefield" effect that reuses
/// `ParentTarget`.
fn effect_is_flicker_return(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            destination: Zone::Battlefield,
            target: TargetFilter::TrackedSet { .. }
                | TargetFilter::TrackedSetFiltered { .. }
                | TargetFilter::ParentTarget,
            ..
        }
    )
}

/// CR 603.6a: the trigger fires on the source itself entering (`SelfRef`, the
/// common "When ~ enters" self-ETB) or on a friendly creature entering (an
/// "whenever a creature you control enters" engine). An opponent-scoped or
/// non-creature filter is not a creature-ETB-value payoff.
///
/// Separated into two orthogonal checks so that compound filters like
/// "friendly white creature" (`And { [Typed(Creature), Typed(White)] }`) are
/// accepted: the creature check uses `.any()` over `And` conjuncts (at least
/// one conjunct must name a creature type), while the opponent-scope check
/// delegates to `target_is_not_opponent_scoped` which uses `.all()` (no
/// conjunct may be opponent-scoped).
fn etb_filter_is_self_or_friendly_creature(filter: &TargetFilter) -> bool {
    target_is_not_opponent_scoped(filter) && filter_contains_creature(filter)
}

fn filter_contains_creature(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::SelfRef => true,
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(type_filter_is_creature),
        TargetFilter::Or { filters } => filters.iter().any(filter_contains_creature),
        TargetFilter::And { filters } => filters.iter().any(filter_contains_creature),
        _ => false,
    }
}

fn type_filter_is_creature(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Creature => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_creature),
        _ => false,
    }
}

/// CR 608.2b: unwrap a flicker-exile target filter and report whether it is NOT
/// opponent-scoped (i.e., a friendly or unscoped permanent the deck would blink
/// for value). `And` rejects if any conjunct is opponent-scoped.
fn target_is_not_opponent_scoped(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => !matches!(typed.controller, Some(ControllerRef::Opponent)),
        TargetFilter::Or { filters } => filters.iter().any(target_is_not_opponent_scoped),
        TargetFilter::And { filters } => filters.iter().all(target_is_not_opponent_scoped),
        // SelfRef / Any / unscoped references are friendly-usable.
        TargetFilter::SelfRef | TargetFilter::Any => true,
        _ => false,
    }
}

/// The curated set of ETB effects worth re-triggering via flicker — the value a
/// blink deck is built to re-use. Each covers a class of canonical blink
/// targets:
/// - `Draw` — card advantage (Mulldrifter, Elvish Visionary)
/// - `Token` — board presence (Cloudgoat Ranger)
/// - `DealDamage` — removal / reach (Flametongue Kavu)
/// - `Destroy` — removal (Ravenous Chupacabra, Shriekmaw)
/// - `SearchLibrary` — tutor / ramp (Solemn Simulacrum, Ranger of Eos)
/// - `Bounce` — tempo (Man-o'-War)
fn effect_is_etb_value(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Draw { .. }
            | Effect::Token { .. }
            | Effect::DealDamage { .. }
            | Effect::Destroy { .. }
            | Effect::SearchLibrary { .. }
            | Effect::Bounce { .. }
    )
}

/// CR 302.1: a creature card — the body a blink deck flickers to re-use its ETB.
fn face_is_creature(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Creature)
}
