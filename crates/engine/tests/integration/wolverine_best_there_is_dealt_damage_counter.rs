//! Wolverine, Best There Is — end-step self-growth trigger.
//!
//! Oracle (verbatim, card-data.json key "wolverine, best there is"):
//!
//! ```text
//! Unrivaled Lethality — Double all damage Wolverine would deal.
//! At the beginning of each end step, if Wolverine dealt damage to another
//! creature this turn, put a +1/+1 counter on him.
//! {1}{G}: Regenerate Wolverine. ...
//! ```
//!
//! The regression these tests guard: the intervening-if "if Wolverine dealt
//! damage to another creature this turn" (dealing-direction, *creature* target)
//! used to be swallowed to `condition: None`, so the +1/+1 counter was placed on
//! EVERY end step regardless of whether Wolverine actually hit a creature. The
//! parser fix (`parse_source_dealt_damage_this_turn`) recognises the creature
//! target; these tests prove the RUNTIME evaluates
//! `QuantityRef::DamageDealtThisTurn { source: SelfRef, target: creature(Another) }`
//! end-to-end through the real trigger pipeline (CR 603.4 intervening-if
//! recheck + CR 120.1 damage history).
//!
//! Both tests build Wolverine identically from the FULL verbatim Oracle text via
//! `add_creature_from_oracle`, so the difference in outcome (+1/+1 vs none) can
//! only be the intervening-if gate, never a parse difference.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// Wolverine, Best There Is — full verbatim Oracle text (card-data.json key
/// "wolverine, best there is"). Built from the real card so the parsed trigger
/// (and its intervening-if) is exercised exactly as production sees it.
const WOLVERINE_ORACLE: &str = "Unrivaled Lethality — Double all damage Wolverine would deal.\n\
     At the beginning of each end step, if Wolverine dealt damage to another creature \
     this turn, put a +1/+1 counter on him.\n\
     {1}{G}: Regenerate Wolverine.";

/// Count of +1/+1 counters on an object (CR 122.1), `0` if absent.
fn plus1_counters(runner: &GameRunner, obj: engine::types::ObjectId) -> u32 {
    runner.state().objects[&obj]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// Effective power/toughness of a permanent (CR 208 / CR 209) — read off the
/// post-layer object fields, mirroring `GameRunner::power_toughness`.
fn power_toughness(runner: &GameRunner, obj: engine::types::ObjectId) -> (i32, i32) {
    let o = &runner.state().objects[&obj];
    (o.power.unwrap_or(0), o.toughness.unwrap_or(0))
}

/// Advance to the declare-blockers prompt, passing any priority window opened
/// after attackers are declared (CR 508.2). Panics if the prompt never arrives
/// — silently falling through would let the attacker go unblocked and deal no
/// damage to the blocker, so the +1/+1 assertion would measure nothing.
fn advance_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..32 {
        match runner.state().waiting_for {
            WaitingFor::DeclareBlockers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority before blockers must succeed");
            }
            ref other => panic!("expected DeclareBlockers or Priority, got {other:?}"),
        }
    }
    panic!("never reached the DeclareBlockers prompt");
}

/// Drive from the current mid-turn state to the End step, then resolve the
/// beginning-of-end-step trigger. The intervening-if is checked when the trigger
/// would go on the stack (CR 603.4), so `advance_until_stack_empty` resolves it
/// (placing the counter) only when the condition holds.
fn resolve_end_step_trigger(runner: &mut GameRunner) {
    runner.advance_to_end_step();
    assert_eq!(
        runner.state().phase,
        Phase::End,
        "must reach the End step so the beginning-of-end-step trigger can fire"
    );
    runner.advance_until_stack_empty();
}

