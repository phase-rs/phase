//! Love on the Battlefield — batched-attack delayed combat-damage rider.
//!
//! Verified Oracle text (`client/public/card-data.json`,
//! `jq '.["love on the battlefield"].oracle_text'`):
//!   "Whenever you attack with exactly two creatures, those creatures gain first
//!    strike until end of turn, then draw a card. Whenever either of those
//!    creatures deals combat damage to a player this combat, put a +1/+1 counter
//!    on it."
//!
//! Exercises the full mechanic:
//!   - Gap B: the "attack with exactly two creatures" count constraint
//!     (`AttackersDeclaredCount { comparator: EQ, count: 2 }`).
//!   - Gap A: the second sentence folds into a delayed `WheneverEvent` whose
//!     source anaphor "either of those creatures" → `ParentTarget` (the declared
//!     attackers, seeded by `seed_batched_attack_parent_targets`).
//!   - The "it" antecedent in "put a +1/+1 counter on it" → `TriggeringSource`
//!     (the creature that dealt combat damage), NOT `SelfRef` (the enchantment).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    Comparator, DelayedTriggerCondition, Effect, TargetFilter, TriggerCondition,
};
use engine::types::counter::CounterType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;

use super::rules::{run_combat, AttackTarget, GameAction, WaitingFor};

const P2: PlayerId = PlayerId(2);

const LOVE_ORACLE: &str = "Whenever you attack with exactly two creatures, those \
    creatures gain first strike until end of turn, then draw a card. Whenever \
    either of those creatures deals combat damage to a player this combat, put a \
    +1/+1 counter on it.";

fn counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

fn stock_library(scenario: &mut GameScenario) {
    for name in ["Lib 1", "Lib 2", "Lib 3", "Lib 4"] {
        scenario.add_card_to_library_top(P0, name);
    }
}

/// End-to-end (A2): two attackers both deal combat damage to a player → EACH
/// gains exactly one +1/+1 counter ON ITSELF, the first-strike grant applied
/// (reach-guard proving the batched-attack seeding worked), and the controller
/// drew a card.
#[test]
fn two_attackers_each_get_one_counter_on_themselves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Love on the Battlefield", LOVE_ORACLE);
    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    stock_library(&mut scenario);
    let mut runner = scenario.build();

    let hand_before = hand_count(&runner, P0);
    let life_before = runner.life(P1);

    run_combat(&mut runner, vec![bear_a, bear_b], vec![]);
    runner.advance_until_stack_empty();

    // Reach-guard: the first-strike grant landed on the attackers. Without the
    // batched-attack `ParentTarget` seeding (`effect_uses_parent_target` +
    // `seed_batched_attack_parent_targets`) this grant would silently drop.
    assert!(
        runner.state().objects[&bear_a].has_keyword(&Keyword::FirstStrike),
        "Bear A must have gained first strike (proves attacker seeding)"
    );

    // CR 121.1: the "then draw a card" rider drew for the controller.
    assert_eq!(
        hand_count(&runner, P0),
        hand_before + 1,
        "controller drew a card from the first ability"
    );

    // CR 120.2a: both 2/2s dealt combat damage to P1.
    assert_eq!(
        runner.life(P1),
        life_before - 4,
        "both attackers hit P1 for 2"
    );

    // The discriminating assertion: each attacker got exactly ONE +1/+1 counter,
    // on ITSELF. If "it" bound to `SelfRef` the counters would land on the
    // enchantment (neither bear); if the source bound to `Any` a wrong count
    // could appear; if `TriggeringSource` didn't re-resolve per firing both
    // counters would land on one creature.
    assert_eq!(
        counters(&runner, bear_a),
        1,
        "Bear A gets exactly one counter"
    );
    assert_eq!(
        counters(&runner, bear_b),
        1,
        "Bear B gets exactly one counter"
    );

    // The enchantment itself must NOT receive a counter ("it" is the creature,
    // not the source permanent).
    let love = runner
        .state()
        .objects
        .values()
        .find(|o| o.name == "Love on the Battlefield")
        .expect("Love present")
        .id;
    assert_eq!(counters(&runner, love), 0, "enchantment gets no counter");
}

