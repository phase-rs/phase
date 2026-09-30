//! Conformer Shuriken — runtime tests for the granted attack trigger.
//!
//! Oracle (Scryfall, verified 2026-09-29), Legendary Artifact — Equipment {2}:
//!   Equipped creature has "Whenever this creature attacks, tap target creature
//!   defending player controls. If that creature has greater power than this
//!   creature, put a number of +1/+1 counters on this creature equal to the
//!   difference."
//!   Equip {2}
//!
//! Every test drives the real declare-attackers → trigger → stack pipeline. The
//! reach guards are that the trigger's stack entry targets the expected
//! creature, and that resolution actually ran (the target got tapped, or the
//! stack drained after a response). "No counters" is never asserted without
//! one of them.
//!
//! Response spells are the printed cards' Oracle text, cast through
//! `GameRunner::cast` at P0's priority while the trigger waits on the stack.
//! Their mana costs are left unset: the tests are about stack timing, not payment.

use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const SHURIKEN: &str = "Equipped creature has \"Whenever this creature attacks, tap target creature defending player controls. If that creature has greater power than this creature, put a number of +1/+1 counters on this creature equal to the difference.\"\nEquip {2}";

const LESS_TOUGHNESS_SHURIKEN: &str = "Equipped creature has \"Whenever this creature attacks, tap target creature defending player controls. If that creature has less toughness than this creature, put a number of +1/+1 counters on this creature equal to the difference.\"\nEquip {2}";

const GIANT_GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const DISENCHANT: &str = "Destroy target artifact or enchantment.";
const UNSUMMON: &str = "Return target creature to its owner's hand.";

struct Board {
    runner: GameRunner,
    attacker: ObjectId,
    shuriken: ObjectId,
    defender: ObjectId,
    response: Option<ObjectId>,
}

/// P0's `attacker` (power/toughness given) is equipped with a Shuriken built from
/// `oracle`; P1 controls `defender`. `response` optionally puts one instant in
/// P0's hand.
fn board(
    oracle: &str,
    attacker_pt: (i32, i32),
    defender_pt: (i32, i32),
    response: Option<(&str, &str)>,
) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Equipped Attacker", attacker_pt.0, attacker_pt.1)
        .id();
    let shuriken = scenario
        .add_artifact_from_oracle(P0, "Conformer Shuriken", oracle)
        .with_subtypes(vec!["Equipment"])
        .id();
    let defender = scenario
        .add_creature(P1, "Defending Creature", defender_pt.0, defender_pt.1)
        .id();
    let response = response.map(|(name, text)| {
        scenario
            .add_spell_to_hand_from_oracle(P0, name, true, text)
            .id()
    });
    let mut runner = scenario.build();
    // CR 301.5a: the Shuriken equips the attacker (what Equip's resolution does).
    let state = runner.state_mut();
    state.objects.get_mut(&shuriken).unwrap().attached_to = Some(AttachTarget::Object(attacker));
    state
        .objects
        .get_mut(&attacker)
        .unwrap()
        .attachments
        .push(shuriken);
    Board {
        runner,
        attacker,
        shuriken,
        defender,
        response,
    }
}

/// CR 508.1m: declare the attack, answer the granted trigger's target prompt
/// (if any) with `target`, and return with the trigger on the stack.
/// REACH GUARD: the trigger's stack entry targets `target`.
fn attack_and_target(b: &mut Board, target: ObjectId) {
    b.runner.advance_to_combat();
    b.runner
        .declare_attackers(&[(b.attacker, AttackTarget::Player(P1))])
        .expect("the equipped creature attacks");
    for _ in 0..8 {
        match b.runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { triggers, .. } => {
                let order = (0..triggers.len()).collect();
                b.runner
                    .act(GameAction::OrderTriggers { order })
                    .expect("order triggers");
            }
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                assert_eq!(
                    target_slots.len(),
                    1,
                    "CR 115.1: only the tap names a target; the gated clause reads it: {target_slots:?}"
                );
                b.runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("choose the tap target");
            }
            _ => break,
        }
    }
    let entry = b
        .runner
        .state()
        .stack
        .back()
        .expect("reach guard: the granted attack trigger is on the stack");
    let ability = entry.ability().expect("a triggered ability");
    assert_eq!(
        ability.targets,
        vec![TargetRef::Object(target)],
        "reach guard: the trigger targets the chosen creature"
    );
    assert_eq!(
        entry.source_id, b.attacker,
        "CR 113.7: the granted trigger's source is the equipped creature"
    );
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// CR 208.1 + CR 608.2c: a 2/2 attacker taps a 5/5 and gets 5 − 2 = 3 counters;
/// the Equipment gets none (CR 113.7: "this creature" is the trigger's source).
#[test]
fn bigger_target_puts_the_difference_on_the_equipped_creature() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&b.defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 3);
    assert_eq!(p1p1(&b.runner, b.shuriken), 0);
}

