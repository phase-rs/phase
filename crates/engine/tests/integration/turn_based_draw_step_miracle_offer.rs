//! Turn-based draw-step coverage for the shared draw-sequence authority.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

fn advance_turn_action(runner: &mut GameRunner) {
    let action = match &runner.state().waiting_for {
        WaitingFor::Priority { .. } | WaitingFor::OrderTriggers { .. } => GameAction::PassPriority,
        WaitingFor::DeclareAttackers { .. } => GameAction::DeclareAttackers {
            attacks: vec![],
            bands: vec![],
        },
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        other => panic!("unexpected turn-action prompt: {other:?}"),
    };
    runner.act(action).expect("advance turn action");
}

fn advance_until_precombat_main(runner: &mut GameRunner) {
    for _ in 0..400 {
        if runner.state().phase == Phase::PreCombatMain
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            return;
        }
        advance_turn_action(runner);
    }
    panic!("did not reach ordinary priority in precombat main within 400 actions");
}

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn library_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|entry| entry.id == player)
        .expect("player exists")
        .library
        .len()
}

fn object_zone(runner: &GameRunner, object_id: engine::types::ObjectId) -> Zone {
    runner.state().objects[&object_id].zone
}

/// CR 702.94a + CR 121.1: The mandatory draw-step draw can be the first card
/// drawn this turn and therefore offers Miracle.
#[test]
fn draw_step_first_draw_of_turn_offers_miracle() {
    let mut scenario = GameScenario::new();
    // `at_phase` sets turn 2, avoiding CR 103.8a's skipped initial draw step.
    scenario.at_phase(Phase::Upkeep);
    let miracle_card = scenario.add_card_to_library_top(P0, "Draw Step Miracle");
    let mut runner = scenario.build();
    let miracle = Keyword::Miracle(ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 0,
    });
    let obj = runner.state_mut().objects.get_mut(&miracle_card).unwrap();
    obj.keywords.push(miracle);
    obj.base_keywords = obj.keywords.clone();

    for _ in 0..400 {
        match &runner.state().waiting_for {
            WaitingFor::MiracleReveal {
                player, object_id, ..
            } => {
                assert_eq!(*player, P0, "the active player must receive the offer");
                assert_eq!(
                    *object_id, miracle_card,
                    "the draw-step offer must identify the card just drawn"
                );
                return;
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::PreCombatMain => {
                panic!("draw step finished without offering Miracle for its first draw");
            }
            _ => advance_turn_action(&mut runner),
        }
    }

    panic!("did not reach a MiracleReveal prompt within 400 actions");
}

/// CR 702.94a: A Miracle card drawn second in the turn is not eligible, even
/// when the second draw is the mandatory draw-step draw.
#[test]
fn draw_step_second_draw_of_turn_does_not_offer_miracle() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let miracle_card = scenario.add_card_to_library_top(P0, "Second Draw Miracle");
    scenario.add_card_to_library_top(P0, "First Draw Ordinary Card");
    let mut runner = scenario.build();
    let miracle = Keyword::Miracle(ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 0,
    });
    let obj = runner.state_mut().objects.get_mut(&miracle_card).unwrap();
    obj.keywords.push(miracle);
    obj.base_keywords = obj.keywords.clone();

    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("first draw this turn");
    if matches!(runner.state().waiting_for, WaitingFor::MiracleReveal { .. }) {
        runner
            .act(GameAction::DecideOptionalEffect { accept: false })
            .expect("decline unexpected first-draw miracle offer");
    }

    for _ in 0..400 {
        match &runner.state().waiting_for {
            WaitingFor::MiracleReveal { object_id, .. } if *object_id == miracle_card => {
                panic!("second draw of the turn must not offer Miracle");
            }
            WaitingFor::MiracleReveal { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline unrelated miracle offer");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::PreCombatMain => {
                return;
            }
            _ => advance_turn_action(&mut runner),
        }
    }

    panic!("did not finish the draw step within 400 actions");
}

/// CR 121.6b: The draw-step draw pauses for Abundance's optional replacement
/// and, after declining, resumes to deliver exactly one card.
#[test]
fn draw_step_pauses_on_optional_replacement_and_resumes() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Upkeep);
    let drawn_card = scenario.add_real_card(P0, "Grizzly Bears", Zone::Library, db);
    scenario.add_real_card(P0, "Plains", Zone::Library, db);
    scenario.add_real_card(P0, "Abundance", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let hand_before = hand_count(&runner, P0);
    let library_before = library_count(&runner, P0);
    for _ in 0..400 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        advance_turn_action(&mut runner);
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Abundance must pause the turn-based draw at a replacement choice"
    );

    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline Abundance");
    advance_until_precombat_main(&mut runner);

    assert_eq!(hand_count(&runner, P0), hand_before + 1);
    assert_eq!(library_count(&runner, P0), library_before - 1);
    assert_eq!(object_zone(&runner, drawn_card), Zone::Hand);
}
