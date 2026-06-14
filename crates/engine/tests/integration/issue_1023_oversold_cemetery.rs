//! Issue #1023 — Oversold Cemetery must not trigger unless the controller has
//! at least four creature cards in their graveyard.

use engine::game::scenario::{GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use engine::types::CoreType;
use engine::types::PlayerId;

use crate::support::shared_card_db as load_db;

fn graveyard_creature_count(
    runner: &engine::game::scenario::GameRunner,
    player: PlayerId,
) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| {
            p.graveyard
                .iter()
                .filter(|id| {
                    runner
                        .state()
                        .objects
                        .get(id)
                        .is_some_and(|obj| obj.card_types.core_types.contains(&CoreType::Creature))
                })
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn issue_1023_oversold_cemetery_does_not_trigger_below_four_creature_cards() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.add_real_card(P0, "Oversold Cemetery", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Elvish Mystic", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Llanowar Elves", Zone::Graveyard, db);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(graveyard_creature_count(&runner, P0), 3);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.auto_advance_to_main_phase();
    runner.advance_until_stack_empty();

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::OptionalEffectChoice { .. }
        ),
        "Oversold Cemetery must not trigger with only three creature cards in graveyard, got {:?}",
        runner.state().waiting_for
    );
}

#[test]
fn issue_1023_oversold_cemetery_triggers_at_four_creature_cards() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.add_real_card(P0, "Oversold Cemetery", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Grizzly Bears", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Elvish Mystic", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Llanowar Elves", Zone::Graveyard, db);
    scenario.add_real_card(P0, "Centaur Courser", Zone::Graveyard, db);
    scenario.with_library_top(P0, &["Plains", "Plains", "Plains"]);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    assert_eq!(graveyard_creature_count(&runner, P0), 4);

    runner.state_mut().turn_number = 2;
    runner.state_mut().phase = Phase::Untap;
    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };

    runner.auto_advance_to_main_phase();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. } | WaitingFor::TriggerTargetSelection { .. }
        ),
        "Oversold Cemetery must trigger at four creature cards in graveyard, got {:?}",
        runner.state().waiting_for
    );
}
