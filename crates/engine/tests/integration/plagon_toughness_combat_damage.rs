//! Plagon, Lord of the Beach — end-to-end runtime proof of the
//! damage-by-toughness activated ability (CR 510.1a + CR 613.11).
//!
//! Oracle:
//! > When Plagon enters, draw a card for each creature you control with
//! > toughness greater than its power.
//! > {W/U}: Target creature you control assigns combat damage equal to its
//! > toughness rather than its power this turn.
//!
//! The `{W/U}` line is the singular one-shot EFFECT form: the effect pipeline
//! deconjugates "assigns" → "assign" (`normalize_verb_token`) before the shared
//! damage-by-toughness predicate runs, so the deconjugated-singular surface
//! ("assign … its … its") must be accepted. Before the fix that surface matched
//! no combinator and the whole clause lowered to `Effect::Unimplemented`, so the
//! ability did nothing and the targeted creature dealt its power in combat.
//!
//! This test drives the real activation + combat pipeline: fund the hybrid cost
//! from the pool, activate targeting a 1/3, and prove via a combat-damage life
//! delta that the 1/3 dealt its toughness (3), not its power (1). If the parser
//! fix is reverted the ability lowers to Unimplemented, the 1/3 deals 1, and the
//! life delta flips from -5 to -3 — a revert-failing assertion.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::parse_oracle_text;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::ObjectId;

const PLAGON_ORACLE: &str = "When Plagon enters, draw a card for each creature you control with toughness greater than its power.\n{W/U}: Target creature you control assigns combat damage equal to its toughness rather than its power this turn.";

/// CR 510.1a + CR 613.11: activating Plagon's `{W/U}` ability on a 1/3 makes it
/// assign combat damage equal to its toughness (3) instead of its power (1),
/// while an untargeted 2/2 keeps assigning its power (2). Declaring both as
/// attackers against a fresh opponent yields a -5 life delta (3 + 2).
#[test]
fn plagon_activated_ability_makes_target_assign_toughness_in_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // A single White pip auto-resolves the {W/U} hybrid via pool finalize
    // (source auto-tap is not modeled; fund the pool directly).
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::White,
            ObjectId(9999),
            false,
            vec![],
        )],
    );

    // Plagon on the battlefield (pre-existing, so its ETB does not fire here).
    let plagon = {
        let mut b = scenario.add_creature(P0, "Plagon, Lord of the Beach", 3, 3);
        b.from_oracle_text(PLAGON_ORACLE);
        b.id()
    };
    // The targeted attacker: a 1/3 — power 1, toughness 3, so the substitution
    // is observable (3 ≠ 1).
    let beater = scenario.add_creature(P0, "Beater", 1, 3).id();
    // An untargeted 2/2 — multi-authority negative: its damage stays at power.
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();

    // The {W/U} line is Plagon's sole activated ability (the ETB is a trigger),
    // so the activation index is 0. Assert the shape to keep the index honest.
    let parsed = parse_oracle_text(
        PLAGON_ORACLE,
        "Plagon, Lord of the Beach",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &["Crab".to_string()],
    );
    assert_eq!(
        parsed.abilities.len(),
        1,
        "Plagon has exactly one activated ability (the {{W/U}} line)"
    );
    let wu_index = 0;

    let mut runner = scenario.build();

    // Activate the {W/U} ability targeting the 1/3.
    runner
        .activate(plagon, wu_index)
        .target_object(beater)
        .resolve();

    // Reach-guard (positive, non-vacuous): the resolved continuous effect set
    // the flag on the targeted creature only — the untargeted 2/2 is unaffected.
    // If the ability had lowered to Unimplemented this would be false and the
    // test would fail here rather than reaching the (identical-signed) combat
    // assertion below.
    assert!(
        runner.state().objects[&beater].assigns_damage_from_toughness,
        "the targeted 1/3 must assign combat damage from toughness after activation"
    );
    assert!(
        !runner.state().objects[&bystander].assigns_damage_from_toughness,
        "the untargeted 2/2 must NOT be affected (single-target authority)"
    );

    // Run combat: both creatures attack the opponent, no blockers.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (beater, AttackTarget::Player(P1)),
            (bystander, AttackTarget::Player(P1)),
        ])
        .expect("declaring the two attackers must be accepted");
    let outcome = runner.combat_damage();

    // CR 510.1a + CR 613.11: the 1/3 assigns its toughness (3) rather than its
    // power (1); the 2/2 assigns its power (2). 3 + 2 = 5 damage to P1. If the
    // parser fix is reverted the ability is Unimplemented → the 1/3 deals 1 →
    // delta -3, so this assertion is revert-failing.
    outcome.assert_life_delta(P1, -5);
}
