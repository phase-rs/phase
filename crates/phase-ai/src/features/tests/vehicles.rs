//! Unit tests for `features::vehicles` — CR 702.122 crewed-Vehicle detection.
//! No `#[cfg(test)]` in SOURCE files; tests live here.

use engine::game::DeckEntry;
use engine::types::ability::PtValue;
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType};
use engine::types::keywords::Keyword;

use crate::features::vehicles::*;

fn face(name: &str, core: CoreType) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: Vec::new(),
            core_types: vec![core],
            subtypes: Vec::new(),
        },
        ..Default::default()
    }
}

fn entry(card: CardFace, count: u32) -> DeckEntry {
    DeckEntry { card, count }
}

/// A Vehicle carrying `Keyword::Crew { power }`.
fn vehicle(name: &str, crew: u32) -> CardFace {
    let mut f = face(name, CoreType::Artifact);
    f.card_type.subtypes.push("Vehicle".to_string());
    f.keywords.push(Keyword::Crew {
        power: crew,
        once_per_turn: None,
    });
    f
}

/// A creature that can be tapped to crew.
fn body(name: &str, power: i32) -> CardFace {
    let mut f = face(name, CoreType::Creature);
    f.power = Some(PtValue::Fixed(power));
    f.toughness = Some(PtValue::Fixed(power.max(1)));
    f
}

/// `vehicles` Vehicles + `bodies` crew-capable creatures, padded to 36 nonland.
fn deck(vehicles: u32, bodies: u32) -> Vec<DeckEntry> {
    let filler = 36u32.saturating_sub(vehicles + bodies);
    vec![
        entry(vehicle("Copter", 1), vehicles),
        entry(body("Bear", 2), bodies),
        entry(face("Filler", CoreType::Enchantment), filler),
    ]
}

