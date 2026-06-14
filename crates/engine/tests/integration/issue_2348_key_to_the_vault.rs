//! Regression for issue #2348: The Key to the Vault must trigger when its
//! equipped creature deals combat damage to a player.
//!
//! https://github.com/phase-rs/phase/issues/2348

use engine::game::effects::attach::attach_to;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::TargetFilter;
use engine::types::phase::Phase;
use engine::types::triggers::TriggerMode;

use super::rules::run_combat;

const KEY_ORACLE: &str = "Whenever equipped creature deals combat damage to a player, look at the \
top X cards of your library, where X is the amount of damage dealt. Exile a nonland card from \
among them. You may cast that card without paying its mana cost. Put the rest on the bottom of \
your library in a random order.\nEquip {3}{U}";

#[test]
fn key_to_the_vault_parses_equipped_creature_combat_damage_trigger() {
    let parsed = parse_oracle_text(
        KEY_ORACLE,
        "The Key to the Vault",
        &["Legendary".to_string()],
        &["Artifact".to_string()],
        &["Equipment".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::DamageDone)
        .expect("Key to the Vault must parse equipped combat-damage trigger");
    assert_eq!(trigger.valid_source, Some(TargetFilter::AttachedTo));
    assert_eq!(trigger.valid_target, Some(TargetFilter::Player));
}

#[test]
fn key_to_the_vault_triggers_on_equipped_creature_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let attacker = scenario.add_creature(P0, "Vault Bearer", 3, 3).id();
    let key = scenario
        .add_creature(P0, "The Key to the Vault", 0, 0)
        .as_artifact()
        .with_subtypes(vec!["Equipment"])
        .from_oracle_text(KEY_ORACLE)
        .id();

    for idx in 0..5 {
        scenario.add_spell_to_library_top(P0, &format!("Library Spell {idx}"), true);
    }

    let mut runner = scenario.build();
    attach_to(runner.state_mut(), key, attacker);
    evaluate_layers(runner.state_mut());

    let life_before = runner.life(P1);
    run_combat(&mut runner, vec![attacker], vec![]);

    assert_eq!(
        runner.life(P1),
        life_before - 3,
        "equipped attacker should deal 3 combat damage"
    );
    assert!(
        !runner.state().stack.is_empty(),
        "Key to the Vault trigger must go on the stack after combat damage, stack={:?}",
        runner.state().stack
    );
}
