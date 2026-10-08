//! Vehicles feature — structural detection of a deck that wins through crewed
//! Vehicles (CR 702.122).
//!
//! Parser AST verification — VERIFIED against engine source:
//! - `Keyword::Crew { power, once_per_turn }` at
//!   `crates/engine/src/types/keywords.rs:707` — the Vehicle payoff and, in
//!   `power`, the exact total power CR 702.122a requires to crew it.
//! - `CardFace.keywords: Vec<Keyword>` at `crates/engine/src/types/card.rs:60`.
//! - `CardFace.power: Option<PtValue>` at `card.rs:171`; `PtValue::Fixed(i32)`
//!   is the only deck-time-knowable form — a `*` power is `PtValue::Variable`
//!   and is excluded rather than guessed.
//! - Vehicle subtype via the shared `reanimator::VEHICLE_SUBTYPE` constant,
//!   promoted rather than redefined so the two features cannot disagree about
//!   what a Vehicle is. The subtype widens ARCHETYPE membership only — CR 702.122a
//!   makes Crew an activated ability, so it never stands in for a crew
//!   requirement at the live seam.
//!
//! No parser remediation required — every axis is expressible over existing
//! typed AST.
//!
//! ## Why this axis exists
//!
//! CR 702.122a: "Crew N" means *tap any number of OTHER untapped creatures you
//! control with total power N or greater*. A Vehicle is therefore not a body the
//! deck pays mana for — it is a body the deck pays *board presence* for, and it
//! does nothing at all without creatures to tap.
//!
//! `CrewTimingPolicy` already decides whether a specific crew activation is
//! worth it, but it runs with `activation-constant Some(1.0)` because there is
//! no vehicles deck-feature for it to scale by. So it applies the same weight in
//! a dedicated Vehicles shell as in a deck with one incidental Smuggler's
//! Copter, and nothing anywhere values *casting* a Vehicle against whether the
//! board can actually crew it.
//!
//! ## Boundary with `equipment`
//!
//! Both axes attach value to a noncreature permanent that needs creatures.
//! Equipment moves stats onto an existing body (CR 702.6); a Vehicle BECOMES the
//! body and taps the creatures instead (CR 702.122a). The costs run opposite
//! ways — Equipment wants creatures on the battlefield afterwards, crewing takes
//! them out of combat — so the two never read the same card and a deck scoring
//! high on both is running two distinct plans.
//!
//! ## Boundary with `artifacts`
//!
//! `artifacts` counts artifact density and artifact-matters payoffs. Vehicles are
//! artifacts, so a Vehicles deck reads on that axis too — intentionally. This
//! axis is the narrower question of whether those artifacts are crewable bodies
//! with the creatures to crew them, which artifact density cannot answer.

use engine::game::DeckEntry;
use engine::types::ability::PtValue;
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;

use crate::features::commitment;
use crate::features::reanimator::VEHICLE_SUBTYPE;

/// Commitment at or above which Vehicles are a real plan rather than one
/// incidental Copter. Gates the vehicles-aware policies' `activation`.
pub const VEHICLES_FLOOR: f32 = 0.40;

/// Vehicle density (per 60 nonland) at which the payoff pillar saturates.
const VEHICLE_SATURATION_PER_60: f32 = 12.0;

/// CR 702.122a: crew taps *any number* of other creatures, so a Vehicle deck
/// needs a bench, not one big body. Two crew-capable creatures per Vehicle is
/// treated as full support.
const BODIES_PER_VEHICLE: f32 = 2.0;

