//! Regression for issue #1135: Ent's Fury must fight the boosted ally creature
//! against the chosen opponent creature.
//!
//! https://github.com/phase-rs/phase/issues/1135

use engine::game::game_object::PhaseOutCause;
use engine::game::phasing::phase_out_object;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const ENTS_FURY_ORACLE: &str = "Put a +1/+1 counter on target creature you control if its power is 4 or greater. Then that creature gets +1/+1 until end of turn and fights target creature you don't control.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn ents_fury_fights_boosted_creature_against_opponent_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ent's Fury", false, ENTS_FURY_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 5, 5).id();
    let wolf = scenario.add_creature(P1, "Wolf", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        floating_mana(1, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(1, ManaType::Green))
            .collect(),
    );

    let mut runner = scenario.build();

    runner
        .cast(spell)
        .target_objects(&[bear, wolf])
        .commit()
        .resolve();

    assert!(
        runner.state().objects[&wolf].damage_marked >= 6,
        "boosted bear must deal fight damage to wolf, got {}",
        runner.state().objects[&wolf].damage_marked
    );
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        2,
        "wolf must deal its power in fight damage back to bear"
    );
}

#[test]
fn ents_fury_fight_fizzles_when_opponent_phases_out_before_resolution() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Ent's Fury", false, ENTS_FURY_ORACLE)
        .id();
    let bear = scenario.add_creature(P0, "Bear", 5, 5).id();
    let wolf = scenario.add_creature(P1, "Wolf", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        floating_mana(1, ManaType::Colorless)
            .into_iter()
            .chain(floating_mana(1, ManaType::Green))
            .collect(),
    );

    let mut runner = scenario.build();
    {
        let _commit = runner.cast(spell).target_objects(&[bear, wolf]).commit();
    }

    let mut events = Vec::new();
    phase_out_object(
        runner.state_mut(),
        wolf,
        PhaseOutCause::Directly,
        &mut events,
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&wolf].damage_marked,
        0,
        "phased-out opponent must not take fight damage"
    );
    assert_eq!(
        runner.state().objects[&bear].damage_marked,
        0,
        "ally must not fight when explicit opponent target is illegal"
    );
}