/// POSITIVE: Wolverine deals combat damage to another creature this turn, so at
/// the end step the intervening-if holds and Wolverine gains exactly one +1/+1
/// counter. Revert-guard: if the runtime stopped evaluating
/// `DamageDealtThisTurn(SelfRef -> creature(Another))` (the pre-fix `None`
/// condition), the counter would either never appear (condition dropped) — this
/// assertion flips.
///
/// CR 120.1 + CR 603.4 + CR 122.1.
#[test]
fn wolverine_gains_counter_after_dealing_damage_to_a_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // 2/4 so that even with "Double all damage Wolverine would deal" the blocker
    // takes at most 4 — below the blocker's 6 toughness, so the blocker SURVIVES
    // and stays a battlefield creature at end-step resolution (the damage-history
    // target match reads the live object).
    let wolverine = scenario
        .add_creature_from_oracle(P0, "Wolverine, Best There Is", 2, 4, WOLVERINE_ORACLE)
        .id();

    // 1/6 blocker: survives Wolverine's (doubled) damage and deals only 1 back,
    // so Wolverine (4 toughness) also survives. No death/SBA noise.
    let blocker = scenario.add_creature(P1, "Stone Wall", 1, 6).id();

    let mut runner = scenario.build();

    // Precondition: no +1/+1 counters before combat.
    assert_eq!(
        plus1_counters(&runner, wolverine),
        0,
        "Wolverine starts with no +1/+1 counters"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(wolverine, AttackTarget::Player(P1))])
        .expect("P0 attacks with Wolverine");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, wolverine)])
        .expect("P1 blocks Wolverine with the 1/6 wall");
    runner.combat_damage();

    // Reach-guard: Wolverine actually dealt damage to the blocker CREATURE this
    // turn (so the end-step condition has something to be true about). Both
    // creatures survived, so the record's target is a live battlefield creature.
    assert!(
        runner.state().objects[&blocker].damage_marked > 0,
        "Wolverine must have dealt marked damage to the blocker creature this turn"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "the blocker survives and stays a creature on the battlefield"
    );

    resolve_end_step_trigger(&mut runner);

    // The intervening-if held: exactly one +1/+1 counter.
    assert_eq!(
        plus1_counters(&runner, wolverine),
        1,
        "Wolverine gains exactly one +1/+1 counter after dealing damage to another creature"
    );
    // The counter is reflected in Wolverine's effective P/T (2/4 -> 3/5).
    assert_eq!(
        power_toughness(&runner, wolverine),
        (3, 5),
        "the +1/+1 counter raises Wolverine to 3/5"
    );
}

/// NEGATIVE: same board and same Wolverine, but this turn Wolverine deals damage
/// only to a PLAYER (an unblocked attack), never to a creature. The
/// intervening-if is false, so NO +1/+1 counter is placed.
///
/// Non-vacuous: the reach-guard asserts P1 lost life — Wolverine DID deal damage
/// this turn, so a bug that read the condition as "dealt ANY damage this turn"
/// would place a counter here. It does not, proving the gate specifically
/// requires a CREATURE target. A bystander creature is present (so "another
/// creature Wolverine could have hit" exists on the board), isolating the
/// player-vs-creature target axis. The positive sibling proves this exact
/// trigger DOES place a counter when the target is a creature.
///
/// CR 120.1 + CR 603.4.
#[test]
fn wolverine_gains_no_counter_when_only_a_player_was_damaged() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let wolverine = scenario
        .add_creature_from_oracle(P0, "Wolverine, Best There Is", 2, 4, WOLVERINE_ORACLE)
        .id();

    // A bystander creature that does NOT block — "another creature" is present on
    // the board, but Wolverine never deals damage to it.
    let bystander = scenario.add_creature(P1, "Grizzly Bear", 2, 2).id();

    let mut runner = scenario.build();
    let p1_life_before = runner.life(P1);

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(wolverine, AttackTarget::Player(P1))])
        .expect("P0 attacks P1 directly with Wolverine");
    advance_to_declare_blockers(&mut runner);
    // No blocks: Wolverine hits the PLAYER, not a creature.
    runner
        .declare_blockers(&[])
        .expect("P1 declares no blockers");
    runner.combat_damage();

    // Reach-guard: Wolverine DID deal damage this turn — to the player. This is
    // what makes the negative non-vacuous: the condition is not false because
    // "no damage happened", it is false because the damage was not to a creature.
    assert!(
        runner.life(P1) < p1_life_before,
        "reach-guard: Wolverine must have dealt damage to P1 this turn (life dropped)"
    );
    // The bystander creature is untouched and still a battlefield creature — a
    // legal creature target existed, Wolverine simply never damaged it.
    assert_eq!(
        runner.state().objects[&bystander].damage_marked,
        0,
        "the bystander creature took no damage"
    );
    assert_eq!(runner.state().objects[&bystander].zone, Zone::Battlefield);

    resolve_end_step_trigger(&mut runner);

    // The intervening-if failed: NO +1/+1 counter, and Wolverine's P/T is still
    // its printed 2/4.
    assert_eq!(
        plus1_counters(&runner, wolverine),
        0,
        "Wolverine gains NO +1/+1 counter when it only dealt damage to a player, not a creature"
    );
    assert_eq!(
        power_toughness(&runner, wolverine),
        (2, 4),
        "Wolverine remains its printed 2/4 — the creature-target gate suppressed the counter"
    );
}