/// "Greater" is strict: equal power puts no counters.
#[test]
fn equal_power_puts_no_counters() {
    let mut b = board(SHURIKEN, (3, 3), (3, 3), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&b.defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 0);
}

/// A smaller target puts no counters (and no negative-difference artifact).
#[test]
fn smaller_target_puts_no_counters() {
    let mut b = board(SHURIKEN, (4, 4), (2, 2), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&b.defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 0);
}

/// The gate reads power, not whether the tap changed anything: an already
/// tapped 5/5 is still a legal target and still yields 3.
#[test]
fn already_tapped_target_still_counts() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), None);
    b.runner
        .state_mut()
        .objects
        .get_mut(&b.defender)
        .unwrap()
        .tapped = true;
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().stack.is_empty(),
        "reach guard: the trigger resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 3);
}

/// CR 608.2b: printed Unsummon returns the only target in response, so the
/// trigger doesn't resolve at all: no counters.
#[test]
fn target_leaving_before_resolution_does_nothing() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), Some(("Unsummon", UNSUMMON)));
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    let unsummon = b.response.unwrap();
    b.runner.cast(unsummon).target_object(defender).resolve();
    assert_eq!(
        b.runner.state().objects[&defender].zone,
        Zone::Hand,
        "reach guard: Unsummon resolved"
    );
    assert!(
        b.runner.state().stack.is_empty(),
        "reach guard: stack drained"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 0);
}

/// CR 608.2h: printed Giant Growth on the 5/5 in response makes it 8/8 at
/// resolution; the 2/2 attacker gets 8 − 2 = 6.
#[test]
fn giant_growth_on_the_target_in_response_raises_the_difference() {
    let mut b = board(
        SHURIKEN,
        (2, 2),
        (5, 5),
        Some(("Giant Growth", GIANT_GROWTH)),
    );
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    let growth = b.response.unwrap();
    b.runner.cast(growth).target_object(defender).resolve();
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 6);
}

/// CR 608.2h: printed Giant Growth on the 2/2 attacker (→ 5/5) against a 4/4
/// removes the gap: no counters.
#[test]
fn giant_growth_on_the_attacker_in_response_removes_the_counters() {
    let mut b = board(
        SHURIKEN,
        (2, 2),
        (4, 4),
        Some(("Giant Growth", GIANT_GROWTH)),
    );
    let (attacker, defender) = (b.attacker, b.defender);
    attack_and_target(&mut b, defender);
    let growth = b.response.unwrap();
    b.runner.cast(growth).target_object(attacker).resolve();
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(
        b.runner.state().objects[&attacker].power,
        Some(5),
        "reach guard: Giant Growth resolved on the attacker"
    );
    assert_eq!(p1p1(&b.runner, attacker), 0);
}

/// CR 113.7a: printed Disenchant destroys the Shuriken with the trigger on the
/// stack; the ability exists independently of it, so the attacker still gets 3.
#[test]
fn destroying_the_shuriken_in_response_does_not_stop_the_trigger() {
    let mut b = board(SHURIKEN, (2, 2), (5, 5), Some(("Disenchant", DISENCHANT)));
    let (attacker, defender, shuriken) = (b.attacker, b.defender, b.shuriken);
    attack_and_target(&mut b, defender);
    let disenchant = b.response.unwrap();
    b.runner.cast(disenchant).target_object(shuriken).resolve();
    assert_eq!(
        b.runner.state().objects[&shuriken].zone,
        Zone::Graveyard,
        "reach guard: Disenchant resolved"
    );
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, attacker), 3);
    assert_eq!(p1p1(&b.runner, shuriken), 0);
}

/// Accepted-shape runtime control (`less toughness`): a 2/5 attacker against a
/// 3/2 target gets 5 − 2 = 3; with the target at toughness 5 (equal), none.
#[test]
fn less_toughness_variant_runs_end_to_end() {
    let mut b = board(LESS_TOUGHNESS_SHURIKEN, (2, 5), (3, 2), None);
    let defender = b.defender;
    attack_and_target(&mut b, defender);
    b.runner.advance_until_stack_empty();
    assert!(
        b.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&b.runner, b.attacker), 3);

    let mut control = board(LESS_TOUGHNESS_SHURIKEN, (2, 5), (3, 5), None);
    let defender = control.defender;
    attack_and_target(&mut control, defender);
    control.runner.advance_until_stack_empty();
    assert!(
        control.runner.state().objects[&defender].tapped,
        "reach guard: tap resolved"
    );
    assert_eq!(p1p1(&control.runner, control.attacker), 0);
}
