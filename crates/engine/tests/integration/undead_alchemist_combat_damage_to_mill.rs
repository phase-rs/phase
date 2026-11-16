//! Undead Alchemist — "If a Zombie you control would deal combat damage to a
//! player, instead that player mills that many cards."
//!
//! CR 614.1a + CR 614.6: the damage-substitution class (Soul-Scar Mage,
//! Szadek) with a typed source scope and a mill-only substitute. The Zombie's
//! combat damage to the player never happens; the player mills that many.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

fn library_count(state: &engine::types::game_state::GameState) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1 exists")
        .library
        .len()
}

/// CR 614.6: a Zombie's combat damage to the player becomes a mill of that
/// many; a non-Zombie attacker's damage is dealt normally (CR 614.1a scope).
#[test]
fn undead_alchemist_replaces_zombie_combat_damage_with_mill() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Undead Alchemist",
        4,
        2,
        "If a Zombie you control would deal combat damage to a player, instead that player \
         mills that many cards.",
    );
    let zombie = scenario
        .add_creature(P0, "Rotting Zombie", 3, 3)
        .with_subtypes(vec!["Zombie"])
        .id();
    let bear = scenario.add_creature(P0, "Bear", 2, 2).id();
    scenario.with_library_top(P1, &["C1", "C2", "C3", "C4", "C5", "C6", "C7"]);

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (zombie, AttackTarget::Player(P1)),
            (bear, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    let outcome = runner.combat_damage();

    assert_eq!(
        outcome.life_delta(P1),
        -2,
        "only the non-Zombie's damage is dealt"
    );
    assert_eq!(
        library_count(outcome.state()),
        4,
        "the Zombie's 3 is milled"
    );
    outcome.assert_zone_count(P1, Zone::Graveyard, 3);
}
