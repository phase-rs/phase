//! Regression for issue #6219: Perrie, the Pulverizer's attack trigger.
//!
//! https://github.com/phase-rs/phase/issues/6219
//!
//! Oracle: "Whenever Perrie attacks, target creature you control gains trample
//! and gets +X/+X until end of turn, where X is the number of different kinds
//! of counters among permanents you control."
//!
//! The quantity parser lowered "the number of different kinds of counters
//! among <filter>" through the generic "the number of [counter type] counters
//! among <filter>" arm, treating "different kinds of" as a literal counter
//! type name instead of routing to the dedicated `DistinctCounterKindsAmong`
//! quantity (CR 122.1). No real permanent ever carries a counter literally
//! named "different kinds of", so X always resolved to 0 — the trample grant
//! landed but the +X/+X boost was silently a no-op.
//!
//! Discriminating observable: P0 controls Perrie (printed 3/3) carrying a
//! shield counter and a lore counter — two distinct counter kinds that don't
//! themselves modify power/toughness. After Perrie attacks and its trigger
//! auto-targets itself (the lone legal "creature you control"), X must
//! resolve to 2, so Perrie's effective power/toughness becomes 5/5 with
//! trample. Reverting the parser fix collapses X to 0, so the final assertion
//! flips to 3/3 with no trample.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;

use super::rules::AttackTarget;

const PERRIE_ORACLE: &str = "When Perrie enters, put a shield counter on target creature. (If it would be dealt damage or destroyed, remove a shield counter from it instead.)\nWhenever Perrie attacks, target creature you control gains trample and gets +X/+X until end of turn, where X is the number of different kinds of counters among permanents you control.";

/// Effective (post-layer) power/toughness of an object.
fn power_toughness(runner: &GameRunner, id: ObjectId) -> (i32, i32) {
    let obj = runner
        .state()
        .objects
        .get(&id)
        .expect("object still present");
    (obj.power.unwrap_or(0), obj.toughness.unwrap_or(0))
}

fn has_trample(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object still present")
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Trample))
}

#[test]
fn perrie_attack_trigger_counts_distinct_counter_kinds() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // P0 controls Perrie, printed 3/3 (NCC). `add_creature_from_oracle` places
    // the object directly on the battlefield without an "enters" event, so the
    // ETB shield-counter trigger does not fire here — only the attack trigger
    // under test does.
    let perrie = scenario
        .add_creature_from_oracle(P0, "Perrie, the Pulverizer", 3, 3, PERRIE_ORACLE)
        .id();

    // Seed two distinct counter kinds directly on Perrie: a shield counter and
    // a lore counter. CR 122.1: counters with different names/descriptions are
    // distinct kinds, so this makes X resolve to 2. Neither kind modifies P/T
    // on its own (unlike a +1/+1 counter), so the trigger's +X/+X is the only
    // source of the P/T change this test observes.
    scenario.with_counter(perrie, CounterType::Shield, 1);
    scenario.with_counter(perrie, CounterType::Lore, 1);

    let mut runner = scenario.build();

    // Sanity: printed 3/3 before combat, no trample.
    assert_eq!(power_toughness(&runner, perrie), (3, 3), "printed P/T");
    assert!(
        !has_trample(&runner, perrie),
        "no trample before the attack"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(perrie, AttackTarget::Player(P1))])
        .expect("declare Perrie as attacker");

    // Drive the trigger through the real trigger -> stack -> resolution
    // pipeline: Perrie is the only legal "creature you control" target, so
    // the engine auto-selects it without a TriggerTargetSelection prompt.
    // Pass priority (and order the trigger, if prompted) until the buff
    // lands or the loop exhausts.
    for _ in 0..300 {
        if power_toughness(&runner, perrie) == (5, 5) && has_trample(&runner, perrie) {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .or_else(|_| runner.act(GameAction::OrderTriggers { order: vec![] }))
                    .expect("order the single attack trigger");
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .expect("no blocks");
            }
            _ => break,
        }
    }

    // CR 122.1 + CR 702.19: X must count the two distinct counter kinds
    // (shield, lore) among permanents P0 controls, so Perrie's effective P/T
    // becomes 5/5 with trample for the turn.
    assert_eq!(
        power_toughness(&runner, perrie),
        (5, 5),
        "Perrie's attack trigger must add +2/+2 for the two distinct counter kinds it carries"
    );
    assert!(
        has_trample(&runner, perrie),
        "Perrie's attack trigger must grant trample to the targeted creature"
    );
}
