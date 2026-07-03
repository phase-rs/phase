//! Regression for issue #3882: Ixhel, Scion of Atraxa's corrupted end-step
//! trigger must exile from opponents with three or more poison counters.
//!
//! https://github.com/phase-rs/phase/issues/3882

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const IXHEL_ORACLE: &str = "Flying, vigilance, toxic 2\n\
Corrupted — At the beginning of your end step, each opponent who has three or more poison counters exiles the top card of their library face down. \
You may look at and play those cards for as long as they remain exiled, and you may spend mana as though it were mana of any color to cast those spells.";

fn reach_active_players_end_step(runner: &mut engine::game::scenario::GameRunner) {
    runner.advance_to_end_step();
    for _ in 0..32 {
        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .expect("empty attack declaration should succeed");
            }
            WaitingFor::Priority { .. } if runner.state().phase == Phase::End => return,
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .expect("ordering triggers should succeed");
            }
            _ if runner.state().phase == Phase::End => return,
            _ => runner.pass_both_players(),
        }
    }
}

#[test]
fn ixhel_exiles_from_opponent_with_three_poison_at_end_step() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ixhel = scenario
        .add_creature_from_oracle(P0, "Ixhel, Scion of Atraxa", 4, 4, IXHEL_ORACLE)
        .id();
    scenario.add_card_to_library_top(P1, "Opponent Top Card");

    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].poison_counters = 3;

    let library_before = runner.state().players[P1.0 as usize].library.len();
    reach_active_players_end_step(&mut runner);

    let trigger_count = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ixhel
            )
        })
        .count();
    assert_eq!(
        trigger_count, 1,
        "Ixhel must trigger at beginning of controller's end step when an opponent is corrupted; stack = {:?}",
        runner.state().stack
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].library.len(),
        library_before - 1,
        "corrupted opponent's top library card must be exiled"
    );
    assert_eq!(
        runner.state().exile.len(),
        1,
        "exactly one card should be exiled face down"
    );
    let exiled = runner.state().exile[0];
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Exile,
        "exiled card must leave the library"
    );
    assert!(
        runner.state().objects[&exiled].face_down,
        "Ixhel's exile must be face down"
    );
}

#[test]
fn ixhel_skips_opponent_below_poison_threshold() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ixhel = scenario
        .add_creature_from_oracle(P0, "Ixhel, Scion of Atraxa", 4, 4, IXHEL_ORACLE)
        .id();
    scenario.add_card_to_library_top(P1, "Opponent Top Card");

    let mut runner = scenario.build();
    runner.state_mut().players[P1.0 as usize].poison_counters = 2;

    let library_before = runner.state().players[P1.0 as usize].library.len();
    reach_active_players_end_step(&mut runner);

    let trigger_count = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ixhel
            )
        })
        .count();
    assert_eq!(
        trigger_count, 1,
        "Ixhel's end-step trigger still fires; per-opponent poison filter applies during resolution"
    );

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().players[P1.0 as usize].library.len(),
        library_before,
        "opponent with fewer than three poison counters must not lose a library card"
    );
    assert!(
        runner.state().exile.is_empty(),
        "no cards should be exiled when no opponent meets the corrupted threshold"
    );
}
