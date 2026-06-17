//! Enchantments-matter feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED:
//! - `CoreType::Enchantment` on `CardFace.card_type.core_types` (CR 301.1) —
//!   enchantment density, the fuel an enchantress/constellation deck runs on.
//! - **Constellation** — `TriggerMode::ChangesZone` with `destination =
//!   Battlefield` and a `valid_card` filter matching an Enchantment you control
//!   (CR 603.6a). Same structural shape `landfall.rs` uses for land-ETB.
//! - **Enchantress** — `TriggerMode::SpellCast` / `SpellCastOrCopy` with a
//!   `valid_card` filter referencing the Enchantment card type (CR 601.2i).
//!   NOTE: the runtime `TriggerMode::SpellCast` is a *unit* variant; the spell
//!   type lives on the trigger's `valid_card` filter, NOT on the mode (the
//!   `SpellCast(Option<CoreType>)` payload belongs to the trigger-*event* enum,
//!   not `TriggerMode`).
//!
//! No parser remediation required — enchantments-matter cards classify
//! structurally using the existing typed AST; never by card name.
//!
//! Not redundant with existing handling: `synergy::detect_spellcast` is gated on
//! instant/sorcery density (`spell_count < 8`), so it never fires for an
//! enchantment deck; no other detector, policy, or eval dimension recognizes
//! enchantment payoffs.

use engine::game::DeckEntry;
use engine::types::ability::{
    ControllerRef, TargetFilter, TriggerDefinition, TypeFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::features::commitment;

/// Commitment floor below which `EnchantmentsPayoffPolicy` opts out.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// CR 301 / CR 603.6a: Per-deck enchantments-matter classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.card_type` and `CardFace.triggers` — never by card name. The
/// companion `EnchantmentsPayoffPolicy` consumes this to value casting
/// enchantments when the deck contains enchantress/constellation payoffs.
#[derive(Debug, Clone, Default)]
pub struct EnchantmentsFeature {
    /// Cards that are enchantments — the density that enchantress/constellation
    /// payoffs feed on. CR 301.1.
    pub enchantment_count: u32,
    /// Cards that REWARD enchantments: enchantress ("whenever you cast an
    /// enchantment spell, …") or constellation ("whenever an enchantment you
    /// control enters, …"). CR 601.2i / CR 603.6a. The intent signal.
    pub payoff_count: u32,
    /// `0.0..=1.0` — how central the enchantments-matter plan is to this deck.
    /// Driven primarily by payoff density; enchantment count is supporting fuel.
    /// Consumed by `EnchantmentsPayoffPolicy::activation` as the scaling knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// enchantments and enchantment payoffs.
pub fn detect(deck: &[DeckEntry]) -> EnchantmentsFeature {
    if deck.is_empty() {
        return EnchantmentsFeature::default();
    }

    let mut enchantment_count = 0u32;
    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }
        if face.card_type.core_types.contains(&CoreType::Enchantment) {
            enchantment_count = enchantment_count.saturating_add(entry.count);
        }
        if is_enchantment_payoff(face) {
            payoff_count = payoff_count.saturating_add(entry.count);
        }
    }

    // Payoffs are the intent signal; enchantment density is supporting fuel.
    // Weights mirror the calibrated artifacts/lifegain axes: a single incidental
    // payoff in a ~40–60-nonland deck stays below `COMMITMENT_FLOOR`
    // (≈0.12–0.18); it takes roughly three payoff-equivalents to activate. The
    // policy is additionally payoff-gated so enchantment density alone never
    // activates it.
    let commitment = commitment::weighted_sum(&[
        (
            0.12,
            commitment::density_per_60(payoff_count, total_nonland),
        ),
        (
            0.03,
            commitment::density_per_60(enchantment_count, total_nonland),
        ),
    ]);

    EnchantmentsFeature {
        enchantment_count,
        payoff_count,
        commitment,
    }
}

/// A payoff rewards controlling/casting enchantments: an enchantress or
/// constellation trigger. CR 601.2i / CR 603.6a.
fn is_enchantment_payoff(face: &CardFace) -> bool {
    face.triggers.iter().any(is_enchantment_payoff_trigger)
}

/// Single authority for the "this trigger is an enchantments-matter payoff"
/// classification. Enchantress (cast-an-enchantment) or constellation
/// (enchantment-you-control enters).
pub(crate) fn is_enchantment_payoff_trigger(trigger: &TriggerDefinition) -> bool {
    is_enchantress_trigger(trigger) || is_constellation_trigger(trigger)
}

/// Enchantress: a `SpellCast`/`SpellCastOrCopy` trigger whose `valid_card`
/// filter narrows to the Enchantment card type. CR 601.2i.
fn is_enchantress_trigger(trigger: &TriggerDefinition) -> bool {
    if !matches!(
        trigger.mode,
        TriggerMode::SpellCast | TriggerMode::SpellCastOrCopy
    ) {
        return false;
    }
    trigger
        .valid_card
        .as_ref()
        .is_some_and(target_filter_references_enchantment)
}

/// Constellation: a `ChangesZone` trigger firing when an Enchantment you
/// control enters the battlefield. CR 603.6a. Mirrors `landfall.rs`'s land-ETB
/// shape (destination = battlefield, origin not battlefield, controller = You).
fn is_constellation_trigger(trigger: &TriggerDefinition) -> bool {
    if trigger.mode != TriggerMode::ChangesZone {
        return false;
    }
    if trigger.destination != Some(Zone::Battlefield) {
        return false;
    }
    // A `Battlefield` origin would be a "leaves" trigger; unset origin == "from
    // anywhere" and is fine.
    if matches!(trigger.origin, Some(Zone::Battlefield)) {
        return false;
    }
    trigger
        .valid_card
        .as_ref()
        .is_some_and(filter_matches_enchantment_you_control)
}

/// True if a `TargetFilter` matches an Enchantment whose controller is `You`.
/// Opponent-scoped enchantment-ETB triggers (punishers) never count.
fn filter_matches_enchantment_you_control(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_is_enchantment_you_control(typed),
        TargetFilter::Or { filters } => filters.iter().any(filter_matches_enchantment_you_control),
        // CR 109.3: an `And` matches the *intersection* of its sub-filters, so if
        // ANY conjunct already restricts the match to "enchantment you control",
        // the whole conjunction is guaranteed to be enchantments you control (an
        // extra "nontoken" / "creature" conjunct only narrows it further). Using
        // `all` here would wrongly reject compound constellation triggers like
        // "whenever a nontoken enchantment you control enters".
        TargetFilter::And { filters } => filters.iter().any(filter_matches_enchantment_you_control),
        _ => false,
    }
}

fn typed_filter_is_enchantment_you_control(typed: &TypedFilter) -> bool {
    if !matches!(typed.controller, Some(ControllerRef::You)) {
        return false;
    }
    typed.type_filters.iter().any(type_filter_is_enchantment)
}

/// True if a `TargetFilter` references the Enchantment card type (controller-
/// agnostic — used for the enchantress cast filter, where the trigger source's
/// controller scope is implicit).
fn target_filter_references_enchantment(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(type_filter_is_enchantment),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_references_enchantment)
        }
        _ => false,
    }
}

fn type_filter_is_enchantment(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Enchantment => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_enchantment),
        _ => false,
    }
}