#[test]
fn empty_deck_produces_defaults() {
    let f = detect(&[]);
    assert_eq!(f.vehicle_count, 0);
    assert_eq!(f.crew_body_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn detects_vehicles_and_crew_cost() {
    let f = detect(&[
        entry(vehicle("Skysovereign", 3), 5),
        entry(body("Bear", 2), 10),
        entry(face("Filler", CoreType::Enchantment), 21),
    ]);
    assert_eq!(f.vehicle_count, 5);
    assert_eq!(f.total_crew_cost, 15, "Crew 3 x 5 copies");
}

#[test]
fn detects_crew_bodies_and_power() {
    let f = detect(&deck(5, 10));
    assert_eq!(f.crew_body_count, 10);
    assert_eq!(f.total_crew_power, 20, "power 2 x 10 bodies");
}

#[test]
fn a_vehicle_is_not_its_own_crew_body() {
    // CR 702.122a: crew taps OTHER creatures. A Vehicle that is also printed as
    // a creature card must not count toward its own bench.
    let mut creature_vehicle = vehicle("Creature Vehicle", 2);
    creature_vehicle
        .card_type
        .core_types
        .push(CoreType::Creature);
    creature_vehicle.power = Some(PtValue::Fixed(4));
    let f = detect(&[
        entry(creature_vehicle, 8),
        entry(face("Filler", CoreType::Enchantment), 28),
    ]);
    assert_eq!(f.vehicle_count, 8);
    assert_eq!(f.crew_body_count, 0, "a Vehicle can never crew itself");
    assert_eq!(f.commitment, 0.0, "no bench ⇒ the axis collapses");
}

#[test]
fn zero_power_creature_is_not_a_crew_body() {
    // Tapping a 0-power creature contributes nothing toward `N`.
    let f = detect(&[
        entry(vehicle("Copter", 1), 5),
        entry(body("Wall", 0), 10),
        entry(face("Filler", CoreType::Enchantment), 21),
    ]);
    assert_eq!(f.crew_body_count, 0);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn variable_power_creature_is_not_counted() {
    // A `*` power has no deck-time value; the axis under-counts rather than
    // inventing a bench.
    let mut star = face("Tarmogoyf", CoreType::Creature);
    star.power = Some(PtValue::Variable("*".to_string()));
    let f = detect(&[
        entry(vehicle("Copter", 1), 5),
        entry(star, 10),
        entry(face("Filler", CoreType::Enchantment), 21),
    ]);
    assert_eq!(f.crew_body_count, 0);
}

#[test]
fn subtype_only_vehicle_has_no_crew_requirement() {
    // CR 702.122a: Crew is an activated ability; the subtype does not grant it.
    // Archetype membership is the broader question and stays true — see the
    // sibling test below — but the live authority must report `None`.
    let mut subtype_only = face("Odd Vehicle", CoreType::Artifact);
    subtype_only.card_type.subtypes.push("Vehicle".to_string());
    assert_eq!(crew_requirement(&subtype_only), None);
    assert!(vehicle_archetype_member(&subtype_only));

    let real = vehicle("Copter", 2);
    assert_eq!(crew_requirement(&real), Some(2));
    assert!(vehicle_archetype_member(&real));
}

#[test]
fn vehicle_subtype_without_the_keyword_still_registers() {
    // A Vehicle whose crew keyword the parser has not attached is still part of
    // the archetype — it just contributes no crew cost.
    let mut subtype_only = face("Odd Vehicle", CoreType::Artifact);
    subtype_only.card_type.subtypes.push("Vehicle".to_string());
    let f = detect(&[
        entry(subtype_only, 5),
        entry(body("Bear", 2), 10),
        entry(face("Filler", CoreType::Enchantment), 21),
    ]);
    assert_eq!(f.vehicle_count, 5);
    assert_eq!(f.total_crew_cost, 0);
}

#[test]
fn noncreature_is_not_a_crew_body() {
    let f = detect(&[
        entry(vehicle("Copter", 1), 5),
        entry(face("Signet", CoreType::Artifact), 10),
        entry(face("Filler", CoreType::Enchantment), 21),
    ]);
    assert_eq!(f.crew_body_count, 0);
}

#[test]
fn vehicles_without_a_bench_collapse_commitment() {
    let f = detect(&[
        entry(vehicle("Copter", 1), 5),
        entry(face("Filler", CoreType::Enchantment), 31),
    ]);
    assert_eq!(f.vehicle_count, 5);
    assert_eq!(f.commitment, 0.0);
}

#[test]
fn creatures_without_vehicles_collapse_commitment() {
    let f = detect(&[
        entry(body("Bear", 2), 20),
        entry(face("Filler", CoreType::Enchantment), 16),
    ]);
    assert_eq!(f.vehicle_count, 0);
    assert_eq!(f.commitment, 0.0);
}

// ─── calibration anchors: computed values, pinned ────────────────────────────

#[test]
fn dedicated_shell_saturates() {
    let f = detect(&deck(8, 16));
    assert!(
        (f.commitment - 1.000).abs() < 0.01,
        "expected 1.000, got {}",
        f.commitment
    );
}

#[test]
fn realistic_build_hits_its_anchor() {
    let f = detect(&deck(5, 10));
    assert!(
        (f.commitment - 0.833).abs() < 0.01,
        "expected ≈0.833, got {}",
        f.commitment
    );
    assert!(f.commitment >= VEHICLES_FLOOR);
}

#[test]
fn light_build_hits_its_anchor() {
    let f = detect(&deck(3, 6));
    assert!(
        (f.commitment - 0.645).abs() < 0.01,
        "expected ≈0.645, got {}",
        f.commitment
    );
    assert!(f.commitment >= VEHICLES_FLOOR);
}

#[test]
fn two_vehicle_splash_still_clears_the_floor() {
    let f = detect(&deck(2, 4));
    assert!(
        (f.commitment - 0.527).abs() < 0.01,
        "expected ≈0.527, got {}",
        f.commitment
    );
    assert!(f.commitment >= VEHICLES_FLOOR);
}

#[test]
fn one_incidental_copter_stays_below_the_floor() {
    // The case that drove the pillar scaling: a saturated bench must NOT
    // compensate for near-zero Vehicle density.
    let f = detect(&deck(1, 20));
    assert!(
        (f.commitment - 0.373).abs() < 0.01,
        "expected ≈0.373, got {}",
        f.commitment
    );
    assert!(
        f.commitment < VEHICLES_FLOOR,
        "one Copter in a creature deck must not switch the axis on, got {}",
        f.commitment
    );
}

#[test]
fn commitment_is_format_size_neutral() {
    let sixty = detect(&deck(5, 10));
    let commander = detect(&[
        entry(vehicle("Copter", 1), 9),
        entry(body("Bear", 2), 18),
        entry(face("Filler", CoreType::Enchantment), 36),
    ]);
    assert!(
        (sixty.commitment - commander.commitment).abs() < 0.05,
        "{} vs {}",
        sixty.commitment,
        commander.commitment
    );
}

#[test]
fn lands_are_excluded_from_the_denominator() {
    let with_lands = detect(&[
        entry(vehicle("Copter", 1), 5),
        entry(body("Bear", 2), 10),
        entry(face("Filler", CoreType::Enchantment), 21),
        entry(face("Plains", CoreType::Land), 24),
    ]);
    assert_eq!(with_lands.commitment, detect(&deck(5, 10)).commitment);
}