/// CR 702.122: per-deck Vehicles classification.
///
/// Populated once per game from `DeckEntry` data. Detection is structural over
/// `CardFace.keywords` and the printed type line — never by card name.
#[derive(Debug, Clone, Default)]
pub struct VehiclesFeature {
    /// Cards carrying `Keyword::Crew` — the Vehicles themselves.
    pub vehicle_count: u32,
    /// Summed `Crew N` requirement across those Vehicles. Consumed by policies
    /// that need to know how much board presence the plan costs to switch on.
    pub total_crew_cost: u32,
    /// Creatures whose printed power can contribute to paying a crew cost
    /// (CR 702.122a) — power must be a known, positive, fixed value.
    pub crew_body_count: u32,
    /// Summed printed power of those bodies.
    pub total_crew_power: u32,
    /// `0.0..=1.0` — how central crewed Vehicles are to this deck.
    pub commitment: f32,
}

/// Structural detection over each `DeckEntry`'s `CardFace` AST.
pub fn detect(deck: &[DeckEntry]) -> VehiclesFeature {
    if deck.is_empty() {
        return VehiclesFeature::default();
    }

    let mut vehicle_count = 0u32;
    let mut total_crew_cost = 0u32;
    let mut crew_body_count = 0u32;
    let mut total_crew_power = 0u32;
    let mut total_nonland = 0u32;

    for entry in deck {
        let face = &entry.card;
        if !face.card_type.core_types.contains(&CoreType::Land) {
            total_nonland = total_nonland.saturating_add(entry.count);
        }

        if vehicle_archetype_member(face) {
            vehicle_count = vehicle_count.saturating_add(entry.count);
            // Only a real `Keyword::Crew` contributes a cost — a subtype-only
            // Vehicle joins the archetype but adds nothing to pay.
            if let Some(crew) = crew_requirement(face) {
                total_crew_cost = total_crew_cost.saturating_add(crew.saturating_mul(entry.count));
            }
        }

        // CR 702.122a: a Vehicle taps OTHER creatures, so it can never crew
        // itself — an uncrewed Vehicle is not a creature and contributes no
        // power. Counting it as its own bench would let a pile of Vehicles look
        // self-sufficient.
        if let Some(power) = crew_capable_power(face) {
            crew_body_count = crew_body_count.saturating_add(entry.count);
            total_crew_power = total_crew_power.saturating_add(power.saturating_mul(entry.count));
        }
    }

    let commitment = compute_commitment(vehicle_count, crew_body_count, total_nonland);

    VehiclesFeature {
        vehicle_count,
        total_crew_cost,
        crew_body_count,
        total_crew_power,
        commitment,
    }
}

/// CR 702.122a: the total power required to crew this face, or `None` when it is
/// not a Vehicle.
///
/// Keyed on `Keyword::Crew` rather than the Vehicle subtype: the keyword is what
/// actually grants the crew ability, and a card can carry it without the printed
/// subtype (or carry the subtype with its crew ability granted elsewhere). The
/// subtype is still accepted as a fallback so a Vehicle whose crew keyword the
/// parser has not attached is not silently dropped from the archetype — it just
/// contributes no crew cost.
pub(crate) fn crew_requirement(face: &CardFace) -> Option<u32> {
    crew_requirement_parts(face.keywords.iter())
}

/// CR 702.122a: the total power required to crew, or `None` when this object has
/// no crew ability at all.
///
/// Keyed STRICTLY on `Keyword::Crew`. CR 702.122a opens *"Crew is an activated
/// ability of Vehicle cards"* — the printed subtype does not grant it. A
/// subtype-only Vehicle therefore has no crew requirement to satisfy, and must
/// not be reported as one: returning `Some(0)` here made `available >= required`
/// true for an empty board (`0 >= 0`) and scored a live crew bonus for a
/// permanent that can never be crewed.
///
/// Deck classification asks a different question and uses
/// [`vehicle_archetype_member`] instead — see that function for why the subtype
/// fallback is honest THERE and wrong here.
///
/// Parts-based so it classifies a deck-time `CardFace.keywords` slice and a live
/// `GameObject.keywords` slice through one predicate.
pub(crate) fn crew_requirement_parts<'a>(
    keywords: impl IntoIterator<Item = &'a Keyword>,
) -> Option<u32> {
    keywords.into_iter().find_map(|keyword| match keyword {
        Keyword::Crew { power, .. } => Some(*power),
        _ => None,
    })
}

