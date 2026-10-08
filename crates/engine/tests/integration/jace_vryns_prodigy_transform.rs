//! Regression for Jace, Vryn's Prodigy: an interactive discard must finish
//! before the following five-cards-in-graveyard condition is checked.
//! CR 608.2c: The controller follows instructions in the order written.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn jace_transforms_after_interactive_fifth_graveyard_card() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario.add_real_card(P0, "Jace, Vryn's Prodigy", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    for _ in 0..4 {
        scenario.add_real_card(P0, "Island", Zone::Graveyard, db);
    }
    // Jace draws a second card, making the required discard an actual choice.
    scenario.add_real_card(P0, "Island", Zone::Hand, db);
    for _ in 0..5 {
        scenario.add_real_card(P1, "Island", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    runner
        .act(GameAction::ActivateAbility {
            source_id: jace,
            ability_index: 0,
        })
        .expect("Jace's loot ability should be activatable");
    runner.advance_until_stack_empty();

    let WaitingFor::DiscardChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "Jace must pause for the required discard, got {:?}",
            runner.state().waiting_for
        );
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("selecting Jace's discard should resume the resolution");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[0].graveyard.len(),
        5,
        "the selected card must reach Jace's controller's graveyard before the condition"
    );
    let object = &runner.state().objects[&jace];
    assert_eq!(object.zone, Zone::Battlefield);
    assert!(
        object.transformed,
        "Jace must return transformed at five cards"
    );
    assert_eq!(object.name, "Jace, Telepath Unbound");
    assert_eq!(object.loyalty, Some(5));
}

/// The threshold is checked after the selected discard moves, but it remains a
/// real "five or more" condition rather than an unconditional transform.
#[test]
fn jace_stays_front_face_after_interactive_fourth_graveyard_card() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario.add_real_card(P0, "Jace, Vryn's Prodigy", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Island", Zone::Library, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Island", Zone::Graveyard, db);
    }
    scenario.add_real_card(P0, "Island", Zone::Hand, db);
    for _ in 0..5 {
        scenario.add_real_card(P1, "Island", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner
        .act(GameAction::ActivateAbility {
            source_id: jace,
            ability_index: 0,
        })
        .expect("Jace's loot ability should be activatable");
    runner.advance_until_stack_empty();

    let WaitingFor::DiscardChoice { cards, .. } = runner.state().waiting_for.clone() else {
        panic!("Jace must pause for the required discard");
    };
    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("selecting Jace's discard should resume the resolution");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().players[0].graveyard.len(), 4);
    assert!(!runner.state().objects[&jace].transformed);
    assert_eq!(runner.state().objects[&jace].name, "Jace, Vryn's Prodigy");
}

/// Jace's condition uses the activator's graveyard, while its transformed
/// return explicitly enters under its owner's control.
#[test]
fn stolen_jace_uses_controllers_graveyard_and_returns_to_owner() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let jace = scenario.add_real_card(P0, "Jace, Vryn's Prodigy", Zone::Battlefield, db);
    scenario.add_real_card(P1, "Island", Zone::Library, db);
    for _ in 0..4 {
        scenario.add_real_card(P1, "Island", Zone::Graveyard, db);
    }
    scenario.add_real_card(P1, "Island", Zone::Hand, db);
    for _ in 0..5 {
        scenario.add_real_card(P0, "Island", Zone::Library, db);
    }

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    {
        let state = runner.state_mut();
        state.objects.get_mut(&jace).unwrap().controller = P1;
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
    }

    runner
        .act(GameAction::ActivateAbility {
            source_id: jace,
            ability_index: 0,
        })
        .expect("the current controller should be able to activate Jace");
    runner.advance_until_stack_empty();

    let WaitingFor::DiscardChoice { player, cards, .. } = runner.state().waiting_for.clone() else {
        panic!("the controller's discard must pause Jace's resolution");
    };
    assert_eq!(player, P1);
    runner
        .act(GameAction::SelectCards {
            cards: vec![cards[0]],
        })
        .expect("the controller's discard should resume the resolution");
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().players[1].graveyard.len(), 5);
    assert!(runner.state().objects[&jace].transformed);
    assert_eq!(runner.state().objects[&jace].controller, P0);
}
