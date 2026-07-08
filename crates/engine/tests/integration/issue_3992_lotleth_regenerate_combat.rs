//! Regression for issue #3992: Regenerate must save Lotleth Troll from lethal
//! damage when activated before lethal SBAs run.
//!
//! CR 510.1: Combat damage is dealt after blockers are declared.
//! CR 704.3: SBAs run before a player receives priority, so regenerate must be
//! activated before lethal damage is dealt — not after damage is marked.
//!
//! End-to-end combat-damage coverage lives in
//! `combat_damage::tests::regeneration_shield_survives_combat_damage_resolution`.
//!
//! https://github.com/phase-rs/phase/issues/3992

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::Effect;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const LOTLETH_ORACLE: &str = "Trample\n\
Discard a creature card: Put a +1/+1 counter on Lotleth Troll.\n\
{B}: Regenerate Lotleth Troll";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, engine::types::identifiers::ObjectId(0), false, vec![]))
        .collect()
}

fn regenerate_ability_index(
    runner: &engine::game::scenario::GameRunner,
    creature: engine::types::identifiers::ObjectId,
) -> usize {
    runner.state().objects[&creature]
        .abilities
        .iter()
        .position(|a| matches!(*a.effect, Effect::Regenerate { .. }))
        .expect("creature must parse a Regenerate ability")
}

#[test]
fn lotleth_regenerate_installs_shield_from_oracle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lotleth = scenario
        .add_creature_from_oracle(P0, "Lotleth Troll", 2, 1, LOTLETH_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Black));

    let mut runner = scenario.build();
    let ability_index = regenerate_ability_index(&runner, lotleth);
    runner.activate(lotleth, ability_index).resolve();

    let obj = runner.state().objects.get(&lotleth).unwrap();
    let shield = obj
        .replacement_definitions
        .as_slice()
        .iter()
        .find(|r| r.shield_kind.is_shield())
        .expect("shield");
    assert!(matches!(
        shield.shield_kind,
        engine::types::ability::ShieldKind::Regeneration
    ));
    assert!(!shield.is_consumed);
}

#[test]
fn lotleth_regenerate_survives_lethal_damage_after_manual_mark() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let lotleth = scenario
        .add_creature_from_oracle(P0, "Lotleth Troll", 2, 1, LOTLETH_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Black));

    let mut runner = scenario.build();
    runner
        .activate(lotleth, regenerate_ability_index(&runner, lotleth))
        .resolve();

    runner
        .state_mut()
        .objects
        .get_mut(&lotleth)
        .unwrap()
        .damage_marked = 5;
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    let lotleth_obj = runner.state().objects.get(&lotleth).unwrap();
    assert_eq!(lotleth_obj.zone, Zone::Battlefield);
    assert_eq!(lotleth_obj.damage_marked, 0);
}
