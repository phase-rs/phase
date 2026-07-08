//! Equipment / Voltron feature — structural detection over a deck's typed AST.
//!
//! Parser AST verification — VERIFIED (no parser remediation required; every
//! axis classifies from the existing typed AST, never by card name):
//! - Equipment density: a face whose `card_type.subtypes` contains `"Equipment"`
//!   (CR 301.5 — the artifact subtype that attaches to a creature). Detected via
//!   `TypeFilter::Subtype("Equipment")`; there is no `TypeFilter::Equipment`.
//! - Equipment-support payoffs (the "equipment-matters" intent signal), all on
//!   NON-Equipment cards so the two pillars stay distinct:
//!   - tutor: `Effect::SearchLibrary { filter }` referencing Equipment
//!     (CR 701.23 — Stoneforge Mystic, Steelshaper's Gift);
//!   - free/triggered attach: `Effect::Attach { attachment }` referencing
//!     Equipment (CR 301.5 — Kor Outfitter, Brass Squire), distinct from an
//!     Equipment's own equip ability (`attachment` is `SelfRef`, and the
//!     `!Equipment` guard excludes it regardless);
//!   - equipment-cast / ETB trigger: a `TriggerDefinition.valid_card` referencing
//!     Equipment (CR 601.2 — Puresteel Paladin's draw on equipment ETB);
//!   - equip-cost grant: a `StaticDefinition` whose `modifications` add the
//!     `Equip` keyword to your Equipment (CR 702.6 — Puresteel's `equip {0}`).
//!
//! Why this is not redundant with existing handling: `features/artifacts.rs`
//! values artifact *count* / affinity-improvise payoffs with zero equip or
//! voltron awareness, and `policies/equipment_priority.rs` is an *anti-over-
//! equipping guard* that only rejects wasteful re-equips and never rewards
//! equipping or recognizes the deck-level voltron plan. This axis fills that gap;
//! the companion `EquipmentPayoffPolicy` is payoff-gated and acts only on
//! `CastSpell`, deferring the equip-activation decision to `EquipmentPriority`.

use engine::types::ability::{
    ContinuousModification, Effect, StaticDefinition, TargetFilter, TriggerDefinition, TypeFilter,
    TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;

use engine::game::DeckEntry;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment floor below which `EquipmentPayoffPolicy` opts out. Matches the
/// reanimator / lifegain / enchantments payoff-axis convention.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// Per-60-nonland Equipment density at which the fuel pillar saturates. Voltron /
/// Hammer shells run ~12–16 Equipment per 60 nonland; the divisor is set so that
/// two incidental swords (2 Equipment / 36 nonland) stay below the floor.
const EQUIPMENT_FULL_DENSITY: f32 = 12.0;

/// Per-60-nonland payoff density at which the intent pillar saturates. A
/// committed equipment deck runs ~6–10 support cards (tutors, equip-cost
/// reducers, auto-attachers, equipment-cast payoffs).
const PAYOFF_FULL_DENSITY: f32 = 7.0;

/// CR 301.5: the artifact subtype that marks an Equipment. Compared
/// case-insensitively against `TypeFilter::Subtype` and `CardType.subtypes`.
const EQUIPMENT_SUBTYPE: &str = "Equipment";

/// Per-deck equipment / voltron classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.{card_type,abilities,triggers,static_abilities}` — never by card
/// name. The companion `EquipmentPayoffPolicy` consumes this to value deploying
/// Equipment and casting equipment payoffs when the deck is equipment-committed.
#[derive(Debug, Clone, Default)]
pub struct EquipmentFeature {
    /// Equipment-subtype cards. CR 301.5. The fuel / voltron package.
    pub equipment_count: u32,
    /// Equipment-matters support cards — equipment tutors, free/triggered
    /// attachers, equipment-cast/ETB payoffs, and equip-cost grants. The intent
    /// signal that distinguishes a voltron deck from incidental Equipment.
    pub payoff_count: u32,
    /// `0.0..=1.0` — how central the equipment plan is to this deck. Requires
    /// both Equipment density and a payoff; missing either collapses to inert.
    /// Consumed by `EquipmentPayoffPolicy::activation` as the scaling knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// Equipment and equipment-support payoffs.
pub fn detect(deck: &[DeckEntry]) -> EquipmentFeature {
    if deck.is_empty() {
        return EquipmentFeature::default();
    }

    let mut equipment_count = 0u32;
    let mut payoff_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }
        if is_equipment(face) {
            equipment_count = equipment_count.saturating_add(entry.count);
        }
        if is_equipment_payoff(face) {
            payoff_count = payoff_count.saturating_add(entry.count);
        }
    }

    let commitment = equipment_commitment(equipment_count, payoff_count, total_nonland);

    EquipmentFeature {
        equipment_count,
        payoff_count,
        commitment,
    }
}

