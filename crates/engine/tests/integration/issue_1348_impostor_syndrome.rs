//! Issue #1348: Impostor Syndrome must copy the combat-damage-dealing creature,
//! not the enchantment itself.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::run_combat;

const IMPOSTOR_SYNDROME_ORACLE: &str =
    "Whenever a nontoken creature you control deals combat damage to a player, \
create a token that's a copy of it, except it isn't legendary.";

fn token_copies_named(runner: &GameRunner, name: &str) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|obj| {
            obj.zone == engine::types::zones::Zone::Battlefield && obj.name == name && obj.is_token
        })
        .count()
}

#[test]
fn impostor_syndrome_copies_combat_damage_creature_not_self() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature(P0, "Impostor Syndrome", 0, 0)
        .as_enchantment()
        .from_oracle_text(IMPOSTOR_SYNDROME_ORACLE);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let mut runner = scenario.build();
    let bears_before = token_copies_named(&runner, "Grizzly Bears");

    let trigger = &runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Impostor Syndrome")
        .unwrap()
        .trigger_definitions[0];
    assert_eq!(trigger.definition.mode, TriggerMode::DamageDone);
    match trigger.definition.execute.as_ref().unwrap().effect.as_ref() {
        engine::types::ability::Effect::CopyTokenOf { target, .. } => {
            assert_eq!(
                *target,
                engine::types::ability::TargetFilter::TriggeringSource,
                "copy target must bind to the damage-dealing creature (#1348)"
            );
        }
        other => panic!("expected CopyTokenOf, got {other:?}"),
    }

    run_combat(&mut runner, vec![bear], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(
        token_copies_named(&runner, "Grizzly Bears"),
        bears_before + 1,
        "combat damage should create a copy of Grizzly Bears"
    );
    assert!(
        token_copies_named(&runner, "Impostor Syndrome") == 0,
        "must not create a copy of Impostor Syndrome itself"
    );
}
