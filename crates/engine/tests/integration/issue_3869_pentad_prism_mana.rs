//! Regression for issue #3869: Pentad Prism must be able to remove a charge
//! counter to add mana.
//!
//! https://github.com/phase-rs/phase/issues/3869

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{ManaChoice, ManaChoicePrompt, WaitingFor};
use engine::types::mana::ManaType;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

#[test]
fn pentad_prism_remove_charge_counter_adds_mana() {
    let Some(db) = shared_card_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let prism = scenario.add_real_card(P0, "Pentad Prism", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    {
        let obj = runner.state_mut().objects.get_mut(&prism).unwrap();
        obj.counters
            .insert(CounterType::Generic("charge".to_string()), 2);
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: prism,
            ability_index: 0,
        })
        .expect("activate Pentad Prism mana ability");

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
            .expect("choose green mana");
    }

    assert_eq!(
        runner.state().players[P0.0 as usize]
            .mana_pool
            .count_color(ManaType::Green),
        1,
        "activating Pentad Prism must add one mana of the chosen color"
    );
    assert_eq!(
        runner
            .state()
            .objects
            .get(&prism)
            .and_then(|o| o.counters.get(&CounterType::Generic("charge".to_string())))
            .copied()
            .unwrap_or(0),
        1,
        "activating Pentad Prism must remove one charge counter"
    );
}