/// Is this face part of the Vehicles ARCHETYPE?
///
/// Deliberately broader than [`crew_requirement`]: a Vehicle whose crew keyword
/// the parser has not attached is still a Vehicle the deck is built around, and
/// dropping it would understate the archetype. That fallback is safe for deck
/// classification — it only decides how strongly the axis activates — and is NOT
/// safe at the live seam, where it would invent a crew cost of zero for a
/// permanent with no crew ability.
pub(crate) fn vehicle_archetype_member(face: &CardFace) -> bool {
    crew_requirement(face).is_some() || face_has_vehicle_subtype(face)
}

/// CR 205.3: the printed type line carries the Vehicle subtype.
pub(crate) fn face_has_vehicle_subtype(face: &CardFace) -> bool {
    face.card_type
        .subtypes
        .iter()
        .any(|subtype| subtype.eq_ignore_ascii_case(VEHICLE_SUBTYPE))
}

/// CR 702.122a: the printed power this face can contribute toward a crew cost,
/// or `None` when it cannot contribute.
///
/// Requires a creature with a KNOWN, positive, fixed power:
/// * A Vehicle is excluded even when it is also a creature card, because crew
///   taps *other* creatures.
/// * `PtValue::Variable` ("*", "1+*") has no deck-time value, so it is excluded
///   rather than guessed — the axis under-counts instead of inventing a bench.
/// * Power 0 taps for nothing and never helps reach `N`.
pub(crate) fn crew_capable_power(face: &CardFace) -> Option<u32> {
    if !face.card_type.core_types.contains(&CoreType::Creature) {
        return None;
    }
    if face_has_vehicle_subtype(face) {
        return None;
    }
    match face.power {
        Some(PtValue::Fixed(power)) if power > 0 => u32::try_from(power).ok(),
        _ => None,
    }
}

/// Calibration — every figure below is the value `compute_commitment` actually
/// returns, verified before being written down:
/// - Dedicated Vehicles shell — 8 Vehicles + 16 crew-capable creatures over 36
///   nonland → **1.000**.
/// - Realistic build — 5 Vehicles + 10 bodies → **0.833**.
/// - Light build — 3 Vehicles + 6 bodies → **0.645**.
/// - Two-Vehicle splash — 2 Vehicles + 4 bodies → **0.527**, still a plan.
///
/// Anti-calibration:
/// - One incidental Smuggler's Copter in a 20-creature deck → **0.373**, below
///   `VEHICLES_FLOOR`. This case drove the pillar scaling: a saturated bench must
///   NOT compensate for near-zero Vehicle density, or a single Copter would
///   switch the whole axis on.
/// - Vehicles with no bench (5 Vehicles + 0 creatures) → 0.0.
/// - Creatures with no Vehicles → 0.0.
///
/// Geometric mean over (vehicle density, crew support): BOTH pillars are
/// mandatory. Vehicles with nothing to tap are artifacts that never become
/// creatures; creatures with no Vehicles are just a creature deck.
fn compute_commitment(vehicle_count: u32, crew_body_count: u32, total_nonland: u32) -> f32 {
    let vehicle_density = (commitment::density_per_60(vehicle_count, total_nonland)
        / VEHICLE_SATURATION_PER_60)
        .min(1.0);
    // Support is measured against THIS deck's own Vehicle count rather than a
    // fixed density: a deck needs a bench proportional to the Vehicles it runs,
    // and that ratio is already deck-size neutral.
    let crew_support = if vehicle_count == 0 {
        0.0
    } else {
        (crew_body_count as f32 / (vehicle_count as f32 * BODIES_PER_VEHICLE)).min(1.0)
    };
    commitment::geometric_mean(&[vehicle_density, crew_support])
}
