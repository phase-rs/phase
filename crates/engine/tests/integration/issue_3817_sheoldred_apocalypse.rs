//! Integration tests for GitHub issue #3817 — Sheoldred, the Apocalypse.
//!
//! Oracle text:
//!   Deathtouch
//!   Whenever you draw a card, you gain 2 life.
//!   Whenever an opponent draws a card, they lose 2 life.
//!
//! CR 121.1: A draw trigger fires only when a card is actually drawn.
//! CR 614.1: Replacement effects that replace the draw (e.g. Abundance) prevent
//!           the draw event, so Sheoldred does not trigger.
//! CR 603.2 + CR 603.2c: Two Sheoldreds are separate triggered abilities; each
//!           triggers once on the same draw event.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const SHEOLDRED: &str = "Sheoldred, the Apocalypse";
const SHEOLDRED_ORACLE: &str = "Deathtouch\n\
Whenever you draw a card, you gain 2 life.\n\
Whenever an opponent draws a card, they lose 2 life.";

const SCRY_ONLY_ORACLE: &str = "Scry 1.";

fn stock_libraries(scenario: &mut GameScenario) {
    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }
}

fn add_sheoldred_from_db(
    scenario: &mut GameScenario,
    db: &engine::database::card_db::CardDatabase,
) {
    scenario.add_real_card(P0, SHEOLDRED, Zone::Battlefield, db);
}

fn add_sheoldred_from_oracle(scenario: &mut GameScenario, name: &str) {
    scenario.add_creature_from_oracle(P0, name, 4, 5, SHEOLDRED_ORACLE);
}

fn advance_until_life_changes(
    runner: &mut engine::game::scenario::GameRunner,
    player: PlayerId,
    baseline: i32,
) {
    for _ in 0..400 {
        if runner.life(player) != baseline {
            return;
        }
        let acted = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            WaitingFor::OrderTriggers { .. } => runner.act(GameAction::PassPriority),
            _ => break,
        };
        if acted.is_err() {
            break;
        }
    }
}

fn resolve_stack(runner: &mut engine::game::scenario::GameRunner) {
    for _ in 0..200 {
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            WaitingFor::OrderTriggers { .. } => {
                let _ = runner.act(GameAction::PassPriority);
            }
            WaitingFor::ScryChoice { .. } => {
                let _ = runner.act(GameAction::SelectCards { cards: vec![] });
            }
            WaitingFor::NamedChoice { options, .. } => {
                let _ = runner.act(GameAction::ChooseOption {
                    choice: options[0].clone(),
                });
            }
            WaitingFor::ReplacementChoice { .. } => {
                let _ = runner.act(GameAction::ChooseReplacement { index: 1 });
            }
            _ => break,
        }
    }
}

/// Case 2 — own draw: controller gains 2 life when Sheoldred is in play.
#[test]
fn sheoldred_own_draw_gains_two_life() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    add_sheoldred_from_db(&mut scenario, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    let life_before = runner.life(P0);
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "Sheoldred must grant 2 life when its controller draws a card"
    );
}

/// Case 3 — opponent draw step: opponent loses 2 life.
#[test]
fn sheoldred_opponent_draw_step_loses_two_life() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    add_sheoldred_from_db(&mut scenario, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);

    let p1_before = runner.life(P1);
    advance_until_life_changes(&mut runner, P1, p1_before);

    assert_eq!(
        runner.life(P1),
        p1_before - 2,
        "Sheoldred must make the opponent who drew lose 2 life"
    );
}

/// Case 1 — scry-only spell: no draw, no Sheoldred trigger.
#[test]
fn sheoldred_scry_only_no_draw_trigger() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    add_sheoldred_from_oracle(&mut scenario, "Sheoldred A");
    let scry_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Scry Spell", true, SCRY_ONLY_ORACLE)
        .id();
    for name in ["Lib 1", "Lib 2", "Lib 3"] {
        scenario.add_card_to_library_top(P0, name);
    }
    let mut runner = scenario.build();
    let card_id = runner.state().objects.get(&scry_spell).unwrap().card_id;
    let library_before = runner.state().players[0].library.len();

    let p0_before = runner.life(P0);
    let p1_before = runner.life(P1);

    runner
        .act(GameAction::CastSpell {
            object_id: scry_spell,
            card_id,
            targets: vec![],
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("cast scry spell");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        p0_before,
        "scry-only must not trigger Sheoldred's gain-life ability"
    );
    assert_eq!(
        runner.life(P1),
        p1_before,
        "scry-only must not trigger Sheoldred's opponent-draw punishment"
    );
    assert_eq!(
        runner.state().players[0].library.len(),
        library_before,
        "scry-only must not draw a card from the library"
    );
}

