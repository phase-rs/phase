//! Reanimator / graveyard-recursion feature — structural detection over a
//! deck's typed AST.
//!
//! Parser AST verification — VERIFIED (no parser remediation required; every
//! axis classifies from the existing typed AST, never by card name):
//! - Reanimation payoff: `Effect::ChangeZone { origin: Some(Zone::Graveyard),
//!   destination: Zone::Battlefield, target, .. }` at `ability.rs:7147` —
//!   CR 404.1 (graveyard is the discard pile) + CR 110.1 (the card becomes a
//!   permanent as it enters the battlefield). The reanimated body is a creature
//!   (`TypeFilter::Creature`) or a Vehicle (`TypeFilter::Subtype("Vehicle")`,
//!   CR 301.7 / CR 205.3) — the two card classes a reanimation effect cheats
//!   onto the battlefield ahead of curve (Reanimate, Unburial Rites, Exhume,
//!   Goryo's Vengeance, Greasefang's vehicle return).
//! - Self-mill enabler: `Effect::Mill { target, destination: Zone::Graveyard }`
//!   at `ability.rs:7075` (CR 701.17a) scoped to the controller — loads your own
//!   graveyard (Stitcher's Supplier, Stinkweed Imp).
//! - Discard-as-resource enabler: `Effect::Discard { target, .. }` at
//!   `ability.rs:7861` (CR 701.9a) scoped to the controller, or an activated
//!   ability whose cost discards from hand (`CostCategory::Discards`) — the
//!   looting / rummaging outlets that pitch fat bodies into the graveyard
//!   (Faithless Looting, "Discard a card:" outlets).
//! - Reanimation target/fuel: a creature or Vehicle face whose mana value is at
//!   least [`REANIMATION_TARGET_MV_FLOOR`] — the expensive body worth reanimating.
//!
//! Why this is not redundant with existing graveyard handling: `mill_targeting`
//! optimizes *who* to mill (and penalizes self-mill when not obviously useful)
//! and `recursion_awareness` is a *targeting* heuristic that avoids feeding an
//! opponent's recursive creature to removal — neither recognizes a deck whose
//! plan is "fill the graveyard, then reanimate a threat." This axis fills that
//! gap; the companion `ReanimatorPayoffPolicy` is payoff-gated so non-reanimator
//! decks (and the `mill_targeting` self-mill penalty) are unaffected.

use engine::game::DeckEntry;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, CostCategory, Effect, TargetFilter, TypeFilter, TypedFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::features::commitment;

/// Commitment floor below which `ReanimatorPayoffPolicy` opts out. Matches the
/// lifegain/enchantments payoff-axis convention.
pub const COMMITMENT_FLOOR: f32 = 0.30;

/// Mana value at or above which a creature/Vehicle face is considered a
/// worthwhile reanimation target — the "ahead of curve" body a reanimation
/// spell cheats onto the battlefield. Five keeps the class to genuine payoffs
/// (Archon of Cruelty, Parhelion II, Griselbrand) and excludes the cheap
/// creatures every deck runs.
pub const REANIMATION_TARGET_MV_FLOOR: u32 = 5;

/// Per-60-nonland payoff density at which the reanimation pillar saturates.
/// Dedicated reanimator shells run ~6–10 reanimation spells per 60 nonland.
const PAYOFF_FULL_DENSITY: f32 = 6.0;

/// Per-60-nonland target density at which the fuel pillar saturates.
const TARGET_FULL_DENSITY: f32 = 6.0;

/// Additive commitment per graveyard enabler, capped by [`ENABLER_BONUS_CAP`].
/// Enablers are supporting fuel, not the intent signal, so they only nudge.
const ENABLER_BONUS_PER_CARD: f32 = 0.03;

/// Maximum commitment contributed by enablers alone.
const ENABLER_BONUS_CAP: f32 = 0.20;

/// CR 205.3 / CR 301.7: the artifact subtype a reanimation target/payoff can be
/// besides a creature. Compared case-insensitively against `TypeFilter::Subtype`
/// and `CardType.subtypes`.
const VEHICLE_SUBTYPE: &str = "Vehicle";

