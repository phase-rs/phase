//! Combustion Technique {1}{R}, Instant — Lesson (Oracle text verified against the
//! Scryfall API):
//! "Combustion Technique deals damage equal to 2 plus the number of Lesson cards in
//! your graveyard to target creature. If that creature would die this turn, exile it
//! instead."
//!
//! The census flagged this face `wrong_object` (confirmed-misparses.toml). These tests
//! observe the printed behavior at runtime: the damage amount (CR 120.3e damage marked +
//! CR 704.5g lethal damage), and that the damaged creature, and only it, is exiled instead
//! of dying (CR 614.1a + CR 614.6).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const COMBUSTION_TECHNIQUE: &str = "Combustion Technique deals damage equal to 2 plus the number of Lesson cards in your graveyard to target creature. If that creature would die this turn, exile it instead.";

struct Board {
    runner: GameRunner,
    spell: ObjectId,
    target: ObjectId,
    bystander: ObjectId,
    shock: ObjectId,
}

/// P0 casts at an opposing creature with `target_toughness`; a second opposing 2/2
/// bystander stands by; `lessons` Lesson cards sit in P0's graveyard.
fn board(target_toughness: i32, lessons: usize) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
            .collect(),
    );
    let target = scenario
        .add_creature(P1, "Target Bear", target_toughness, target_toughness)
        .id();
    let bystander = scenario.add_creature(P1, "Bystander Bear", 2, 2).id();
    for i in 0..lessons {
        scenario
            .add_spell_to_graveyard(P0, &format!("Lesson {i}"), true)
            .with_subtypes(vec!["Lesson"]);
    }
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Combustion Technique", true, COMBUSTION_TECHNIQUE)
        .id();
    let shock = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Shock",
            true,
            "Shock deals 2 damage to target creature.",
        )
        .id();
    Board {
        runner: scenario.build(),
        spell,
        target,
        bystander,
        shock,
    }
}

/// Lethal damage: the creature is exiled, not put into the graveyard.
#[test]
fn lethal_damage_exiles_the_target_instead_of_dying() {
    let mut b = board(2, 0);
    let outcome = b.runner.cast(b.spell).target_object(b.target).resolve();
    assert_eq!(
        outcome.zone_of(b.target),
        Zone::Exile,
        "CR 614.1a: the creature that would die is exiled instead"
    );
}

/// CR 120.3e + CR 704.5g: damage is 2 + Lessons in YOUR graveyard. With two Lessons the damage is
/// 4, so a 4-toughness creature must be exiled (it would survive a bare 2).
#[test]
fn damage_counts_lessons_in_the_casters_graveyard() {
    let mut b = board(4, 2);
    let outcome = b.runner.cast(b.spell).target_object(b.target).resolve();
    assert_eq!(
        outcome.zone_of(b.target),
        Zone::Exile,
        "2 + 2 Lessons = 4 damage is lethal to a 4/4"
    );
}

/// Non-lethal damage leaves the creature alive: no exile without a death.
#[test]
fn nonlethal_damage_leaves_the_creature_on_the_battlefield() {
    let mut b = board(5, 0);
    let outcome = b.runner.cast(b.spell).target_object(b.target).resolve();
    assert_eq!(outcome.zone_of(b.target), Zone::Battlefield);
}

/// The replacement is about THAT creature only. A different creature dying later
/// this turn must reach the graveyard normally.
#[test]
fn other_creatures_dying_this_turn_are_not_exiled() {
    let mut b = board(5, 0);
    b.runner.cast(b.spell).target_object(b.target).resolve();
    // Kill the bystander with an ordinary damage spell cast after the replacement is live.
    let outcome = b.runner.cast(b.shock).target_object(b.bystander).resolve();
    assert_eq!(
        outcome.zone_of(b.bystander),
        Zone::Graveyard,
        "the replacement applies only to the damaged creature"
    );
}
