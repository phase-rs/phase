//! Hawkeye, Avenging Archer — death-trigger intervening-if runtime gate.
//!
//! CR 603.4 + CR 700.4 + CR 120.1: "Whenever a creature an opponent controls
//! dies, if Hawkeye dealt damage to it this turn, draw a card." The controller
//! draws ONLY when Hawkeye dealt damage to the dying opponent creature this
//! turn. Before the parser arm existed the intervening-if was dropped
//! (`condition == None`), so the trigger drew unconditionally on any opponent
//! creature death — the audit-flagged DroppedCondition.
//!
//! These two tests share an identical death setup; the only difference is
//! whether a Hawkeye damage record exists this turn. The positive test is the
//! reach-guard proving the trigger fires and draws for this exact opponent-death
//! shape, which makes the negative test non-vacuous: it isolates the condition
//! gate. Reverting the parser fix makes the negative test draw a card and fail.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{DamageRecord, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const HAWKEYE_ORACLE: &str = "Reach\nWhenever a creature an opponent controls \
    dies, if Hawkeye dealt damage to it this turn, draw a card.\n{T}: Hawkeye \
    deals 1 damage to any target.";

fn drain_stack(runner: &mut GameRunner) {
    for _ in 0..200 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            continue;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
}

/// Kill `victim` via lethal marked damage + SBA, then process any triggers the
/// death produced and resolve the resulting stack.
fn kill_via_sba(runner: &mut GameRunner, victim: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&victim)
        .unwrap()
        .damage_marked = 1;
    let mut sba_events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut sba_events);
    engine::game::triggers::process_triggers(runner.state_mut(), &sba_events);
    drain_stack(runner);
}

#[test]
fn hawkeye_draws_when_it_damaged_the_dying_opponent_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw Fodder"]);
    let hawkeye = scenario
        .add_creature_from_oracle(P0, "Hawkeye, Avenging Archer", 3, 3, HAWKEYE_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Damaged Victim", 1, 1).id();

    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    // Hawkeye dealt 1 damage to the victim this turn (records the same
    // `DamageRecord` the deal-damage resolver would).
    runner
        .state_mut()
        .damage_dealt_this_turn
        .push_back(DamageRecord {
            source_id: hawkeye,
            source_controller: P0,
            target: TargetRef::Object(victim),
            target_controller: P1,
            amount: 1,
            is_combat: false,
            ..Default::default()
        });

    kill_via_sba(&mut runner, victim);

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "Hawkeye's controller must draw when Hawkeye dealt damage to the dying \
         opponent creature this turn"
    );
}

#[test]
fn hawkeye_does_not_draw_when_it_did_not_damage_the_dying_opponent_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw Fodder"]);
    scenario
        .add_creature_from_oracle(P0, "Hawkeye, Avenging Archer", 3, 3, HAWKEYE_ORACLE)
        .id();
    let victim = scenario.add_creature(P1, "Unharmed Victim", 1, 1).id();

    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    // No Hawkeye damage record this turn — the intervening-if (CR 603.4) must be
    // false, so the trigger never draws. With the fix reverted the condition is
    // dropped and this death draws a card unconditionally, failing the assert.
    kill_via_sba(&mut runner, victim);

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "Hawkeye's controller must NOT draw when Hawkeye never damaged the dying \
         opponent creature this turn"
    );
}