/// Per-deck reanimator classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.{abilities,triggers,card_type,mana_cost}` — never by card name. The
/// companion `ReanimatorPayoffPolicy` consumes this to value reanimation spells
/// and graveyard enablers when the deck contains both payoffs and targets.
#[derive(Debug, Clone, Default)]
pub struct ReanimatorFeature {
    /// Cards that put a creature/Vehicle from a graveyard onto the battlefield.
    /// CR 404.1 + CR 110.1. The payoff — the reanimation itself.
    pub reanimation_count: u32,
    /// Cards that load your graveyard: self-mill (CR 701.17a) or discard-as-
    /// resource (CR 701.9a). The fuel that fills the graveyard to reanimate from.
    pub enabler_count: u32,
    /// Expensive creature/Vehicle bodies worth reanimating
    /// ([`REANIMATION_TARGET_MV_FLOOR`]+ mana value). Without a target, a pile of
    /// reanimation spells has nothing to cheat out, so this is a required pillar.
    pub target_count: u32,
    /// `0.0..=1.0` — how central the reanimator plan is to this deck. Requires
    /// both reanimation payoffs and targets; enablers add a bounded bonus.
    /// Consumed by `ReanimatorPayoffPolicy::activation` as the scaling knob.
    pub commitment: f32,
}

/// Structural detection — walks each `DeckEntry`'s `CardFace` AST and counts
/// reanimation payoffs, graveyard enablers, and reanimation targets.
pub fn detect(deck: &[DeckEntry]) -> ReanimatorFeature {
    if deck.is_empty() {
        return ReanimatorFeature::default();
    }

    let mut reanimation_count = 0u32;
    let mut enabler_count = 0u32;
    let mut target_count = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }
        if is_reanimation_payoff(face) {
            reanimation_count = reanimation_count.saturating_add(entry.count);
        }
        if is_graveyard_enabler(face) {
            enabler_count = enabler_count.saturating_add(entry.count);
        }
        if is_reanimation_target(face) {
            target_count = target_count.saturating_add(entry.count);
        }
    }

    let commitment = reanimator_commitment(
        reanimation_count,
        target_count,
        enabler_count,
        total_nonland,
    );

    ReanimatorFeature {
        reanimation_count,
        enabler_count,
        target_count,
        commitment,
    }
}

/// Geometric-mean commitment over the two required pillars (reanimation payoffs
/// and reanimation targets) plus a bounded enabler bonus.
///
/// A reanimator deck needs BOTH a way to reanimate AND a body worth reanimating;
/// missing either pillar means it is not a reanimator deck, so commitment
/// collapses to the small enabler bonus (which keeps the policy opted out for
/// graveyard-value decks that merely self-mill).
///
/// Calibration — Legacy Reanimate (≈36 nonland: 8 reanimation spells, 6 fat
/// targets, 6 looting/Entomb enablers): payoff density ≈13.3 and target density
/// ≈10 both saturate their pillars → geometric mean 1.0 + capped enabler bonus →
/// commitment ≈1.0, well above the floor.
///
/// Anti-calibration — a midrange deck with one incidental reanimation spell and
/// no fat target collapses to the enabler bonus (≤0.20), staying inert.
fn reanimator_commitment(
    reanimation_count: u32,
    target_count: u32,
    enabler_count: u32,
    total_nonland: u32,
) -> f32 {
    let enabler_bonus = (ENABLER_BONUS_PER_CARD * enabler_count as f32).min(ENABLER_BONUS_CAP);

    // Both pillars are mandatory — a reanimation spell with nothing worth
    // reanimating, or a fat creature with no way to cheat it out, is not the
    // reanimator plan.
    if reanimation_count == 0 || target_count == 0 {
        return enabler_bonus.min(1.0);
    }

    let payoff = (commitment::density_per_60(reanimation_count, total_nonland)
        / PAYOFF_FULL_DENSITY)
        .min(1.0);
    let target =
        (commitment::density_per_60(target_count, total_nonland) / TARGET_FULL_DENSITY).min(1.0);

    (commitment::geometric_mean(&[payoff, target]) + enabler_bonus).min(1.0)
}

/// True if this face can reanimate — its ability/trigger effect chains put a
/// creature or Vehicle from a graveyard onto the battlefield. CR 404.1 + CR 110.1.
pub fn is_reanimation_payoff(face: &CardFace) -> bool {
    effects_include_reanimation(&collect_face_effects(face))
}

/// True if this face loads your graveyard — a controller-scoped self-mill or
/// discard effect (CR 701.17a / CR 701.9a), or an activated ability whose cost
/// discards from hand. These are the enablers that fill the graveyard to
/// reanimate from.
pub fn is_graveyard_enabler(face: &CardFace) -> bool {
    effects_include_self_graveyard_fill(&collect_face_effects(face))
        || face.abilities.iter().any(ability_is_discard_outlet)
}

/// True if this face is a worthwhile reanimation target: a creature or Vehicle
/// body whose mana value is at least [`REANIMATION_TARGET_MV_FLOOR`].
pub fn is_reanimation_target(face: &CardFace) -> bool {
    face_is_reanimatable_body(face) && face.mana_cost.mana_value() >= REANIMATION_TARGET_MV_FLOOR
}

