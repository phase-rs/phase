//! Regression for issue #565: Birthing Ritual's end-step trigger must fire when
//! its intervening-if ("if you control a creature") is satisfied.
//!
//! Discord report was vague ("Ritual trigger should 1"); this test pins the
//! expected behavior: with Birthing Ritual on the battlefield and another
//! creature controlled, the beginning-of-end-step trigger enters the stack
//! exactly once.
//!
//! https://github.com/phase-rs/phase/issues/565

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::phase::Phase;

const BIRTHING_RITUAL_ORACLE: &str = "At the beginning of your end step, if you control a creature, look at the top seven cards of your library. Then you may sacrifice a creature. If you do, you may put a creature card with mana value X or less from among those cards onto the battlefield, where X is 1 plus the sacrificed creature's mana value. Put the rest on the bottom of your library in a random order.";

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
                    .ok();
            }
            _ if runner.state().phase == Phase::End => return,
            _ => runner.pass_both_players(),
        }
    }
}

#[test]
fn birthing_ritual_end_step_trigger_puts_ability_on_stack_once() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ritual = scenario
        .add_creature(P0, "Birthing Ritual", 0, 0)
        .as_enchantment()
        .from_oracle_text(BIRTHING_RITUAL_ORACLE)
        .id();
    let _other_creature = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    for i in 0..7 {
        scenario.add_card_to_library_top(P0, &format!("Library Filler {i}"));
    }

    let mut runner = scenario.build();
    reach_active_players_end_step(&mut runner);

    let trigger_stack_entries = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ritual
            )
        })
        .count();

    assert_eq!(
        runner.state().phase,
        Phase::End,
        "scenario should reach the active player's end step"
    );
    assert_eq!(
        trigger_stack_entries, 1,
        "Birthing Ritual must trigger exactly once at beginning of end step; stack = {:?}, waiting_for = {:?}",
        runner.state().stack,
        runner.state().waiting_for
    );
}

#[test]
fn birthing_ritual_end_step_trigger_skipped_without_other_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let ritual = scenario
        .add_creature(P0, "Birthing Ritual", 0, 0)
        .as_enchantment()
        .from_oracle_text(BIRTHING_RITUAL_ORACLE)
        .id();

    let mut runner = scenario.build();
    reach_active_players_end_step(&mut runner);

    // Observe at the end-step priority window WITHOUT draining the stack —
    // after `advance_until_stack_empty()` the stack is empty whether or not
    // the trigger ever fired, which made the original form of this assertion
    // vacuous (it passed even with the intervening-if satisfied).
    let trigger_stack_entries = runner
        .state()
        .stack
        .iter()
        .filter(|entry| {
            matches!(
                &entry.kind,
                StackEntryKind::TriggeredAbility { source_id, .. } if *source_id == ritual
            )
        })
        .count();

    assert_eq!(
        runner.state().phase,
        Phase::End,
        "scenario should reach the active player's end step"
    );
    assert_eq!(
        trigger_stack_entries, 0,
        "without another creature, Birthing Ritual's intervening-if (CR 603.4) must suppress the trigger; stack = {:?}",
        runner.state().stack
    );
}