/// Multiplayer discrimination (CR 603.2c + CR 510.2): the two attackers attack
/// DIFFERENT opponents, so one simultaneous combat-damage step emits TWO aggregate
/// `CombatDamageDealtToPlayer` events at once — one per defending player. Each
/// attacker must still get exactly one +1/+1 counter on itself. Before the
/// multi-defender expansion fix, the delayed rider only expanded the FIRST
/// aggregate (`.find()`), so the creature that hit the second defender silently
/// got no counter — this test's `counters(bear_b) == 1` assertion flips (fails)
/// when the fix is reverted.
#[test]
fn attackers_against_two_different_opponents_each_get_a_counter() {
    let mut scenario = GameScenario::new_n_player(3, 71);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Love on the Battlefield", LOVE_ORACLE);
    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    stock_library(&mut scenario);
    let mut runner = scenario.build();

    let p1_life_before = runner.life(P1);
    let p2_life_before = runner.life(P2);

    // bear_a attacks P1, bear_b attacks P2 — two defending players in one attack.
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (bear_a, AttackTarget::Player(P1)),
            (bear_b, AttackTarget::Player(P2)),
        ])
        .expect("declare two attackers against different opponents");

    // Drive the YouAttack trigger, the (empty) declare-blockers step, and the
    // combat-damage step. P1/P2 control no creatures, so there are no blockers and
    // no interactive damage assignment. A bounded priority loop (multiplayer-safe,
    // unlike `run_combat`'s 2-player `pass_both_players`) carries the turn through
    // combat; the harness may batch the combat-damage delayed-trigger firing with
    // the phase advance to the postcombat main phase, so `advance_until_stack_empty`
    // afterward resolves the two rider triggers the combat-damage step placed on
    // the stack (each puts a +1/+1 counter on its damaging creature).
    for _ in 0..80 {
        if matches!(
            runner.state().phase,
            Phase::PostCombatMain | Phase::End | Phase::Cleanup
        ) {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order: Vec<usize> = (0..triggers.len()).collect();
                let _ = runner.act(GameAction::OrderTriggers { order });
            }
            WaitingFor::DeclareBlockers { .. } => {
                let _ = runner.act(GameAction::DeclareBlockers {
                    assignments: vec![],
                });
            }
            _ => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
        }
    }
    runner.advance_until_stack_empty();

    // Reach-guard: both defenders took 2 combat damage — both aggregate
    // `CombatDamageDealtToPlayer` events genuinely fired, so the counters below
    // cannot be attributed to a no-combat path.
    assert_eq!(
        runner.life(P1),
        p1_life_before - 2,
        "bear_a dealt 2 combat damage to P1"
    );
    assert_eq!(
        runner.life(P2),
        p2_life_before - 2,
        "bear_b dealt 2 combat damage to P2"
    );

    // The discriminating assertions: EACH attacker gets exactly one +1/+1 counter,
    // even though they hit different defenders (separate aggregate events).
    assert_eq!(
        counters(&runner, bear_a),
        1,
        "attacker vs P1 gets its counter"
    );
    assert_eq!(
        counters(&runner, bear_b),
        1,
        "attacker vs P2 gets its counter (reverting the multi-defender fix drops this)"
    );
}

/// Negative reach-guard: a third creature that does NOT attack receives no
/// counter (the rider fires only for members of the declared-attacker set that
/// actually dealt combat damage).
#[test]
fn non_attacking_creature_gets_no_counter() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Love on the Battlefield", LOVE_ORACLE);
    let bear_a = scenario.add_creature(P0, "Bear A", 2, 2).id();
    let bear_b = scenario.add_creature(P0, "Bear B", 2, 2).id();
    let bench = scenario.add_creature(P0, "Bench Bear", 2, 2).id();
    stock_library(&mut scenario);
    let mut runner = scenario.build();

    run_combat(&mut runner, vec![bear_a, bear_b], vec![]);
    runner.advance_until_stack_empty();

    assert_eq!(counters(&runner, bench), 0, "non-attacker gets no counter");
    assert_eq!(
        counters(&runner, bear_a),
        1,
        "attacker A still gets its counter"
    );
}

/// Gap B discrimination: attacking with THREE creatures does not satisfy the
/// "exactly two" (`Comparator::EQ`) constraint, so the whole ability does not
/// fire — no first strike, no draw, no counters. If the constraint were dropped
/// (`constraint: None`, the pre-fix behavior) or read as GE, this would fire.
#[test]
fn three_attackers_do_not_satisfy_exactly_two() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Love on the Battlefield", LOVE_ORACLE);
    let a = scenario.add_creature(P0, "A", 2, 2).id();
    let b = scenario.add_creature(P0, "B", 2, 2).id();
    let c = scenario.add_creature(P0, "C", 2, 2).id();
    stock_library(&mut scenario);
    let mut runner = scenario.build();

    let hand_before = hand_count(&runner, P0);
    let life_before = runner.life(P1);
    run_combat(&mut runner, vec![a, b, c], vec![]);
    runner.advance_until_stack_empty();

    // CR 120.2a (positive reach-guard): the three 2/2s actually attacked and dealt
    // combat damage to P1 (6 total). This proves the attack RESOLVED and the
    // `Comparator::EQ`/`count: 2` constraint was genuinely exercised on a real
    // combat — the no-draw/no-counter/no-first-strike assertions below are the
    // constraint declining, not a combat that never happened.
    assert_eq!(
        runner.life(P1),
        life_before - 6,
        "all three attackers hit P1 for 2 (attack resolved)"
    );

    assert_eq!(
        hand_count(&runner, P0),
        hand_before,
        "exactly-two constraint not met by 3 attackers → no draw"
    );
    assert_eq!(counters(&runner, a), 0, "no counter — ability did not fire");
    assert!(
        !runner.state().objects[&a].has_keyword(&Keyword::FirstStrike),
        "no first strike — ability did not fire on a 3-creature attack"
    );
}

