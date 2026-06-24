//! Issue #3277: Captain N'ghathrod's end-step reanimation must not prompt
//! after the milled opponent has left the game.
//!
//! When an opponent is eliminated (CR 800.4a), their zones leave the game before
//! "at the beginning of your end step" resolves. N'ghathrod must not offer a
//! target in that eliminated player's graveyard.
//!
//! https://github.com/phase-rs/phase/issues/3277

use engine::game::elimination::eliminate_player;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{WaitingFor, ZoneChangeRecord};
use engine::types::identifiers::CardId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const CAPTAIN_NGHATHROD_ORACLE: &str = "Horrors you control have menace.\n\
Whenever a Horror you control deals combat damage to a player, that player mills that many cards.\n\
At the beginning of your end step, choose target artifact or creature card in an opponent's graveyard that was put there from their library this turn. Put it onto the battlefield under your control.";

fn resolve_stack(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![] })
                    .expect("order triggers");
            }
            WaitingFor::TargetSelection { .. } => {
                panic!(
                    "unexpected target selection before end step: {:?}",
                    runner.state().waiting_for
                );
            }
            other => panic!("unexpected waiting state while resolving stack: {other:?}"),
        }
    }
    panic!("stack did not empty");
}

#[test]
fn captain_nghathrod_end_step_skips_eliminated_opponent_graveyard() {
    let mut scenario = GameScenario::new_n_player(3, 3277);
    scenario.at_phase(Phase::PreCombatMain);

    let _nghathrod = scenario
        .add_creature_from_oracle(P0, "Captain N'ghathrod", 3, 6, CAPTAIN_NGHATHROD_ORACLE)
        .with_subtypes(vec!["Horror", "Pirate"])
        .id();

    let mut runner = scenario.build();

    // Simulate the mill from N'ghathrod's combat-damage trigger: a creature card
    // milled from P1's library into their graveyard this turn.
    let milled_creature = create_object(
        runner.state_mut(),
        CardId(9001),
        P1,
        "Milled Bear".to_string(),
        Zone::Graveyard,
    );
    runner
        .state_mut()
        .objects
        .get_mut(&milled_creature)
        .unwrap()
        .card_types
        .core_types = vec![CoreType::Creature];
    runner
        .state_mut()
        .zone_changes_this_turn
        .push(ZoneChangeRecord {
            object_id: milled_creature,
            name: "Milled Bear".to_string(),
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            keywords: vec![],
            trigger_definitions: vec![],
            power: Some(2),
            toughness: Some(2),
            base_power: Some(2),
            base_toughness: Some(2),
            colors: vec![],
            mana_value: 2,
            controller: P1,
            owner: P1,
            from_zone: Some(Zone::Library),
            cast_from_zone: None,
            played_from_zone: None,
            to_zone: Zone::Graveyard,
            attachments: vec![],
            linked_exile_snapshot: vec![],
            is_token: false,
            combat_status: Default::default(),
            co_departed: Vec::new(),
            turn_zone_change_index: 0,
        });

    let mut events = Vec::new();
    eliminate_player(runner.state_mut(), P1, &mut events);

    assert!(
        runner.state().players[1].is_eliminated,
        "P1 must be eliminated"
    );
    assert!(
        runner.state().players[1].graveyard.is_empty(),
        "eliminated player's graveyard must leave the game (CR 800.4a)"
    );
    assert!(
        !runner.state().players[2].is_eliminated,
        "P2 must still be in the game"
    );

    // CR 513.1a: Fire end-step triggers without auto-advancing through combat.
    runner.state_mut().phase = Phase::End;
    engine::game::triggers::process_triggers(
        runner.state_mut(),
        &[engine::types::events::GameEvent::PhaseChanged { phase: Phase::End }],
    );
    resolve_stack(&mut runner);

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::TargetSelection { .. }
        ),
        "N'ghathrod must not prompt for a target in an eliminated opponent's graveyard, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "multiplayer game must continue after one opponent is eliminated"
    );
}
