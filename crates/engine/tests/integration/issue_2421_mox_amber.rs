//! Regression for issue #2421: Mox Amber must only offer colors among legendary
//! creatures and planeswalkers you control.
//!
//! https://github.com/phase-rs/phase/issues/2421

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::mana::ManaColor;
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const MOX_AMBER_ORACLE: &str =
    "{T}: Add one mana of any color among legendary creatures and planeswalkers you control.";

fn set_object_colors(
    runner: &mut engine::game::scenario::GameRunner,
    id: engine::types::identifiers::ObjectId,
    colors: &[ManaColor],
) {
    runner.state_mut().objects.get_mut(&id).unwrap().color = colors.to_vec();
}

#[test]
fn mox_amber_offers_only_legendary_creature_colors() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox_id = scenario
        .add_creature(P0, "Mox Amber", 0, 0)
        .as_artifact()
        .as_legendary()
        .from_oracle_text(MOX_AMBER_ORACLE)
        .id();
    let red_id = scenario
        .add_creature(P0, "Legendary Red", 2, 2)
        .as_legendary()
        .id();
    let blue_id = scenario
        .add_creature(P0, "Legendary Blue", 2, 2)
        .as_legendary()
        .id();

    let mut runner = scenario.build();
    set_object_colors(&mut runner, red_id, &[ManaColor::Red]);
    set_object_colors(&mut runner, blue_id, &[ManaColor::Blue]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: mox_id,
            ability_index: 0,
        })
        .expect("activate Mox Amber");

    match &runner.state().waiting_for {
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { options },
            ..
        } => {
            assert_eq!(
                options.len(),
                2,
                "must offer red and blue only; got {options:?}"
            );
            assert!(options.contains(&ManaType::Red));
            assert!(options.contains(&ManaType::Blue));
            assert!(!options.contains(&ManaType::Green));
            assert!(!options.contains(&ManaType::White));
        }
        other => panic!("expected ChooseManaColor prompt, got {other:?}"),
    }
}

#[test]
fn mox_amber_with_no_legendary_creatures_or_planeswalkers_produces_no_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox_id = scenario
        .add_creature(P0, "Mox Amber", 0, 0)
        .as_artifact()
        .as_legendary()
        .from_oracle_text(MOX_AMBER_ORACLE)
        .id();

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: mox_id,
            ability_index: 0,
        })
        .expect("activate Mox Amber with no eligible legendaries");

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::ChooseManaColor { .. }
        ),
        "no colored legendaries means no color choice; got {:?}",
        runner.state().waiting_for
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "CR 106.5: undefined color set produces no mana"
    );
}

#[test]
fn mox_amber_does_not_count_itself_as_a_color_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox_id = scenario
        .add_creature(P0, "Mox Amber", 0, 0)
        .as_artifact()
        .as_legendary()
        .from_oracle_text(MOX_AMBER_ORACLE)
        .id();
    let mut runner = scenario.build();
    set_object_colors(&mut runner, mox_id, &[ManaColor::Red]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: mox_id,
            ability_index: 0,
        })
        .expect("activate Mox Amber alone");

    assert_eq!(
        runner.state().players[P0.0 as usize].mana_pool.total(),
        0,
        "legendary artifacts are outside the creature/planeswalker filter"
    );
}

#[test]
fn mox_amber_chosen_color_produces_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mox_id = scenario
        .add_creature(P0, "Mox Amber", 0, 0)
        .as_artifact()
        .as_legendary()
        .from_oracle_text(MOX_AMBER_ORACLE)
        .id();
    let green_id = scenario
        .add_creature(P0, "Legendary Green", 2, 2)
        .as_legendary()
        .id();

    let mut runner = scenario.build();
    set_object_colors(&mut runner, green_id, &[ManaColor::Green]);
    runner
        .act(GameAction::ActivateAbility {
            source_id: mox_id,
            ability_index: 0,
        })
        .expect("activate Mox Amber");

    if let WaitingFor::ChooseManaColor { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            })
            .expect("choose green");
    }

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1
    );
}