/// Parse-shape pins (Gap A + Gap B): the outer trigger enforces
/// `AttackersDeclaredCount { EQ, 2 }`, and the folded delayed trigger is
/// `DamageDone`/`CombatOnly` with `valid_source: ParentTarget`, `valid_target:
/// Player`, and an inner `PutCounter` on `TriggeringSource`. Complements the
/// runtime tests: a revert of Gap A returns `mode: Unknown`, and a revert of the
/// "it" fix returns `SelfRef`.
#[test]
fn parse_shape_exactly_two_and_delayed_damage_rider() {
    let parsed = parse_oracle_text(
        LOVE_ORACLE,
        "Love on the Battlefield",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::YouAttack)
        .expect("YouAttack trigger");

    // Gap B: the exactly-two count constraint.
    match trigger
        .condition
        .as_ref()
        .expect("count constraint present")
    {
        TriggerCondition::AttackersDeclaredCount {
            comparator, count, ..
        } => {
            assert_eq!(*comparator, Comparator::EQ, "exactly → EQ");
            assert_eq!(*count, 2, "two");
        }
        other => panic!("expected AttackersDeclaredCount, got {other:?}"),
    }

    // Walk the effect chain to the folded CreateDelayedTrigger.
    let execute = trigger.execute.as_ref().expect("execute present");
    let mut ability = execute.as_ref();
    let delayed = loop {
        if let Effect::CreateDelayedTrigger {
            condition, effect, ..
        } = &*ability.effect
        {
            break (condition, effect);
        }
        ability = ability
            .sub_ability
            .as_deref()
            .expect("CreateDelayedTrigger must appear in the effect chain");
    };
    let (condition, inner) = delayed;
    let DelayedTriggerCondition::WheneverEvent {
        trigger: inner_trigger,
        ..
    } = condition
    else {
        panic!("delayed condition must be WheneverEvent, got {condition:?}");
    };
    assert_eq!(
        inner_trigger.mode,
        TriggerMode::DamageDone,
        "Gap A: rider is DamageDone, not Unknown"
    );
    assert_eq!(
        inner_trigger.valid_source,
        Some(TargetFilter::ParentTarget),
        "source anaphor 'either of those creatures' → ParentTarget"
    );
    assert_eq!(
        inner_trigger.valid_target,
        Some(TargetFilter::Player),
        "recipient is a player"
    );

    // Inner "it" → TriggeringSource (the damaging creature).
    match &*inner.effect {
        Effect::PutCounter { target, .. } => assert_eq!(
            *target,
            TargetFilter::TriggeringSource,
            "'it' must be TriggeringSource (the creature that dealt damage)"
        ),
        other => panic!("expected PutCounter, got {other:?}"),
    }
}

/// Scope-evidence class guard (PR #6884, signatures 3 & 4): the "attack with
/// exactly two creatures" recognition is a CLASS fix, not a one-off for Love on
/// the Battlefield. Alluring Suitor is the sibling card whose YouAttack trigger
/// gained the same `AttackersDeclaredCount { EQ, 2 }` condition and `you` target
/// scope. Before the fix the constraint was dropped, so the transform over-fired
/// on any attack; pinning it here proves the class improvement and guards the
/// sibling from regressing independently of Love's runtime tests.
#[test]
fn parse_shape_alluring_suitor_exactly_two_attack_constraint() {
    // Verified Oracle text (Scryfall, front face): "When you attack with exactly
    // two creatures, transform this creature."
    let parsed = parse_oracle_text(
        "When you attack with exactly two creatures, transform this creature.",
        "Alluring Suitor",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find(|t| t.mode == TriggerMode::YouAttack)
        .expect("YouAttack trigger");

    match trigger
        .condition
        .as_ref()
        .expect("exactly-two count constraint present (not dropped → no over-fire)")
    {
        TriggerCondition::AttackersDeclaredCount {
            comparator, count, ..
        } => {
            assert_eq!(*comparator, Comparator::EQ, "exactly → EQ");
            assert_eq!(*count, 2, "two");
        }
        other => panic!("expected AttackersDeclaredCount, got {other:?}"),
    }
}
