//! Hawkeye, Avenging Archer — death-trigger intervening-if runtime gate.
//!
//! CR 603.4 + CR 700.4 + CR 120.1: "Whenever a creature an opponent controls
//! dies, if Hawkeye dealt damage to it this turn, draw a card." The controller
//! draws ONLY when Hawkeye dealt damage to the dying opponent creature this
//! turn. Before the parser arm existed the intervening-if was dropped
//! (`condition == None`), so the trigger drew unconditionally on any opponent
//! creature death — the audit-flagged DroppedCondition.
//!
//! The positive test drives Hawkeye's REAL `{T}` activated ability through the
//! production pipeline (`runner.activate(..).target_object(..).resolve()`): the
//! deal-damage resolver records the `DamageRecord` (source id + incarnation), a
//! state-based action kills the 1/1 target, and the dies trigger's intervening-if
//! is evaluated against that genuine record. Nothing is hand-injected, so the
//! test exercises the exact failure path the fix prevents — a revert of the
//! parser arm makes the trigger draw unconditionally and the negative test fail.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;

const HAWKEYE_ORACLE: &str = "Reach\nWhenever a creature an opponent controls \
    dies, if Hawkeye dealt damage to it this turn, draw a card.\n{T}: Hawkeye \
    deals 1 damage to any target.";

/// Index of Hawkeye's `{T}: deals 1 damage to any target` activated ability.
fn tap_damage_index(runner: &GameRunner, hawkeye: ObjectId) -> usize {
    runner.state().objects[&hawkeye]
        .abilities
        .iter()
        .position(|a| matches!(a.effect.as_ref(), Effect::DealDamage { .. }))
        .expect("Hawkeye must carry a DealDamage ({T}) activated ability")
}

/// Give `player` priority in their own pre-combat main so an activated ability
/// can be declared (mirrors the setup other activated-ability integration tests
/// use after `scenario.build()`).
fn hand_priority(runner: &mut GameRunner, player: engine::types::player::PlayerId) {
    runner.state_mut().active_player = player;
    runner.state_mut().priority_player = player;
    runner.state_mut().waiting_for = WaitingFor::Priority { player };
}

#[test]
fn hawkeye_draws_when_it_damaged_the_dying_opponent_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Draw Fodder"]);
    let hawkeye = scenario
        .add_creature_from_oracle(P0, "Hawkeye, Avenging Archer", 3, 3, HAWKEYE_ORACLE)
        .id();
    // A 1/1 so Hawkeye's own 1 damage is lethal — the same activation that
    // records the damage also causes the death, driving the whole chain
    // (deal damage → SBA death → dies trigger → intervening-if → draw).
    let victim = scenario.add_creature(P1, "Damaged Victim", 1, 1).id();

    let mut runner = scenario.build();
    hand_priority(&mut runner, P0);
    let idx = tap_damage_index(&runner, hawkeye);

    // CR 602.2 + CR 120.1: activate Hawkeye's {T}, target the opponent 1/1, and
    // let the production pipeline deal the damage, run SBA, and resolve the
    // resulting dies trigger. The deal-damage resolver writes the authoritative
    // `DamageRecord`; the intervening-if reads it — no hand-injected record.
    let outcome = runner
        .activate(hawkeye, idx)
        .target_object(victim)
        .resolve();

    // CR 603.4: the intervening-if is true (Hawkeye dealt damage to the dying
    // creature this turn), so exactly one card is drawn on resolution.
    outcome.assert_hand_drawn(P0, 1);
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

    // The victim dies WITHOUT Hawkeye ever dealing it damage — a
    // Hawkeye-independent death (lethal marked damage + SBA), so there is
    // deliberately no `DamageRecord` for the intervening-if to find. The absence
    // of the record — not the death mechanism — is what the assertion isolates.
    // CR 603.4: the intervening-if must be false, so the trigger never draws.
    // With the parser fix reverted the condition is dropped and this death draws
    // unconditionally, failing the assert.
    kill_untouched_victim(&mut runner, victim);

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before,
        "Hawkeye's controller must NOT draw when Hawkeye never damaged the dying \
         opponent creature this turn"
    );
}

/// Kill `victim` via lethal marked damage + SBA — a death Hawkeye had no part
/// in — then process the death's triggers and drain the resulting stack. Used
/// only by the negative test, where the point is that NO Hawkeye damage record
/// exists; the death itself is intentionally Hawkeye-independent.
fn kill_untouched_victim(runner: &mut GameRunner, victim: ObjectId) {
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