/// Case 4 — Abundance replaces the draw: Sheoldred does not trigger.
#[test]
fn sheoldred_abundance_replacement_suppresses_draw_trigger() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    for name in ["Grizzly Bears", "Forest", "Plains", "Mountain"] {
        scenario.add_real_card(P0, name, Zone::Library, db);
    }
    for _ in 0..5 {
        scenario.add_real_card(P1, "Plains", Zone::Library, db);
    }
    add_sheoldred_from_db(&mut scenario, db);
    scenario.add_real_card(P0, "Abundance", Zone::Battlefield, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    let life_before = runner.life(P0);
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw");

    if matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("decline Abundance");
    }
    resolve_stack(&mut runner);

    let mut scenario2 = GameScenario::new();
    scenario2.at_phase(Phase::PreCombatMain);
    for name in ["Grizzly Bears", "Forest", "Plains", "Mountain"] {
        scenario2.add_real_card(P0, name, Zone::Library, db);
    }
    for _ in 0..5 {
        scenario2.add_real_card(P1, "Plains", Zone::Library, db);
    }
    add_sheoldred_from_db(&mut scenario2, db);
    scenario2.add_real_card(P0, "Abundance", Zone::Battlefield, db);
    let mut runner2 = scenario2.build();
    engine::game::rehydrate_game_from_card_db(runner2.state_mut(), db);
    runner2.state_mut().debug_mode = true;

    let life_before2 = runner2.life(P0);
    runner2
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw");

    if matches!(
        runner2.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ) {
        runner2
            .act(GameAction::ChooseReplacement { index: 0 })
            .expect("accept Abundance");
    }
    if matches!(runner2.state().waiting_for, WaitingFor::NamedChoice { .. }) {
        runner2
            .act(GameAction::ChooseOption {
                choice: "Land".to_string(),
            })
            .expect("choose Land");
    }
    resolve_stack(&mut runner2);

    assert_eq!(
        runner2.life(P0),
        life_before2,
        "Abundance replacement must prevent Sheoldred's draw trigger (no life gain)"
    );
    assert_eq!(
        runner.life(P0),
        life_before + 2,
        "declining Abundance must allow the draw and Sheoldred trigger"
    );
}

/// Case 5 — two Sheoldreds: each trigger grants 2 life (total +4).
#[test]
fn two_sheoldreds_each_gain_two_on_own_draw() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    add_sheoldred_from_oracle(&mut scenario, "Sheoldred A");
    add_sheoldred_from_oracle(&mut scenario, "Sheoldred B");
    let mut runner = scenario.build();
    runner.state_mut().debug_mode = true;

    assert_eq!(
        runner
            .state()
            .battlefield
            .iter()
            .filter(|id| runner
                .state()
                .objects
                .get(id)
                .is_some_and(|o| o.zone == Zone::Battlefield))
            .count(),
        2,
        "scenario must place two Sheoldred permanents"
    );

    let life_before = runner.life(P0);
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P0,
            count: 1,
        }))
        .expect("debug draw");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        life_before + 4,
        "two Sheoldreds must each trigger independently for +4 total (CR 603.2 + CR 603.2c)"
    );
}

/// Case 6 — opponent draws 3 cards, loses 6 life total.
#[test]
fn sheoldred_opponent_draws_three_loses_six() {
    let Some(db) = load_db() else {
        return;
    };

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    stock_libraries(&mut scenario);
    add_sheoldred_from_db(&mut scenario, db);
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    runner.state_mut().debug_mode = true;

    let p1_before = runner.life(P1);
    runner
        .act(GameAction::Debug(DebugAction::DrawCards {
            player_id: P1,
            count: 3,
        }))
        .expect("debug draw x3");
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_before - 6,
        "three opponent draws must trigger Sheoldred once per draw for -6 total"
    );
}