/// Geometric-mean commitment over the two required pillars (Equipment density
/// and equipment-support payoffs).
///
/// A voltron deck needs BOTH a dense Equipment package AND support that makes
/// equipment matter; missing either pillar means it is not an equipment-matters
/// deck, so commitment collapses to `0.0` (which keeps the policy opted out for
/// decks that merely run a couple of swords).
///
/// Calibration — Mono-White Equipment (≈38 nonland: 14 Equipment, 13 support):
/// both pillar densities saturate → geometric mean 1.0, well above the floor.
/// Anti-calibration — two incidental swords with no support collapse to 0.0.
fn equipment_commitment(equipment_count: u32, payoff_count: u32, total_nonland: u32) -> f32 {
    if equipment_count == 0 || payoff_count == 0 {
        return 0.0;
    }

    let equipment = (commitment::density_per_60(equipment_count, total_nonland)
        / EQUIPMENT_FULL_DENSITY)
        .min(1.0);
    let payoff =
        (commitment::density_per_60(payoff_count, total_nonland) / PAYOFF_FULL_DENSITY).min(1.0);

    commitment::geometric_mean(&[equipment, payoff]).min(1.0)
}

/// True if this face is an Equipment. CR 301.5.
pub fn is_equipment(face: &CardFace) -> bool {
    subtypes_contain_equipment(&face.card_type.subtypes)
}

/// True if this face is an equipment-matters support card: a NON-Equipment card
/// that tutors / auto-attaches Equipment, triggers off your Equipment, or grants
/// cheaper equip. These are the intent signal that the deck is built around
/// equipment.
pub fn is_equipment_payoff(face: &CardFace) -> bool {
    let effects = collect_face_effects(face);
    let triggers: Vec<&TriggerDefinition> = face.triggers.iter().collect();
    let statics: Vec<&StaticDefinition> = face.static_abilities.iter().collect();
    parts_are_equipment_payoff(&face.card_type.subtypes, &effects, &triggers, &statics)
}

/// Single authority — true if `subtypes` contains the Equipment subtype.
/// Shared by deck-time `CardFace` detection and the live-game
/// `EquipmentPayoffPolicy` (which reads `GameObject.card_types.subtypes`).
pub(crate) fn subtypes_contain_equipment(subtypes: &[String]) -> bool {
    subtypes
        .iter()
        .any(|sub| sub.eq_ignore_ascii_case(EQUIPMENT_SUBTYPE))
}

/// Single authority — true if these parts describe an equipment-matters support
/// card. A card that is itself an Equipment is density, not support, so it is
/// rejected here. Shared by the detector and the live policy so the two never
/// drift; operates on the slices both a `CardFace` and a `GameObject` can supply.
pub(crate) fn parts_are_equipment_payoff(
    subtypes: &[String],
    effects: &[&Effect],
    triggers: &[&TriggerDefinition],
    statics: &[&StaticDefinition],
) -> bool {
    if subtypes_contain_equipment(subtypes) {
        return false;
    }
    effects.iter().copied().any(effect_is_equipment_support)
        || triggers.iter().copied().any(trigger_references_equipment)
        || statics.iter().copied().any(static_grants_equip)
}

/// CR 701.23 / CR 301.5: an effect that fetches an Equipment (tutor) or attaches
/// one for free (auto-attacher). The Equipment's own equip ability attaches
/// `SelfRef`, not an Equipment-filtered target, so it does not match here.
pub(crate) fn effect_is_equipment_support(effect: &Effect) -> bool {
    match effect {
        Effect::SearchLibrary { filter, .. } => target_filter_references_equipment(filter),
        Effect::Attach { attachment, .. } => target_filter_references_equipment(attachment),
        _ => false,
    }
}

/// CR 601.2: a trigger that fires off your Equipment (cast or ETB) — the
/// equipment-cast payoff (Puresteel Paladin's draw).
pub(crate) fn trigger_references_equipment(trigger: &TriggerDefinition) -> bool {
    trigger
        .valid_card
        .as_ref()
        .is_some_and(target_filter_references_equipment)
}

/// CR 702.6: a static that grants the Equip keyword (typically a cheaper equip,
/// e.g. Puresteel Paladin's `equip {0}`) to your Equipment.
pub(crate) fn static_grants_equip(static_def: &StaticDefinition) -> bool {
    static_def.modifications.iter().any(|modification| {
        matches!(
            modification,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Equip(_),
            }
        )
    })
}

/// CR 608.2b: unwrap a target filter and report whether it references the
/// Equipment subtype.
fn target_filter_references_equipment(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_references_equipment(typed),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_references_equipment)
        }
        _ => false,
    }
}

fn typed_filter_references_equipment(typed: &TypedFilter) -> bool {
    typed.type_filters.iter().any(type_filter_is_equipment)
}

/// CR 205.3 / CR 301.5: the Equipment subtype, including inside an `AnyOf`
/// disjunction ("an Aura or Equipment card", Steelshaper's Gift / Open the
/// Armory).
fn type_filter_is_equipment(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Subtype(sub) => sub.eq_ignore_ascii_case(EQUIPMENT_SUBTYPE),
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_equipment),
        _ => false,
    }
}

/// Flatten a face's ability chains and trigger-executed chains into the effect
/// slice the support predicate inspects. An equipment tutor/attach can live in a
/// Spell ability (Steelshaper's Gift), an activated ability (Brass Squire), or a
/// trigger's executed chain (Kor Outfitter's ETB), so all are walked.
fn collect_face_effects(face: &CardFace) -> Vec<&Effect> {
    let ability_effects = face.abilities.iter().flat_map(collect_chain_effects);
    let trigger_effects = face
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref())
        .flat_map(collect_chain_effects);
    ability_effects.chain(trigger_effects).collect()
}
