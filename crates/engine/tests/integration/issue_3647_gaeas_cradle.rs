//! Regression for GitHub issue #3647 — Gaea's Cradle mana scales with creatures.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;

const GAEAS_CRADLE_ORACLE: &str = "{T}: Add {G} for each creature you control.";

#[test]
fn gaeas_cradle_adds_green_for_each_creature_you_control() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let cradle = scenario
        .add_creature(P0, "Gaea's Cradle", 0, 0)
        .as_artifact()
        .from_oracle_text(GAEAS_CRADLE_ORACLE)
        .id();
    let bear1 = scenario.add_creature(P0, "Bear 1", 2, 2).id();
    let bear2 = scenario.add_creature(P0, "Bear 2", 2, 2).id();
    let _ = (bear1, bear2);

    let mut runner = scenario.build();
    runner
        .act(GameAction::ActivateAbility {
            source_id: cradle,
            ability_index: 0,
        })
        .expect("activate Gaea's Cradle");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseManaColor {
            choice: ManaChoicePrompt::SingleColor { .. },
            ..
        }
    ) {
        runner
            .act(GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Green),
                count: 1,
            })
            .expect("choose green");
    }

    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Green),
        2,
        "two creatures should produce two green mana (cradle itself is not a creature)"
    );
    assert!(runner.state().objects[&cradle].tapped);
}
