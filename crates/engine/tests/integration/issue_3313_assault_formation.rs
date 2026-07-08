//! Regression for GitHub issue #3313 — Assault Formation toughness combat damage.
//!
//! Oracle:
//!   Each creature you control assigns combat damage equal to its toughness
//!   rather than its power.
//!   {G}: Target creature with defender can attack this turn as though it
//!   didn't have defender.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

use super::rules::run_combat;

const ASSAULT_FORMATION_ORACLE: &str = "Each creature you control assigns combat damage equal to its toughness rather than its power.\n\
{G}: Target creature with defender can attack this turn as though it didn't have defender.\n\
{2}{G}: Creatures you control get +0/+1 until end of turn.";

#[test]
fn assault_formation_assigns_combat_damage_from_toughness() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let _formation = scenario
        .add_creature(P0, "Assault Formation", 0, 0)
        .as_enchantment()
        .from_oracle_text(ASSAULT_FORMATION_ORACLE)
        .id();
    let attacker = scenario.add_creature(P0, "Attacker", 1, 4).id();

    let mut runner = scenario.build();
    evaluate_layers(runner.state_mut());

    let obj = runner.state().objects.get(&attacker).expect("attacker");
    assert!(
        obj.assigns_damage_from_toughness,
        "Assault Formation should make creatures assign damage from toughness"
    );
    assert_eq!(obj.power, Some(1));
    assert_eq!(obj.toughness, Some(4));

    run_combat(&mut runner, vec![attacker], vec![]);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        16,
        "1/4 attacker should deal 4 combat damage under Assault Formation"
    );
}

#[test]
fn assault_formation_green_ability_lets_defender_attack_for_toughness_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let formation = scenario
        .add_creature(P0, "Assault Formation", 0, 0)
        .as_enchantment()
        .from_oracle_text(ASSAULT_FORMATION_ORACLE)
        .id();
    let defender = scenario.add_creature(P0, "Wall", 0, 4).defender().id();

    let mut runner = scenario.build();
    runner.state_mut().players[P0.0 as usize]
        .mana_pool
        .add(ManaUnit::new(ManaType::Green, formation, false, vec![]));

    let green_idx = runner.state().objects[&formation]
        .abilities
        .iter()
        .position(|a| {
            a.description
                .as_deref()
                .is_some_and(|d| d.contains("defender can attack"))
        })
        .expect("Assault Formation green activated ability");

    runner
        .activate(formation, green_idx)
        .target_object(defender)
        .resolve();

    evaluate_layers(runner.state_mut());

    run_combat(&mut runner, vec![defender], vec![]);

    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        16,
        "0/4 defender should deal 4 combat damage after paying green"
    );
}

#[test]
fn assault_formation_static_parses_assign_damage_from_toughness() {
    use engine::parser::oracle_static::parse_static_line_multi;
    use engine::types::ability::ContinuousModification;

    let defs = parse_static_line_multi(
        "Each creature you control assigns combat damage equal to its toughness rather than its power.",
    );
    assert_eq!(
        defs.len(),
        1,
        "expected one continuous static, got {defs:?}"
    );
    assert!(
        defs[0]
            .modifications
            .contains(&ContinuousModification::AssignDamageFromToughness),
        "Assault Formation static must assign damage from toughness"
    );
}
