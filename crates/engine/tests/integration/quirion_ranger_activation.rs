//! Regression (issue #1517): Quirion Ranger must be activatable when the player
//! controls a Forest and a legal untap target exists.

use engine::ai_support::legal_actions_full;
use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const QUIRION_RANGER: &str =
    "Return a Forest you control to its owner's hand: Untap target creature. Activate only once each turn.";

#[test]
fn quirion_ranger_activatable_with_forest_and_other_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ranger_id = scenario
        .add_creature(P0, "Quirion Ranger", 1, 1)
        .from_oracle_text(QUIRION_RANGER)
        .id();
    let _elf_id = scenario.add_creature(P0, "Llanowar Elves", 1, 1).id();
    let _forest_id = scenario.add_basic_land(P0, ManaColor::Green);

    let runner = scenario.build();
    let (_, _, grouped) = legal_actions_full(runner.state());

    let ranger_actions = grouped.get(&ranger_id).map(Vec::as_slice).unwrap_or(&[]);
    assert!(
        ranger_actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            } if *source_id == ranger_id
        )),
        "expected ActivateAbility on Quirion Ranger when Forest + creature exist; got {ranger_actions:?}",
    );
}

#[test]
fn quirion_ranger_activatable_with_forest_when_only_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ranger_id = scenario
        .add_creature(P0, "Quirion Ranger", 1, 1)
        .from_oracle_text(QUIRION_RANGER)
        .id();
    let _forest_id = scenario.add_basic_land(P0, ManaColor::Green);

    let runner = scenario.build();
    let (_, _, grouped) = legal_actions_full(runner.state());

    let ranger_actions = grouped.get(&ranger_id).map(Vec::as_slice).unwrap_or(&[]);
    assert!(
        ranger_actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility { source_id, .. } if *source_id == ranger_id
        )),
        "Ranger may untap itself as the only creature; got {ranger_actions:?}",
    );
}

#[test]
fn quirion_ranger_not_activatable_without_forest() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ranger_id = scenario
        .add_creature(P0, "Quirion Ranger", 1, 1)
        .from_oracle_text(QUIRION_RANGER)
        .id();
    scenario.add_creature(P0, "Llanowar Elves", 1, 1);

    let runner = scenario.build();
    let (_, _, grouped) = legal_actions_full(runner.state());

    let ranger_actions = grouped.get(&ranger_id);
    assert!(
        ranger_actions.is_none()
            || !ranger_actions.unwrap().iter().any(|a| matches!(
                a,
                GameAction::ActivateAbility { source_id, .. } if *source_id == ranger_id
            )),
        "without a Forest the ability must not be offered",
    );
}

#[test]
fn quirion_ranger_from_card_db_with_forest() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ranger_id = scenario.add_real_card(P0, "Quirion Ranger", Zone::Battlefield, db);
    let _elf_id = scenario.add_real_card(P0, "Llanowar Elves", Zone::Battlefield, db);
    let _forest_id = scenario.add_real_card(P0, "Forest", Zone::Battlefield, db);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let (_, _, grouped) = legal_actions_full(runner.state());
    let ranger_actions = grouped.get(&ranger_id).map(Vec::as_slice).unwrap_or(&[]);
    assert!(
        ranger_actions.iter().any(|a| matches!(
            a,
            GameAction::ActivateAbility { source_id, .. } if *source_id == ranger_id
        )),
        "real card-data Quirion Ranger must be activatable with a Forest on board; got {ranger_actions:?}",
    );

    let forest = runner.state().objects.get(&_forest_id).expect("forest");
    assert!(
        forest
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Forest")),
        "Forest card must carry the Forest land subtype for Quirion Ranger's cost",
    );
}

#[test]
fn quirion_ranger_not_activatable_with_non_forest_land() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ranger_id = scenario
        .add_creature(P0, "Quirion Ranger", 1, 1)
        .from_oracle_text(QUIRION_RANGER)
        .id();
    scenario.add_creature(P0, "Llanowar Elves", 1, 1);
    // Basic Island has the Island subtype only — cannot pay "return a Forest".
    let _island = scenario.add_basic_land(P0, ManaColor::Blue);

    let runner = scenario.build();
    let (_, _, grouped) = legal_actions_full(runner.state());

    let ranger_actions = grouped.get(&ranger_id);
    assert!(
        ranger_actions.is_none()
            || !ranger_actions.unwrap().iter().any(|a| matches!(
                a,
                GameAction::ActivateAbility { source_id, .. } if *source_id == ranger_id
            )),
        "non-Forest lands must not satisfy Quirion Ranger's cost",
    );
}
