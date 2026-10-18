//! Akroan Horse
//!
//! Oracle Text:
//! Defender
//! When this creature enters, an opponent gains control of it.
//! At the beginning of your upkeep, each opponent creates a 1/1 white Soldier creature token.

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const AKROAN_HORSE: &str = "Defender\nWhen this creature enters, an opponent gains control of it.\nAt the beginning of your upkeep, each opponent creates a 1/1 white Soldier creature token.";

#[test]
fn akroan_horse_etb_transfers_control_to_opponent() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario
        .add_creature_to_hand_from_oracle(P0, "Akroan Horse", 0, 4, AKROAN_HORSE)
        .with_subtypes(vec!["Horse"])
        .id();

    let mut runner = scenario.build();

    let outcome = runner.cast(horse).choose_option("1").resolve();
    outcome.assert_zone(&[horse], Zone::Battlefield);

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&horse].controller,
        P1,
        "Akroan Horse must be controlled by the opponent (P1) after ETB trigger resolves"
    );
}

#[test]
fn akroan_horse_upkeep_creates_soldier_tokens_for_controller_opponents() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Akroan Horse is on the battlefield under P1's control.
    let _horse = scenario
        .add_creature_from_oracle(P1, "Akroan Horse", 0, 4, AKROAN_HORSE)
        .with_subtypes(vec!["Horse"])
        .id();

    let mut runner = scenario.build();

    // Advance past P0's turn to P1's Upkeep
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "should now be P1's turn (their upkeep step)"
    );

    // Pass priority to resolve the upkeep trigger on the stack
    runner.pass_both_players();

    // P0 (opponent of P1) should now have a 1/1 white Soldier creature token
    let soldiers_p0 = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.controller == P0
                && obj.card_types.subtypes.iter().any(|st| st == "Soldier")
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count();

    assert_eq!(
        soldiers_p0, 1,
        "P0 (opponent of Akroan Horse controller P1) must receive a 1/1 Soldier token on P1's upkeep"
    );

    let soldiers_p1 = runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|obj| {
            obj.controller == P1
                && obj.card_types.subtypes.iter().any(|st| st == "Soldier")
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .count();

    assert_eq!(
        soldiers_p1, 0,
        "P1 (Akroan Horse controller) must not receive a Soldier token from their own Horse"
    );
}

#[test]
fn akroan_horse_etb_three_player_choice_transfers_control_to_selected_opponent() {
    // 1. Choose P2 in 3-player game
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let horse_p2 = scenario
        .add_creature_to_hand_from_oracle(P0, "Akroan Horse", 0, 4, AKROAN_HORSE)
        .with_subtypes(vec!["Horse"])
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(horse_p2).choose_option("2").resolve();
    outcome.assert_zone(&[horse_p2], Zone::Battlefield);

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&horse_p2].controller,
        P2,
        "Akroan Horse must be controlled by chosen opponent (P2) after ETB trigger resolves"
    );

    // 2. Choose P1 in 3-player game
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let horse_p1 = scenario
        .add_creature_to_hand_from_oracle(P0, "Akroan Horse", 0, 4, AKROAN_HORSE)
        .with_subtypes(vec!["Horse"])
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(horse_p1).choose_option("1").resolve();
    outcome.assert_zone(&[horse_p1], Zone::Battlefield);

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&horse_p1].controller,
        P1,
        "Akroan Horse must be controlled by chosen opponent (P1) after ETB trigger resolves"
    );
}

#[test]
fn akroan_horse_etb_three_player_choice_prompts_all_opponents() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let horse = scenario
        .add_creature_to_hand_from_oracle(P0, "Akroan Horse", 0, 4, AKROAN_HORSE)
        .with_subtypes(vec!["Horse"])
        .id();

    let mut runner = scenario.build();
    // Cast without declaring a choice so resolution halts at NamedChoice prompt
    runner.cast(horse).resolve();

    match &runner.state().waiting_for {
        WaitingFor::NamedChoice {
            choice_type,
            options,
            player,
            ..
        } => {
            assert_eq!(
                *player, P0,
                "active player / controller must make the choice"
            );
            assert_eq!(*choice_type, ChoiceType::opponent());
            assert_eq!(
                options,
                &["1", "2"],
                "options must contain all legal opponents (P1 and P2) and exclude controller P0"
            );
        }
        other => panic!("expected NamedChoice for opponent, got {other:?}"),
    }

    // Explicitly choose P2 via GameAction::ChooseOption
    runner
        .act(GameAction::ChooseOption {
            choice: "2".to_string(),
        })
        .expect("ChooseOption(2) must succeed");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&horse].controller,
        P2,
        "Akroan Horse must be controlled by P2 after choosing option '2'"
    );
}