/// Single authority — true if any effect in the slice is a reanimation effect.
/// Shared by deck-time `CardFace` detection ([`is_reanimation_payoff`]) and the
/// live-game `ReanimatorPayoffPolicy` so the two never drift.
pub(crate) fn effects_include_reanimation(effects: &[&Effect]) -> bool {
    effects.iter().copied().any(effect_is_reanimation)
}

/// Single authority — true if any effect in the slice fills the controller's own
/// graveyard (self-mill or self-discard). Shared with the live policy.
pub(crate) fn effects_include_self_graveyard_fill(effects: &[&Effect]) -> bool {
    effects.iter().copied().any(effect_fills_own_graveyard)
}

/// Single authority — true if this ability discards from hand as part of its
/// cost (a looting / rummaging outlet). CR 701.9a. Shared with the live policy.
/// Uses `cost_categories()` per the single-authority cost rule — never
/// destructures `AbilityCost::Discard`.
pub(crate) fn ability_is_discard_outlet(ability: &AbilityDefinition) -> bool {
    ability.cost_categories().contains(&CostCategory::Discards)
}

/// CR 404.1 + CR 110.1: a reanimation effect moves a creature/Vehicle card from
/// a graveyard onto the battlefield. Origin must be the graveyard; destination
/// the battlefield. An unset origin (`None`) is "from anywhere" and does not
/// qualify as a *reanimation* signature on its own.
fn effect_is_reanimation(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ChangeZone {
            origin: Some(Zone::Graveyard),
            destination: Zone::Battlefield,
            target,
            ..
        } if target_filter_is_reanimatable_body(target)
    )
}

/// CR 701.17a / CR 701.9a: an effect that fills the controller's own graveyard.
/// A self-mill must deposit into the graveyard (not exile); a discard always
/// goes to the graveyard. Both must be controller-scoped — a `Player`-targeted
/// mill/discard is opponent disruption (Mind Rot, Glimpse the Unthinkable), not
/// a self-enabler.
fn effect_fills_own_graveyard(effect: &Effect) -> bool {
    match effect {
        Effect::Mill {
            target,
            destination,
            ..
        } => *destination == Zone::Graveyard && target_fills_own_graveyard(target),
        Effect::Discard { target, .. } => target_fills_own_graveyard(target),
        _ => false,
    }
}

/// CR 608.2c: "you mill/discard" (`Controller`) and the unspecified default
/// (`Any`, e.g. "discard two cards" / "mill three cards") load the resolving
/// player's own graveyard. An explicitly targeted `Player` is opponent-facing.
fn target_fills_own_graveyard(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::Controller | TargetFilter::Any)
}

/// CR 608.2b: unwrap a reanimation target filter. Accepts a filter that
/// references a creature/Vehicle body controlled by you or unscoped; rejects
/// opponent-scoped filters. `And` requires every conjunct to match (CR 608.2c).
fn target_filter_is_reanimatable_body(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed_filter_is_reanimatable_body(typed),
        TargetFilter::Or { filters } => filters.iter().any(target_filter_is_reanimatable_body),
        TargetFilter::And { filters } => filters.iter().all(target_filter_is_reanimatable_body),
        _ => false,
    }
}

fn typed_filter_is_reanimatable_body(typed: &TypedFilter) -> bool {
    // Reject opponent-scoped reanimation (rare; not the AI's own payoff).
    if matches!(typed.controller, Some(ControllerRef::Opponent)) {
        return false;
    }
    typed.type_filters.iter().any(type_filter_is_body)
}

/// CR 301.7 / CR 205.3: a reanimatable body is a creature or a Vehicle.
fn type_filter_is_body(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Creature => true,
        TypeFilter::Subtype(sub) => sub.eq_ignore_ascii_case(VEHICLE_SUBTYPE),
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_body),
        _ => false,
    }
}

/// CR 301.7: a face is a reanimatable body if it is a creature card or carries
/// the Vehicle subtype (an artifact Vehicle becomes a creature once crewed, and
/// is the body a vehicle-reanimator returns).
fn face_is_reanimatable_body(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Creature)
        || face
            .card_type
            .subtypes
            .iter()
            .any(|sub| sub.eq_ignore_ascii_case(VEHICLE_SUBTYPE))
}

/// Flatten a face's ability chains and trigger-executed chains into the effect
/// slice the payoff/enabler predicates inspect. A reanimation effect can live in
/// a `Spell` ability (Reanimate), an activated ability, or a trigger's executed
/// chain (Greasefang's attack trigger), so all are walked.
fn collect_face_effects(face: &CardFace) -> Vec<&Effect> {
    let ability_effects = face.abilities.iter().flat_map(collect_chain_effects);
    let trigger_effects = face
        .triggers
        .iter()
        .filter_map(|trigger| trigger.execute.as_deref())
        .flat_map(collect_chain_effects);
    ability_effects.chain(trigger_effects).collect()
}
